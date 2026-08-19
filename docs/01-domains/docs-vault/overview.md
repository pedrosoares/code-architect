---
id: domain.docs-vault
type: domain
title: Documentation Knowledge Base Domain
depends_on:
- domain.tools
relations:
  related_flows:
  - flow.send-message
---

The documentation domain (`architect-docs`) is a pluggable documentation knowledge base for the coding agent: markdown docs carrying YAML frontmatter (stable `id`, `type`, `depends_on`/`relations` links) so the agent can traverse domain → flow → rule → entity relationships without re-deriving them from source every turn.

## The data model

Every doc is a markdown file with frontmatter:

```
---
id: domain.orders          # stable, unique across the KB, survives renames/moves
type: domain               # system | domain | flow | rule | entity | integration | adr | index
title: Orders
status: active             # optional
owner: someone             # optional
depends_on: [domain.users] # directed dependency edges, by id
relations:                 # typed, many-to-many edges, by id
  related_flows: [flow.order-creation]
---
body…
```

- **A graph, not a tree** — links are id strings, never paths, so they're stable across moves; edges are never resolved or validated by any tool; `list_docs` surfaces `depends_on` per row so the model traverses the graph itself.
- The frontmatter format is exact-inverse by construction between `render` and `parse`; `parse` deliberately handles only this one format, not general Jekyll/Obsidian frontmatter.
- `read_doc` re-renders frontmatter (hand-formatted YAML gets normalized; fields not in `DocMetadata` are dropped on round-trip).

## Path conventions

Vault-relative paths, conventionally: `00-system/` (overview, glossary, architecture, conventions) · `01-domains/<name>/` (overview, rules, entities, integrations) · `02-flows/` · `03-business-rules/` · `04-integrations/` · `05-data/` · `06-decisions/` (ADRs) · `99-index/`. The 01/02/03/04/06 directories appear lazily on first `write_doc`.

## The driver

`DocDriver` (trait): `kind()`, `write_doc(path, meta, body)`, `edit_doc(path, old, new, replace_all) -> String (unified diff)`, `read_doc(path) -> (DocMetadata, String)`, `search_docs(query, doc_type?) -> Vec<DocSearchHit>`, `list_docs(doc_type?) -> Vec<DocSummary>`. Paths are driver-defined identifiers (vault-relative for Obsidian; a page id for a future Notion driver) — deliberately all strings/structs.

**`ObsidianDriver`** — a local markdown folder Obsidian can open directly, no credentials. `new(vault_path)` creates and canonicalizes the root; a private `resolve()` re-implements the sandbox (a vault may live outside the turn's workspace) and rejects paths escaping it. `tools(driver: Arc<dyn DocDriver>) -> Vec<Arc<dyn Tool>>` returns the six tools — the same extension point the GitHub crate plays.

## The six tools

| Tool | Params | Behavior |
|---|---|---|
| `write_doc` | `path`*, `id`*, `type`* (8-value enum), `title`*, `status?`, `owner?`, `depends_on?` (id array), `relations?` (object of id arrays), `body`* | Create or **fully replace** a doc; creates missing parents. Output: `Wrote {path}`. |
| `edit_doc` | `path`*, `old_string`*, `new_string`*, `replace_all?` | Body-only exact-substring replace (frontmatter untouched); ambiguous → error. Output: unified diff (radius 3). |
| `read_doc` | `path`* | Frontmatter re-serialized + body. |
| `search_docs` | `query`*, `doc_type?`, `limit?` | Case-insensitive **substring** match over id, title, body — one hit per doc, priority id > title > body, snippet ~±40 chars around the match. Not regex, despite "the doc analog of grep". `limit` truncates the complete result client-side (no early stop). Empty query matches everything. |
| `list_docs` | `doc_type?` | Every doc's id/type/title/path/depends_on, "generated on demand instead of hand-maintained" — the domain/dependency map. |
| `scaffold_docs` | — | Creates the default folder skeleton (10 skeleton docs across 00-system, 05-data, 99-index) if missing; **idempotent** — never overwrites. |

## Registration

Gated only by `DocsConfig.enabled` (default on) — no credential needed. The engine builds `ObsidianDriver::new(vault_path)` (default `<workspace_root>/docs`) and registers the six tools into `external_tools`; failure → a global `Failed` ("documentation tools unavailable"). When doc tools are present, the engine appends a docs-protocol block to the system prompt: look up affected domains/rules/entities before non-trivial changes; update docs after behavior-affecting changes — treat them as part of the change, not an afterthought.

**Sub-agent turns** get only the three read-only doc tools (`read_doc`, `search_docs`, `list_docs`) — a filtered view of the same `external_tools` — so children can consult the KB but not mutate it.

## Notable behaviors

- Walk uses `ignore::WalkBuilder` with `.hidden(false)` — hidden files **are** included.
- `search_docs`/`list_docs` do synchronous `std::fs` reads inside async methods.
- Uncovered by tests: `limit`, empty query, unicode search, `relations` round-trip at the tool level.