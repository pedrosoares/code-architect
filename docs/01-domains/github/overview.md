---
id: domain.github
type: domain
title: GitHub Domain
relations:
  related_integrations:
  - integration.github
  related_flows:
  - flow.oauth-login
---

The GitHub domain is the read-only window into the code host: the agent can inspect issues, pull requests (including the full review conversation and per-file diff), commits, and repository file contents without any write path. All access goes through the GitHub REST API, one crate (`architect-github`), eight tools — see the [GitHub Integration](../../04-integrations/github.md) for the per-endpoint details.

## Scope

- **Issues and PRs**: issue body + comments; PR metadata (title, author, state, base/head, diff stats), the combined PR comment/review conversation, the per-file `.diff`, and the commit list.
- **Repository contents**: read a file (any ref, default branch) or list a directory.
- **No writes**: there is deliberately no create/update/close/comment tool.

## Credential model

Token resolution is the desktop engine's job (saved `github_token`, or `gh auth token` when the gh-CLI toggle is on). The crate itself only takes a `String` token and builds a client; without a resolvable token the tools simply aren't registered.

## Design notes

- **PR comments are one merged, chronological list** (inline review comments + conversation comments + review summaries) — the same view a human sees on GitHub, so the model doesn't have to correlate three endpoints.
- **`github_read_file` and `github_list_directory` share one request** to `contents/{path}`; the response shape (object vs. array) selects the format, and a mismatched call gets an error pointing at the other tool.
- **Graceful degradation over hard failure**: symlinks, submodules, binary files, and >1MB files are *reported* rather than erroring; missing JSON fields become `?`/`unknown`; a non-2xx becomes a tool error with the status and body excerpt.

## See also

- [GitHub Integration](../../04-integrations/github.md)
- [OAuth Login Flow](../../02-flows/oauth-login.md)