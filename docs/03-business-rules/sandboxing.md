---
id: rule.sandboxing
type: rule
title: Sandboxing & Shell Safety Rules
---

**The agent can touch the workspace. It cannot touch the rest of the machine — and the shell is explicitly not sandboxed.**

## The file sandbox — one choke point

- **`ToolContext::resolve(path)` is the single place** that canonicalizes a model-supplied path and checks it stays inside `workspace_root` — added because V1 had that check copy-pasted into each file tool separately, "easy to get subtly wrong in one of them".
- Algorithm: join against the root → walk `ancestors()` to the first existing prefix (the path need not exist yet — `write_file` creates files) → canonicalize that prefix → require `starts_with(workspace_root)` → re-attach the nonexistent suffix.
- Rejects: `../../etc/passwd` (relative escape), absolute paths outside the root, **symlinks pointing out** (canonicalization resolves them). Errors: `"{path:?} is not inside the workspace"`, `"could not resolve {path:?}: {error}"`, `"{path:?} is outside the workspace"`.
- Every path-taking built-in tool goes through it. `glob` roots patterns at the workspace directly. `display_path` strips the root prefix from tool output so the host's directory layout never leaks.
- **The doc vault has its own independent sandbox**: `ObsidianDriver`'s private `resolve()` re-implements the same check against the vault path, because a vault may legitimately live *outside* the turn's workspace.
- **Sub-agent scope**: a child session's `workspace_root` is its task's `path` (must be an existing directory) — its file sandbox is structurally smaller than the parent's.

## The shell — a denylist, not a sandbox

`run_command` and `start_process` are **denylist-checked first** (`blocklist::check`), before spawn. The docs are explicit: *"not a sandbox. There is no seccomp, no container, no chroot."*

- Split statements on `\n`, `;`, `&&`, `||`, `|` — **not quote-aware, a known accepted limitation**.
- Per statement, first token (path-stripped):
  - Unconditional block: `su`, `sudo`.
  - `BLOCKED_COMMANDS`: `dd, mkfs, format, shutdown, reboot, poweroff, halt, kill, pkill, chown, passwd, wget, rmdir, del`.
  - Pairs: `rm -rf`, `rm -fr`, `rm --recursive`, `chmod 0`, `chmod 777`.
  - Patterns: `^rm -[rfR]`, `^chmod 0|777`, `^curl -o /`, `^mv … /dev/null`, fork bombs (`^:()\s*{`), writes to raw block devices (`/dev/sd*`, `/dev/nvme*`, `/dev/disk*`).
- **Quoted/obfuscated commands bypass it by design** — "It stops the obvious, unintentional mistake, not a deliberate attempt to get around it."
- Consequences the denylist intentionally produces: the model can never `kill`/`pkill` arbitrary processes via the shell (the app's own process killing goes through `ProcessRegistry`'s internal `libc::kill`), and `wget` is blocked (curl-to-file is pattern-blocked) — network fetches must go through a tool.

## Process hygiene (not a sandbox, but containment)

- Every spawned process runs in its **own process group** (`process_group(0)`) so `kill_tree` (`libc::kill(-pid, SIGKILL)`) takes the whole tree down — `stop_process`, `kill_all`, and app shutdown all go through it. No orphaned grandchildren (`npm run dev` → `node`).
- `kill_on_drop(true)` as a second net.
- **Closing the window kills every tracked process** — nothing on the OS does that for a dead parent's children; `Engine::shutdown()` runs `kill_all()` (2 s grace) before the engine acks.
- Logs are capped at 200,000 bytes per process (drained from the front); `run_command` output at 100,000 bytes; timeouts 30 s default / 120 s max.

## What is *not* sandboxed

- **MCP servers** run as arbitrary local processes with the environment the user configured (`env` map) — their own filesystem access is outside this workspace's sandbox and not tracked (`McpToolAdapter` never calls `record_change`).
- **`run_command`'s working directory** is the workspace root, but the command itself can read anything the user account can (subject only to the denylist).
- **Network** from `run_command` is unrestricted except the `curl -o /` and `wget` blocks.