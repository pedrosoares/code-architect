---
id: integration.github
type: integration
title: GitHub Integration
depends_on:
- domain.github
relations:
  related_domains:
  - domain.github
  related_flows:
  - flow.oauth-login
---

GitHub integration: read-only access to issues, pull requests, and repository file contents via the GitHub REST API (`reqwest`, base `https://api.github.com`). One crate — `architect-github` — fills the `Tool` extension point with **eight** tools; nothing here writes to GitHub.

## Authentication

The token is resolved in the desktop engine (see [Desktop App Domain](../desktop-app/overview.md)), in this order:
1. The saved `github_token` from `IntegrationsConfig` (set via Settings → Integrations, either typed in or via the **Login** OAuth flow — `scope=repo`).
2. Otherwise, when `github_use_gh_cli` is on, the output of the `gh auth token` subprocess. Failure produces a global `Failed` event with the hints "is the GitHub CLI installed and on PATH?" / "run `gh auth login` first".

No token resolvable → the eight tools are not registered at all.

## Tools

| Tool | Endpoint | Notes |
|---|---|---|
| `github_read_issue` | `GET /repos/{o}/{r}/issues/{n}` | A PR number hits the same endpoint: the output appends a hint that the number is actually a pull request |
| `github_read_issue_comments` | `GET /repos/{o}/{r}/issues/{n}/comments` | All pages, chronological, "No comments" when empty |
| `github_read_pull_request` | `GET /repos/{o}/{r}/pulls/{n}` | Title, author, state, base/head, diff stats |
| `github_read_pull_request_comments` | `GET /repos/{o}/{r}/pulls/{n}/comments` + `/issues/{n}/comments` + review comments + review summaries | Merged into one chronological list, as GitHub's own PR view shows it |
| `github_read_pull_request_diff` | `GET /repos/{o}/{r}/pulls/{n}.diff` | Per-file status + unified patch; GitHub omits very large / binary files |
| `github_read_pull_request_commits` | `GET /repos/{o}/{r}/pulls/{n}/commits` | SHA, author, date, full message |
| `github_read_file` | `GET /repos/{o}/{r}/contents/{path}` | Ref param (branch/tag/SHA) defaults to the default branch; base64-decoded (GitHub's ~60-char newlines stripped first); reports symlinks, submodules, binary files, and >1MB files instead of failing opaquely |
| `github_list_directory` | same endpoint | Shares the request with `github_read_file`; the response shape (object vs array) decides which of the two formats to run — each tool errors with a pointer to the other when the shape is wrong |

## Error handling

All client errors surface as tool errors (never panics): non-2xx responses report the status and body excerpt, and missing fields in the JSON degrade to `?`/`unknown` rather than failing.

## See also

- [GitHub Domain](../github/overview.md)
- [OAuth Login Flow](../../02-flows/oauth-login.md) — how the `github_token` gets there
- [MCP Integration](mcp.md) — the other way of adding tools