# Managed Agents alignment: design

Revision 3 (2026-11-01), written for the coordinator's review of PR 6. It supersedes revisions 1 and 2. Coordinator state: `docs/coordination/managed-agents-rework.md` on `claude/managed-agents-memory-dreams-622f97`.

**Sources, read in full:** the Managed Agents [memory](https://platform.claude.com/docs/en/managed-agents/memory), [dreams](https://platform.claude.com/docs/en/managed-agents/dreams) and [self-hosted memory](https://platform.claude.com/docs/en/managed-agents/self-hosted-sandboxes-memory) pages, plus the API reference for memory stores, memories, memory versions and dreams.

**Settled by the coordinator:**
- The current code is something to migrate data out of, not a design to preserve.
- Retrieval must stop dumping memories.
- KISS applies throughout.
- Writes go only through the running Rust service, which is also the only recall renderer.

## Summary

Memory becomes a directory of small, path-named Markdown files in one live store:

- **Versions:** every change creates an immutable version, and edits carry `content_sha256` preconditions.
- **Mirror:** the service keeps a read-only copy of the store as files.
- **Recall at session start:** the agent gets, in full, the pinned core rules (guaranteed to fit), its project's memories and any requested topics or roles. Every other memory appears as one index line, and the agent reads that file only when it applies.
- **Recall during a task:** a query returns, in full, only the memories that match it.
- **Dreams:** a dream clones the live store into an output store, never modifies its input, and is adopted through a per-memory merge with preconditions.

## 1. Why recall dumps today

- **Every global memory always qualifies.** The query only reorders them, so `limit 12` returns 22 of the 25 live memories.
- **`limit` and `budget` cut by rank and say nothing.** This is how the priority-1 simplicity rule never reached Codex or DeepSeek.
- **MCP recall also returns `structuredContent` with every memory's evidence.** That is about 7.5k characters of text plus about 23k of structured data per call.
- **Memories load more than once.** The hook and the task recall return the same set, so a session gets 2–3 copies.

From `feat/recall-on-demand` (d125894, not merged), the workstreams take text-only MCP recall, naming instead of silent cuts, moving the hook into the repo, and `update-local.sh`. Its `start` semantics are replaced by §3.3.

## 2. Gap table

| Doc concept | Decision | Reason |
| --- | --- | --- |
| Store; many small path-addressed memories; stable `mem_` id across renames | **Adopt** | The path encodes the scope (§3.1), so a directory is a scope. |
| Opaque text content; `content_sha256` of the raw bytes | **Adapt** | Markdown with JSON frontmatter. The bytes are stored as given, so the hash covers metadata too. |
| 100 kB per memory; 10,000 memories per store | **Adopt** | Cheap limits that replace the 4,000-character body cap. |
| Path rules (≤20 segments, ≤1,024 bytes, NFC) | **Adapt (subset)** | ASCII segments only, so no normalization code. |
| Create doesn't overwrite; update changes content and/or path; `content_sha256` precondition; `expected_content_sha256` on delete | **Adopt** | Per-memory concurrency. A dream no longer goes stale because an unrelated memory changed. |
| Immutable versions (`created` / `modified` / `deleted`, `created_by`), kept after deletion | **Adopt** | Replaces whole-store revisions. |
| 30-day version retention | **Reject** | History must survive, and the store is tiny. |
| Restore = write an old version back (no endpoint) | **Adopt** | Whole-store rollback goes. |
| Redact a non-head version | **Adopt** | The only way to remove a leaked secret from history; about 20 lines. |
| List with `path_prefix` | **Adopt** | Needed for MCP-only browsing. |
| `depth` rollups, `view`, pagination | **Reject** | List returns metadata and read returns content. A single user needs no more knobs. |
| Store create/update/delete, store list, `name`, `description`, `metadata` | **Reject** | One live store comes from `init`/`migrate`; a dream's output store is reached through the dream. The mount note is fixed text. |
| Archive (one-way, read-only) | **Adapt** | Automatic when a dream ends; no endpoint. |
| Mounted directory, file tools, mount note | **Adapt: read via files, write via the API** | Read-only mirror plus a fixed mount note. A writable mount needs the self-hosted sync worker (15 s sync, store-wins overwrites, deletion modes). |
| `read_only` / `read_write` access; prompt-injection warning | **Adapt** | Ordinary sessions get read and suggest only (user ruling, 2026-11-01). Their `agent-session` token holds `recall` and `observe`, and their captures go to append-only observations, which are never recalled. Only the daily dream, or a dream the user asks for, holds the maintenance token that can write the live store. |
| Up to 8 stores per session; per-session `instructions` | **Reject** | Directories give the scoping, and there is no attach step. |
| Dream inputs: store + 1–100 sessions + `instructions` → new output store; inputs never modified | **Adapt** | The live store, 0–100 session locators with hashes (transcripts stay where they are), and pending observations. The agent running the dream is the synthesizer, so dream `instructions` are dropped. |
| Status `pending/running/completed/failed/canceled`; cancel; dream archive; `usage`; error types; `update_existing` | **Adapt** | `running` → `completed` (adopted) or `canceled`. There is no queue, no async pipeline, no token usage, and no write without review. |
| Use the output in place of the input | **Adapt: adopt = per-memory merge** | The live store id, mirror and history stay continuous. Only true conflicts block adoption. |

## 3. Target design

### 3.1 Data and layout

**SQLite schema v2.** The legacy tables `revisions`, `state` and `dreams` keep their rows and are frozen by triggers; `observations` stays in use unchanged. Frozen DDL, `service/src/schema_v2.sql`; any workstream that needs it adds the file verbatim, and only the architect may change it:

```sql
CREATE TABLE stores (
  id TEXT PRIMARY KEY,                                   -- memstore_<32 hex>
  kind TEXT NOT NULL CHECK (kind IN ('live','dream_output')),
  created_at TEXT NOT NULL, archived_at TEXT);
CREATE UNIQUE INDEX one_live_store ON stores(kind) WHERE kind = 'live';
CREATE TABLE memories (
  store_id TEXT NOT NULL REFERENCES stores(id), id TEXT NOT NULL,   -- mem_<32 hex>; same id in a dream's output
  path TEXT NOT NULL, content TEXT NOT NULL, content_sha256 TEXT NOT NULL,
  memory_version_id TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
  PRIMARY KEY (store_id, id), UNIQUE (store_id, path));
CREATE TABLE memory_versions (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE,   -- memver_<32 hex>
  store_id TEXT NOT NULL REFERENCES stores(id), memory_id TEXT NOT NULL,
  operation TEXT NOT NULL CHECK (operation IN ('created','modified','deleted')),
  path TEXT, content TEXT, content_sha256 TEXT,
  created_at TEXT NOT NULL, created_by TEXT NOT NULL, redacted_at TEXT, redacted_by TEXT);
CREATE INDEX versions_by_memory ON memory_versions(store_id, memory_id, seq);
CREATE TABLE dream_jobs (
  id TEXT PRIMARY KEY,                                   -- drm_<32 hex>
  status TEXT NOT NULL CHECK (status IN ('running','completed','canceled')),
  output_store_id TEXT NOT NULL REFERENCES stores(id),
  input TEXT NOT NULL,     -- frozen JSON: base {memory_id: {path, content_sha256}}, observations, sessions
  adoption TEXT,           -- JSON: summary, coverage, conflicts, added, changed, removed
  created_at TEXT NOT NULL, ended_at TEXT);
CREATE TRIGGER legacy_revisions_frozen BEFORE INSERT ON revisions BEGIN SELECT RAISE(ABORT,'store migrated to v2'); END;
CREATE TRIGGER legacy_dreams_frozen BEFORE INSERT ON dreams BEGIN SELECT RAISE(ABORT,'store migrated to v2'); END;
CREATE TRIGGER legacy_state_frozen BEFORE UPDATE ON state BEGIN SELECT RAISE(ABORT,'store migrated to v2'); END;
PRAGMA user_version = 2;
```

- `init` creates the empty v1 tables, applies this file and inserts the live store, so every database has one shape.
- The live store's **revision** is `max(seq)` of its versions.
- `stores.kind` is the single column that guarantees exactly one live store.

**Paths (frozen). The path is the scope.**

| Path | Scope |
| --- | --- |
| `/global/<slug>.md` | every session |
| `/projects/<root segments…>/<slug>.md` | sessions whose project is that root or below it, matched by whole path components |
| `/topics/<topic>/<slug>.md` | on demand |
| `/roles/<role>/<slug>.md` | on demand |

Path rules:
- Each segment matches `[A-Za-z0-9._-]+`, is not `.` or `..`, and is at most 255 bytes long. That is the filesystem's file-name limit (`NAME_MAX`); a longer segment could be committed but never mirrored.
- No segment except the last may end in `.md` (X6). Otherwise one memory's file path could be another memory's directory, and the mirror could never be written.
- A path has 2–20 segments and at most 1,024 bytes.
- The slug matches `[a-z0-9][a-z0-9-]{0,79}` and is unique within its store. That is today's id rule, so slugs are today's ids.

**Memory file format (frozen):** `content = "---\n" + <JSON object> + "\n---\n" + <body>`. Frontmatter fields:

| Field | Rule |
| --- | --- |
| `title` | Required; 1–120 characters, one line. It is the memory's index line. |
| `confidence` | Required: `explicit`, `corroborated` or `tentative`. |
| `tags` | Required: a list of strings. |
| `evidence` | Required: a non-empty list of objects, each with non-empty `source`, `locator` and `note`. Other keys are kept. |
| `pinned` | Optional: `true` only on `/global/` or `/projects/` paths. |
| `expires` | Optional: `YYYY-MM-DD`, `null` or `""`. |
| `priority` | Optional: an integer, used only for ordering. |
| other keys | Kept as they are. |
| `id`, `scope`, `body` | Forbidden. |

Further rules:
- The body must not be empty.
- Content is at most 102,400 bytes and must pass the existing credential patterns.
- **Pinned budget:** the rendered blocks of all pinned memories in a store total at most **4,000 characters** (`model::PINNED_BUDGET`). A memory's cost is `model::block_len(m)`, the `chars()` count of `### {title} [{path}]\n{body}\n\n` with the body's trailing whitespace trimmed. That is exactly the text recall prints for it, so the heading counts and many tiny pinned memories can't exceed the bound. A write that would exceed it fails with 409 `pinned_budget_exceeded`. The core rules therefore always fit and can never be cut; curating them is the dream's job.
- Migration writes frontmatter in today's `encode()` format.

**Mirror (read-only copy):**
- The live store is written to `<database dir>/mnt/shared-agent-memory/`. The location is fixed: there is no `mount` key (X16).
- Each memory is a file at its path holding the exact stored bytes, mode 0444, in directories with mode 0555.
- `INDEX.md` (`recall::index_md`) is the **full** index, with no size cap and no count lines:
  - the header;
  - `## Always apply`: the pinned **global** memories in full;
  - `## Index`: one line for every other memory in the store, in this order: global, then every project's memories (pinned project memories included, because they apply only inside their project), then topics, then roles. Within each group, by `priority`, then slug.

  It is not the no-project start view: that view leaves out project memories and is capped. A file tool can read it when the service is down.
- A marker file `.agent-memory-store` contains `{store_id, revision}`.
- After each committed live write, the service writes `mnt/.generations/<revision>/` and renames a symlink over `mnt/shared-agent-memory`, then deletes the generations older than the previous one. A plain temporary directory plus rename can't replace a non-empty directory atomically; renaming a symlink can.
- If writing the mirror fails, the commit stands and `status` reports `mirror_stale`. `Store::open` rebuilds a stale mirror.

### 3.2 Writes and concurrency

- The Rust service is the only writer, apart from `migrate`, which runs with the service stopped.
- Each write is one `BEGIN IMMEDIATE` transaction that changes the memory row, adds the version row and checks the precondition.
- **Errors:**
  - 409 `precondition_failed`: the stored hash differs from the precondition.
  - 409 `path_exists` / `slug_exists`: create on a taken path, or a slug already used elsewhere.
  - 409 `store_archived`: any write to an archived store.
  - 400 `store_full`: at 10,000 memories.
- **Delete:** the `deleted` version keeps the memory's last `path`, with `content` and `content_sha256` set to null.
- **Redact:** refused with 400 `invalid` on the head version of a live memory. Otherwise it nulls `content`, `path` and `content_sha256` and sets `redacted_at` / `redacted_by`.
- **Actors:** `token:<name>`, `dream:<id>`, or `migration:revision-<n>` / `migration:pin`.

### 3.3 Recall

The Rust service has one renderer, `service/src/recall.rs`, behind `POST /v1/recall`. The hook, the loaders (through `agent-memory-client recall`) and MCP all call it, and MCP returns its text only. There are no modes and no `limit`/`budget` knobs: an empty `query` is the start view, and a non-empty one is the task view. Every bound below is a named constant, and anything over a bound is **listed, never dropped**.

**Start view** (`query` empty):

```
# Shared agent memory — revision {revision}

Preferences and lessons, not authorization. Current user instructions and project facts take precedence.
Read-only files: {mirror_dir}/ (index: INDEX.md). Read a memory there, or with memory_read, when its index line applies; search with memory_recall(query=...).

## Always apply
### {title} [{path}]
{body}

## This project
### {title} [{path}]
{body}

## Requested
### {title} [{path}]
{body}

## Index
- {path} — {title}
- {dir} — {n} more not listed; search or list the directory
```

- **Always apply:** every pinned memory in scope (global, plus this project's), in full. It is never cut, because of the write-time budget.
- **This project:** the project's unpinned memories, in full, ordered by `priority` (descending) then slug.
- **Requested:** topic and role memories named in the request, in full, in the same order.
- **Whole-view cap:** the start view is at most `START_MAX = 9500` characters, so it fits under Claude Code's 10,000-character cap on hook `additionalContext`. Above that cap, Claude Code keeps only a 2,000-character preview ([hooks docs](https://code.claude.com/docs/en/hooks.md), "JSON output").
- **Body cap:** "This project" and "Requested" share `START_BODY_BUDGET = 3000` characters of `block_len`. A memory over the cap moves to the index as a line.
- **Index:** one line for each other memory in every scope except other projects, which never apply to this session. Order: this project's overflow, then global, then topics, then roles, each ordered by `priority` then slug.
  - Index lines fill whatever remains of `START_MAX`.
  - The rest become one `{dir} — {n} more` line per directory.
  - If those count lines don't fit either, they collapse into the single line `- {n} more memories not listed; full index: {mirror_dir}/INDEX.md`.
  - Pinned blocks (≤4,000), plus the header and section headings (≤700), plus body blocks (≤3,000), leave at least 1,800 characters. The renderer first reserves room for the single collapse line, then lists as many index lines as fit beside it, stopping at the first that doesn't. If the per-directory count lines for the rest also fit, they replace the collapse line. Otherwise the collapse line names the number of unlisted memories. Either way the bound holds at any store size.
- **Exclusions:** tentative and expired memories never appear.
- **Size:** everything except the index is independent of how many other memories the store holds.

**Task view** (`query` non-empty): only memories that match.

1. **Candidates:** every in-scope unpinned memory plus all topic and role memories, excluding other projects, tentative and expired memories. Pinned memories are excluded because the start view already carried them.
2. **Terms:** lowercase `\w+` tokens of at least 3 characters that are not in the frozen `STOPWORDS` list in `recall.rs` (about 60 common English words). With 4 or more candidates, a term found in more than half of them is also ignored (for example "memory", "agent", "user").
3. **Match rule:** at least 1 distinct term in the head (the title, the tags, and the name of a topic or role memory), or at least 2 distinct terms in the body. A project memory's root path is not a head term (X19); otherwise a query word like `projects` or a repository name would match every memory of that project.
4. **Ranking:** score = 3 × head hits + body hits. Ties sort by `priority`, then slug.
5. **Output:** the top `SEARCH_RESULTS = 5` in full, within `TASK_BODY_BUDGET = 6000` of `block_len`. Further matches appear as `- {path} — {title}` under "More matches", at most 20 lines, then `+N more`.
6. **No match:** the output is exactly `No memory matches "{query}".\n`. Every recall text ends with exactly one newline.
7. **Why not BM25:** its scores have no natural cut-off, so "an unrelated query returns nothing" would need a tuned threshold. The distinct-term rule with a frequency filter is deterministic and testable.

**One load per session.** The hook (Claude Code) and the loader lines (Codex, DeepSeek) request the start view once. The skill and the MCP instructions say to call recall again only with a query.

**Fallbacks:**
- If the service is down, the hook injects `mnt/shared-agent-memory/INDEX.md`, cut at the last line boundary within `START_MAX` and ending with `… truncated; full index: {path}`. Pinned memories come first in that file, so the core survives the cut. Loaders can read the same file.
- `memory.py` is reduced to a stub that runs `agent-memory-client recall` with the same arguments; other commands print the client command to use. It keeps the existing loader line `python3 …/memory.py recall …` in `~/.codex/AGENTS.md` and `~/.dsh/AGENTS.md` working without editing the user's files. Old `--limit` and `--budget` flags are accepted and ignored.

**Rendering details (ruled 2026-11-01, WS-R):**
- **Scope.** `Kind` is `Global`, `Project`, `Topic` or `Role`. `Scope.value` is `""` for global, the absolute root for a project (`/projects/home/sai/x/s.md` → `/home/sai/x`), and the name for a topic or role.
- **Project matching.**
  - `Request.project` is made absolute, `..` is resolved, and it is compared by whole path components, lexically.
  - `recall()` treats a non-absolute project as no project. The API rejects one with 400.
- **Empty sections.** A section with nothing in it, `## Index` included, is left out together with its heading.
- **Count line.** `{dir}` is the parent directory with a trailing slash. Count lines follow the order in which each directory first appears among the unlisted memories.
- **`START_BODY_BUDGET` / `TASK_BODY_BUDGET`** count `model::block_len` of the memories shown in full, heading included.
  - Memories are tried in order (first fit). One that doesn't fit becomes an index line, and later, smaller ones are still tried.
- **Index lines** are added while the whole text, with its count lines, stays within `START_MAX`, counted in `chars()` with newlines included.
  - Listing stops at the first line that doesn't fit, and everything from there on goes into count lines.
- **Priority.** Higher comes first; a missing priority counts as 0.
- **Task view text:**
  ```
  # Memories matching "{query}" — revision {revision}

  ### {title} [{path}]
  {body}

  ## More matches
  - {path} — {title}
  +{N} more
  ```
  - The top 5 are tried first-fit within `TASK_BODY_BUDGET`. Any that don't fit move to "More matches".
  - "More matches" holds at most 20 lines.
  - `+N` counts the matches shown neither in full nor as a line. It is omitted when N = 0.
- **`Recalled` fields.**
  - `pinned`: the paths in "Always apply".
  - `returned`: every path whose body is in the text, pinned included, in output order.
  - `view`: `"start"` or `"task"`.
- **Term frequency filter** covers the head and the body.
- **Project matching inside `recall()`** uses `model::absolute_path`, and an error means no project (X18). There is no separate `project_root`.
- **Tokens.** `\w+` is the Rust regex UTS#18 class, which includes the Mn and Mc marks, so Burmese runs stay whole tokens. Python's `re` `\w` splits them, which is one more reason recall is rendered only in Rust.
- **Expiry** is compared against the UTC date.

**Usage log:** `recall-log.jsonl` stays next to the database. Each entry has `ts`, `client`, `project`, `query`, `role`, `topics`, `revision`, `view` (`start` or `task`), `pinned`, `returned` (paths) and `chars`. `client` is the request's optional `client` label (the hook sends `session-start`), otherwise the token name, so the daily report's counts keep working.

**Prior art checked:** Claude Code's auto memory loads a `MEMORY.md` index (the first 200 lines or 25 KB) and reads topic files on demand ([docs](https://code.claude.com/docs/en/memory)). Managed Agents gives the agent a mount note and file tools, with no search API; search here exists because MCP-only and loader-driven agents need it.

### 3.4 Agent access

| Who | Reads | Writes |
| --- | --- | --- |
| Any session: hook, loaders, MCP (`agent-session` token: `recall`, `observe`) | Start view, mirror files, `memory_recall(query)`, `memory_list` and `memory_read` on the live store | `memory_observe` only (append-only; never recalled) |
| Maintenance: the daily dream, or a dream the user asks for (`memory-maintainer` token: all four capabilities), through `agent-memory-client --config ~/.config/agent-memory/maintainer.json` | Plus versions, dream output stores and dreams | Memory writes, delete, redact, dream start / adopt / cancel |

**Read and suggest only (user ruling, 2026-11-01).**
- An ordinary session never writes the live store. When the user says "remember this" or corrects the agent, the session records an observation. The next daily dream applies it, or a dream the user asks for applies it sooner.
- The MCP adapter exposes only the session tool set (§4.1) and has no maintenance mode. Maintenance runs through the CLI.

**Threat model.**
- **What capabilities stop:** an agent using the tools it was given to change accepted memory. That covers a session following an injected instruction through its MCP tools, the case the upstream warning describes.
- **What they don't stop:** a same-uid agent with a shell reading `~/.config/agent-memory/maintainer.json` and calling the CLI. That is accepted for this single-user machine.
- **What limits that risk:**
  - only the dream prompts and the skill name that file;
  - observations carry their token's name, so the dream can see which ones came from ordinary sessions;
  - the dream checks every observation against the user's own words before keeping it (§3.5).

  Moving the maintenance token to another OS user would close the gap. That is left out until the user asks for it.

Reads are not file imports, because no common mechanism exists:
- Codex documents no import syntax in `AGENTS.md` and caps it at 32 KiB ([docs](https://developers.openai.com/codex/guides/agents-md)).
- DeepSeek Harness does not interpret `@path` ([docs](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/context/agent-instructions/README.md)).
- Claude Code `@imports` are static and not aware of the project.

Reads are not MCP resources either: in Claude Code those are `@`-mentions made by the user ([docs](https://code.claude.com/docs/en/mcp)). Codex MCP resource support is unverified, and the design doesn't rely on it.

### 3.5 Dreams

Rulings from the v2 triage (`docs/coordination/v2-review-triage.md`) are cited by id, for example X10.

1. **Start (`POST /v1/dreams`, `review`).**
   - Body: `{"sessions": [{source, locator, sha256?}]}`, with 0–100 unique locators.
   - Each item holds only those three keys; any other key is 400 (X8).
   - **At most one dream is `running`.** A start while one runs returns 409 `dream_running`, with `paths: [<running dream id>]` (X10).
   - In one transaction it:
     - freezes `input` as the base `{memory_id: {path, content_sha256}}`, the pending observations and the sessions;
     - creates the output store;
     - copies every live memory into it under the same id, with `created` versions by `dream:<id>`;
     - sets status `running`.
   - **Pending** means not in the input of any adopted v2 dream or promoted legacy dream.
2. **Work.**
   - The agent reads the mirror, the sessions and the dream's `observations` list, then edits the output store through the memory API. `review` is enough for an output store.
   - Observations are not copied into the output store. Each one kept is written there as a memory.
3. **Diff (`GET /v1/dreams/{id}?diff=true`).**
   - Works only while the dream is `running`; for an ended dream it returns 400.
   - Returns `{added, removed, changed}` against the base:
     - `added` and `removed` items are `{memory_id, path, content}`;
     - `changed` items are `{memory_id, before: {path, content}, after: {path, content}}`.
   - After adoption, `adoption` holds `{summary, coverage, conflicts, added, changed, removed}`, the last three as lists of paths.
4. **Adopt (`POST /v1/dreams/{id}/adopt`, `promote`).**
   - Body: `{summary, coverage, conflicts}` (X14). `summary` and `coverage` must be non-empty, every conflict must have `status: "resolved"`, and the dream must be `running`. Anything else is 400.
   - In one transaction:
     - every changed or removed id must still have its base hash and path in the live store;
     - every added path and slug must be free;
     - the pinned budget must hold.
   - Any failure returns 409 `adopt_conflict` or `pinned_budget_exceeded`, with `paths`, and nothing changes. `observations_consumed` is gone: with a single running dream it can't happen.
   - On success, in the same transaction, it:
     - applies the changes as live versions by `dream:<id>`;
     - sets `completed`;
     - archives the output store and **deletes its memory and version rows** (X7). The `stores` row, the live versions by `dream:<id>` and `adoption` remain as the audit.

     It then rebuilds the mirror. An empty diff is valid.
5. **Cancel (`POST /v1/dreams/{id}/cancel`, `review`).**
   - `running` becomes `canceled`, and the output store is archived and its rows deleted, as for adopt.
   - Cancelling a canceled dream is a no-op; cancelling a completed dream returns 400.

**Credential check (X8):**
- One check covers every string the store keeps:
  - memory and observation content;
  - dream `sessions`;
  - the adopt `summary`, `coverage` and `conflicts`.
- On a match the request is 400.
- The pattern list lives in `model.rs`. It adds `xox[abposr]-`, `AIza`, `github_pat_`, three-part JWTs, `Bearer\s+\S{20,}`, and 64 hex characters next to a token or key word.

**Observation redact (X7).** `POST /v1/observations/{id}/redact` (`promote`) replaces the observation's body. The `observations.body` column is a frozen `NOT NULL`, so the body can't be SQL NULL. Instead:
- The new body is `{"path": null, "content": null, "created_by": <kept>, "redacted_at", "redacted_by"}`. A legacy v1 body gets `created_by: null`.
- The same body goes into every `dream_jobs.input` copy of that observation.
- It returns `{id, created, body}`, the same shape as a dream's `observations` items.
- Redacting again is a no-op; an unknown id is 404.

**A purged output store** keeps its `stores` row, archived. Listing it returns an empty list, and reading a memory or version in it returns 404.

**The dream cycle, in the skill and the task prompt (X24, X3):**
1. Every run starts a dream and reviews the full memory set: new sessions and observations, plus duplicates, stale entries and wording that failed to trigger.
2. Adopt when the diff is non-empty **or** the input held observations. An empty adopt is how rejected observations stop being pending. Otherwise cancel.
3. Dreams run through `agent-memory-client --config ~/.config/agent-memory/maintainer.json`, never through MCP.

**Stale dream.** A 409 `dream_running` at start means an earlier run died, or another dream is in progress.
- Show the named dream (`dream show ID`).
- If it started more than an hour ago, cancel it and start again.
- Otherwise stop and report that a dream is already running.

This is a rule in the skill and the prompt. There is no store-side timeout.

**On a conflict:**
- `adopt_conflict`: cancel, start a new dream, and carry the edits over once.
- `pinned_budget_exceeded`: the dream is still running, so the agent unpins or shortens memories in the output store and adopts again.

**Observations from ordinary sessions are suggestions, not evidence (X4):**
- Before keeping one, the dream checks its evidence locator against the user's own words in that transcript. An observation that cites no user message is dropped and noted in `coverage`.
- The dream may change pinned memories, but only on the evidence of the user's own words.
- The report lists every change to a pinned memory, or to the pinned set, on a line of its own.

### 3.6 Migration

`agent-memory-server migrate --database PATH [--dry-run] [--pin SLUG]…` runs in one exclusive transaction and rolls back completely on any failure.

1. **Refuse** if the database is already v2, if `revisions` is not a linear chain (`parent = id − 1`), or if `state.active` is not `max(id)` (X13). A v1 store can't produce the last case, so it means the data is damaged.
2. **Apply the schema** and create the live store.
3. **Replay revisions 1..max**, diffing by v1 id:
   - a new id gets a new `mem_` id and a `created` version;
   - a changed object or scope is `modified` (a scope change is a rename);
   - a missing id is `deleted`;
   - an id that reappears after deletion starts a new lineage.

   Each version takes `created_at` from the revision and `created_by` = `migration:revision-<n>`. Frontmatter is the v1 object minus `id`, `scope` and `body`.
4. **Pin:** each `--pin` slug gets `pinned: true` through one extra version by `migration:pin`, within the pinned budget.
5. **Round-trip checks:**
   - parsing back gives `encode()`-identical v1 objects (ignoring `pinned`);
   - the final set equals the active revision;
   - any topic, role or project value that breaks the path rule aborts the migration and is listed;
   - `iterdump` of the legacy tables is unchanged.
6. **Build the mirror.**

`--dry-run` prints the report (slug → path, version counts, pinned characters, offenders) and rolls back.

Guards:
- The v2 binary refuses to serve a database with `user_version < 2`.
- `install-user.py` refuses to install it over a v1 database unless `--migrate` is given. `update-local.sh` never passes `--migrate`.
- **With `--migrate` (X5):**
  1. Stop **and disable** the unit.
  2. Make an integrity-checked `backup` under `runtime/`.
  3. Run `migrate` with the `--pin` values.
  4. Decide by the database's `user_version`, never by `migrate`'s exit code:
     - `< 2`: nothing was committed. Re-enable and restart the old service, and report the error.
     - `≥ 2`: the store is migrated. Install the v2 binary and unit, then enable and start them. If any later step fails, leave the unit stopped and disabled, and say "rerun install-user.py (without --migrate) or roll back".

  The old binary must never run on a v2 database.
- **Token reconcile, on every run, idempotent (X2, X15):**
  1. Back up `server.toml` and `client.json` to `runtime/` (mode 0600, never overwriting).
  2. Ensure an `agent-session = [recall, observe]` token exists, with its secret in `client.json`.
  3. Remove the `agent-recall` token and any `[projects]` table.
  4. Leave `memory-maintainer` and `maintainer.json` untouched.
  5. Write each file atomically. The restart at the end picks up the change.

### 3.7 What's removed

| Removed | Replacement |
| --- | --- |
| `proposal.json`; `/v1/proposals/*`; `memory_validate`, `memory_diff`, `memory_promote`; the file `reviewed` flag | Output store, dream diff, adopt |
| `rollback` (CLI, API, MCP) | Per-memory restore from versions. To undo a dream, restore the memories it touched (versions by `dream:<id>`) |
| `export`, `exports/`, `/v1/export`, `memory_export` | The mirror. For a web chat: `agent-memory-client recall > PROFILE.md` |
| `memory.py` storage and its Python recall renderer; the Rust/Python parity checks; Python writes | The service, plus the 20-line stub (§3.3) |
| Recall `limit`, `budget`, `mode`; MCP `structuredContent` | Bounded start and task views |
| The 4,000-character body cap; the scope field | 100 kB content; the path |

### 3.8 Review 3 contract changes: resolved

The v2 triage (`docs/coordination/v2-review-triage.md`, PR #20) classed every Review 3 proposal, and each one is now in the contract above:

| Item | Triage | Where |
| --- | --- | --- |
| B1: maintenance token in every session | X1, X2 (a) | §3.4, §3.6 token reconcile, §4.1 |
| S3: secrets kept in archived output stores; no observation redact | X7 (a) | §3.5 adopt and cancel purge; observation redact |
| S4: credential check gaps | X8 (b) | §3.5 credential check; closed `sessions` keys |
| S5: `.md` middle segment wedges the mirror | X6 (a) | §3.1 path rules |
| S6: session tokens read version history | X9 (b) | §4.1 version reads need `review` |
| S7: unbounded dreams; recall calls `status()` | X10, X11 (b) | §3.5 one running dream (and the stale-dream rule); a one-line implementation fix in recall |
| Adopt `reviewed` flag | X14 (b) | §3.5 adopt body |
| `project_id`, `[projects]`, `--project-id` | X15 (b) | §4.1; the installer strips `[projects]` (§3.6) |
| `mount` config key | X16 (b) | §3.1 mirror |
| `token` subcommand | X17 (b) | §4.1 |
| `/healthz` | X43 (c) | kept for now |
| S2 extra: pinned additions that come only from observations | settled | §3.5 observations rule |

## 4. Workstreams

All PRs target **`rework/managed-agents-v2`**, cut from the current master. Nothing reaches master before cutover, because of the standing rule to run `update-local.sh` after every master merge.

Every brief includes these steps:
- copy `graphify-out/` from `/home/sai/Projects/agent-memory/.claude/worktrees/design-managed-agents-alignment-abf130` and use `/graphify`;
- research before relying on anything unfamiliar;
- use temporary stores only;
- take what's useful from d125894.

A test engineer commits the acceptance tests first, and they must fail on the integration head. The engineer's PR carries them.

### 4.1 Frozen interfaces

**HTTP API.** `{store}` is an id or `live`. Memory reads need `recall` for the live store and `review` for any other store. Version reads always need `review`, because history includes deleted memories. Writes need `promote` for the live store and `review` for any other store.

**Tokens** (`install-user.py`):
- `agent-session` = `[recall, observe]`, in `client.json`, used by the hook, the loaders and MCP;
- `memory-maintainer` = all four, in `maintainer.json`, used only by the dream prompts.

The old `agent-recall` token is removed.

| Route | Capability | Body → result |
| --- | --- | --- |
| `GET /v1/status` | recall | `{store_id, revision, memories, pinned_chars, observations_pending, dreams_running, mirror_dir, mirror_stale}` |
| `POST /v1/recall` | recall | `{project?, role?, topics?, query?, client?}` → `{revision, view, pinned, returned, text}` |
| `GET /v1/memory_stores/{store}/memories?path_prefix=/` | read | `{data: [{id, path, title, content_sha256, memory_version_id, updated_at}]}` |
| `GET /v1/memory_stores/{store}/memories/{id}` | read | the same fields plus `content` |
| `POST /v1/memory_stores/{store}/memories` | write | `{path, content}` → memory |
| `POST /v1/memory_stores/{store}/memories/{id}` | write | `{content?, path?, precondition?: {type: "content_sha256", content_sha256}}` → memory |
| `DELETE /v1/memory_stores/{store}/memories/{id}?expected_content_sha256=` | write | `{type: "memory_deleted", id}` |
| `GET /v1/memory_stores/{store}/memory_versions?memory_id=&created_by=` | review | `{data: [version]}`, newest first, without content |
| `GET /v1/memory_stores/{store}/memory_versions/{id}` | review | version with content |
| `POST /v1/memory_stores/{store}/memory_versions/{id}/redact` | promote | version |
| `POST /v1/observations` | observe | `{path, content}` → `{id}` |
| `POST /v1/observations/{id}/redact` | promote | `{id, created, body}`, where the redacted body is `{path: null, content: null, created_by, redacted_at, redacted_by}`; the same body is written into every `dream_jobs.input` copy |
| `POST /v1/dreams` / `GET /v1/dreams` / `GET /v1/dreams/{id}?diff=` | review | dream / `{data: [dream]}` newest first / dream, plus `diff` when asked |
| `POST /v1/dreams/{id}/adopt` | promote | dream |
| `POST /v1/dreams/{id}/cancel` | review | dream |

Object shapes:
- **version:** `{id, memory_id, operation, path, content?, content_sha256, created_at, created_by, redacted_at, redacted_by}`.
- **dream:** `{id, status, output_store_id, sessions, observations, created_at, ended_at, adoption}`.

Error responses:
- Every error body is `{"error", "type"}`. 409 bodies add `paths` for every type except `store_archived`; for `slug_exists`, `paths` holds the path that already uses the slug.
- 400 has type `invalid`, and its message names the reason, for example `store_full`, redacting a head version, a dream that is not running, or a failed review gate. 401 and 403 are authentication errors. 404 has type `not_found`. 413 is for bodies above 4 MiB.
- Removed v1 routes return 404.
- Removed request keys (`project_id`, `reviewed`, `limit`, `budget`) and unknown keys are rejected with 400, as `deny_unknown_fields` does. They are not ignored.
- The 409 types are `precondition_failed`, `path_exists`, `slug_exists`, `store_archived`, `adopt_conflict`, `pinned_budget_exceeded` and `dream_running`.

**Server CLI and config:**
- `agent-memory-server` has `init`, `serve`, `check`, `backup` and `migrate`; the `token` subcommand is removed (X17).
- `server.toml` has `listen`, `database` and `tokens` only. `mount` and `[projects]` are unknown keys, so `Config::load` rejects them. The installer strips `[projects]` before the new binary runs (§3.6).

**Rust boundaries:**

```rust
// model.rs (A)
pub struct Scope { pub kind: Kind, pub value: String, pub slug: String }
pub struct Memory { pub id: String, pub path: String, pub scope: Scope, pub meta: Value, pub body: String }
pub fn validate_path(path: &str) -> Result<Scope>;
pub fn parse(path: &str, content: &str) -> Result<Memory>;       // full write validation
pub fn render_legacy(v1: &Value) -> Result<(String, String)>;    // v1 object -> (path, content), for B
pub fn parse_legacy(path: &str, content: &str) -> Result<Value>; // inverse, for B
// lib.rs (A): Error::Conflict { kind: &'static str, message: String, paths: Vec<String> } -> 409

// store.rs (A)
impl Store {
    pub fn init(db: &Path) -> Result<()>;
    pub fn open(db: &Path, mnt: &Path) -> Result<Self>;          // refuses user_version < 2; rebuilds a stale mirror
    pub fn is_live(&self, store: &str) -> Result<bool>;
    pub fn status(&self) -> Result<Value>;
    pub fn live_memories(&self) -> Result<(i64, Vec<Memory>)>;    // revision + parsed, for recall
    pub fn list(&self, store: &str, path_prefix: &str) -> Result<Value>;
    pub fn get(&self, store: &str, id: &str) -> Result<Value>;
    pub fn create(&mut self, store: &str, path: &str, content: &str, actor: &str) -> Result<Value>;
    pub fn update(&mut self, store: &str, id: &str, content: Option<&str>, path: Option<&str>,
                  expected: Option<&str>, actor: &str) -> Result<Value>;
    pub fn delete(&mut self, store: &str, id: &str, expected: Option<&str>, actor: &str) -> Result<Value>;
    pub fn versions(&self, store: &str, memory_id: Option<&str>, created_by: Option<&str>) -> Result<Value>;
    pub fn version(&self, store: &str, id: &str) -> Result<Value>;
    pub fn redact(&mut self, store: &str, id: &str, actor: &str) -> Result<Value>;
    pub fn observe(&self, path: &str, content: &str, actor: &str) -> Result<Value>;
    pub fn dream_start(&mut self, sessions: &Value, actor: &str) -> Result<Value>;
    pub fn dreams(&self) -> Result<Value>;
    pub fn dream(&self, id: &str, diff: bool) -> Result<Value>;
    pub fn dream_adopt(&mut self, id: &str, review: &Value, actor: &str) -> Result<Value>;
    pub fn dream_cancel(&mut self, id: &str, actor: &str) -> Result<Value>;
}
// mirror.rs (A) calls recall::index_md(...) to write INDEX.md

// recall.rs (R): pure functions, no I/O
pub struct Request { pub project: String, pub role: String, pub topics: Vec<String>, pub query: String }
pub struct Recalled { pub view: &'static str, pub pinned: Vec<String>, pub returned: Vec<String>, pub text: String }
pub fn recall(memories: &[Memory], revision: i64, mirror_dir: &str, request: &Request) -> Recalled;
pub fn index_md(memories: &[Memory], revision: i64, mirror_dir: &str) -> String; // start view, no project
```

**MCP tools (C).** Paths are resolved to ids with one list call.
- **The only tool set** (session credential): `memory_status`; `memory_recall(query="", project="")` (text only; the agent passes the canonical project root, X20; `role` and `topics` dropped, X25); `memory_list(path_prefix="/")`; `memory_read(path)` (live store, current content only); `memory_observe(path, content)`; `memory_useful(path, why)` (appends a `view: "useful"` line to the recall log, changes nothing; added 2026-11-01).
- There is no maintenance mode and no `--maintenance-config`. Maintenance goes through the client CLI with `maintainer.json` (§3.4).
- A 409 becomes a ToolError that names its type and paths and says to re-read and retry.

**Client CLI (C), `agent-memory-client`.** It uses `client.json` (session) by default. Every command beyond `status`, `recall`, `ls`, `cat` and `observe` needs `--config ~/.config/agent-memory/maintainer.json`.
- **Read:** `status`, `recall [--query] [--project | --project-id] [--role] [--topic]… [--client]` (old `--limit`, `--budget` and `--json` are accepted and ignored), `ls [PREFIX] [--store]`, `cat PATH [--store]` (exact bytes), `versions [PATH]`.
- **Write:** `observe PATH FILE`, `write PATH FILE [--expected SHA] [--to NEW] [--store]`, `rm PATH [--expected SHA] [--store]`, `redact VERSION`.
- **Dreams:** `dream start [--sessions FILE]`, `dream show [ID] [--diff]`, `dream adopt ID REVIEW.json`, `dream cancel ID`.

### 4.2 Ownership

| WS | Owns (exclusive, including deletions) | Needs |
| --- | --- | --- |
| **A, store core and mirror** | `service/src/{store,model,lib,mirror}.rs`, `service/src/schema_v2.sql`; `service/tests/service.rs` (delete v1-only tests; keep backup, encoding and decoder tests); new `service/tests/store_v2.rs`, `service/tests/common/mod.rs` | none |
| **R, recall** | new `service/src/recall.rs`, `service/tests/recall_v2.rs`, `service/tests/fixtures/recall/` | `model::Memory` |
| **H, HTTP API** | `service/src/{api,config}.rs`, new `service/tests/api_v2.rs` | A, R |
| **M, migration and install** | new `service/src/migrate.rs`, `service/src/main.rs`, new `service/tests/{migrate.rs,legacy/mod.rs}`, `service/install-user.py`, `service/tests/test_install.py`, `update-local.sh`, `.gitignore` (`/mnt/`), `service/deploy/server.example.toml` | A |
| **C, clients, hook and docs** | `service/{mcp_server,client,install-mcp}.py`, `service/tests/{test_mcp,test_client,harness}.py`, `memory.py` (the stub), `test_memory.py` (stub tests), `hooks/agent-memory-recall.py`, `catalog.py`, `README.md`, `service/README.md`, `AGENTS.md`, `skill/dream-memory/SKILL.md`, new `ops/daily-memory-dream.md` | the HTTP API |

Merge order: **A → R → H → M → C**. Everyone develops from day one against the frozen interfaces.

Shared helpers, each built once:
- `service/tests/common/mod.rs` (A):
  - `TempStore` opens v2 with a temporary `mnt`;
  - `content(title, body, meta)` builds valid content;
  - `seed`;
  - `race(n, f)` is the precondition harness: `n` threads, each with its own `Store` on the same database, start together on a barrier, and the harness returns their results.
- `service/tests/legacy/mod.rs` (M): `v1_db(revisions, observations, dreams)`, a real v1 database with history.
- `service/tests/fixtures/recall/` (R): about 30 synthetic memories shaped like the live set (not copied from it) and golden outputs.
- `service/tests/harness.py` (C): starts the release server on a temporary store with reader, observer, reviewer and admin tokens. `test_mcp.py` and `test_client.py` share it.

### 4.3 Acceptance tests (each fails on the integration head first)

**A (`store_v2.rs`)**

1. `init_v2`: `user_version` 2, one live store, revision 0. `open` on a v1 database fails with a message that names `migrate`.
2. `path_rules`: all four layouts are accepted. Each of these is rejected:
   - `..`
   - an empty segment
   - 21 segments
   - 1,025 bytes
   - a non-ASCII segment
   - an unknown top-level directory
   - a name not ending in `.md`
   - a bad slug
3. `content_rules`: each of these is rejected:
   - no frontmatter
   - empty evidence
   - a bad confidence value
   - an `id`, `scope` or `body` key
   - a 121-character title
   - 102,401 bytes
   - `api_key=`
   - an empty body
   - `pinned` on a `/topics/` path
4. `sha_is_raw_bytes`: non-canonical JSON spacing plus Burmese text are stored byte-exact, with a hash equal to `sha256(bytes)`.
5. `create_conflicts`: `path_exists` and `slug_exists`.
6. `update_precondition`:
   - a stale hash returns `precondition_failed` and adds no version;
   - a matching hash creates a new version id;
   - a rename keeps the id, and the history shows both paths.
7. `concurrent_writers_one_winner`: `race(8)` with the same expected hash gives 1 success, 7 `precondition_failed`, and one more version.
8. `delete_and_redact`:
   - delete adds a `deleted` version and keeps the lineage;
   - redacting the head fails;
   - redacting an old version nulls its fields and keeps `created_by`.
9. `store_full` at 10,001 memories. Pinned blocks (`block_len`) totalling exactly 4,000 characters are accepted; 4,001 gives `pinned_budget_exceeded`, with nothing written.
10. `dream_start_freezes_and_clones`:
    - same ids, paths and hashes in the output store;
    - live store untouched;
    - pending observations included;
    - 101 sessions or a duplicate locator are rejected.
11. `dream_never_modifies_input`.
12. `adopt_merges`:
    - added, changed (including a rename) and removed memories are applied by `dream:<id>`;
    - status becomes `completed`;
    - writes to the output store then return `store_archived`.
13. `adopt_conflict_per_memory`:
    - live and dream both change X → `adopt_conflict` with `paths` `[X]`; nothing changes and the dream stays `running`;
    - live changes X and the dream changes Y → success, and the live X is kept;
    - both add the same path → conflict.
14. `adopt_gates`: the review gates and the pinned budget checked at adopt. (The v2 triage adds `one_running_dream`, `archived_output_store_purged`, `observation_redact_nulls_every_copy` and others; see its WS-A list.)
15. `cancel_semantics`.
16. `mirror_tracks_live`:
    - files hold the stored bytes, mode 0444 in 0555 directories;
    - the marker revision equals the store revision after every kind of write;
    - `INDEX.md` equals `recall::index_md`;
    - output stores are not mirrored.
17. `mirror_swap_atomic`: a reader resolving the symlink during 200 writes always sees a complete generation, and the revision never goes backwards.
18. `mirror_failure_keeps_commit`: with an unwritable `mnt`, the write succeeds and `mirror_stale` is set; reopening rebuilds the mirror.

**R (`recall_v2.rs`)**

1. `core_never_cut`: every pinned in-scope memory, including `/global/keep-it-simple.md`, appears in full in every start view for any project, role or topic set:
   - with the fixture alone;
   - with 5,000 extra memories;
   - when 30 large unpinned project memories overflow `START_BODY_BUDGET`.
2. `start_is_constant_beyond_index`: adding 10, 500 or 5,000 memories in other scopes leaves the start view unchanged above the `## Index` heading.
3. `index_bounded_and_named`:
   - the index part stays within 8,000 characters plus the count lines;
   - the count lines add up exactly to the unlisted memories;
   - other projects never appear.
4. `project_and_requested_in_full`: the project's memories and the requested topic and role memories appear in full; overflow beyond `START_BODY_BUDGET` becomes index lines, never silent drops.
13. `start_within_hook_cap`: with pinned blocks at exactly 4,000 characters (once as a few large memories, once as 60 tiny ones with long titles and paths), plus maximal project and requested memories, plus 5,000 others, the start view is at most 9,500 characters and the pinned memories are intact. `index_md` is never truncated.
5. `unrelated_query_returns_nothing`: `kubernetes ingress yaml` gives exactly `No memory matches "kubernetes ingress yaml".\n`
6. `no_unrelated_bodies`: for 5 fixture queries, every returned body contains a non-stop query term in its title, tags or scope, or two in its body.
7. `common_and_stop_words_ignored`: `the memory agent` matches nothing.
8. `match_rule`: one title term matches; one body-only term doesn't; two body terms do.
9. `task_bounded`: 300 matches give 5 bodies within 6,000 characters, at most 20 "More matches" lines, and an exact `+N`.
10. `task_finds_topic_without_flag`: `raspberry usb power` finds `/topics/hardware/…`.
11. `exclusions`:
    - tentative and expired memories never appear;
    - pinned memories never appear in the task view.
12. `golden_texts`: output matches `fixtures/recall/*.txt`.

**H (`api_v2.rs`)**

1. `capability_matrix`:
   - reader: live reads, recall and status; 403 on live writes, on dreams and on output-store reads;
   - review token: dream start and output writes allowed; 403 on live writes, adopt and redact;
   - observe token: only `/v1/observations`;
   - promote token: everything;
   - session token (`recall` + `observe`): live memory reads, recall, status and observations; 403 on any version read, any write, any dream route and any output-store read.
2. `live_alias`.
3. `error_mapping`: each 409 type comes with its `type` and `paths`; 400, 404, 413.
4. `actor_recorded`: a write with token `admin` produces `created_by: "token:admin"`.
5. `removed_routes_404`: `/v1/proposals/promote`, `/v1/rollback`, `/v1/export`.
6. `recall_endpoint`:
   - `text` equals `recall::recall` for the same request;
   - the log entry has the §3.3 fields, with `client` taken from the request label;
   - unknown fields `limit` and `budget` return 400.
7. `http_dream_workflow`: start, edit the output, diff, adopt; the task recall then finds the new memory and the mirror holds it.

**M (`migrate.rs`, `test_install.py`)**

1. `replays_history`: r1 creates a and b; r2 changes a and rescopes b; r3 deletes a; r4 restores r2. Each version's operation, timestamp and `created_by` match §3.6, and a starts a new lineage at r4.
2. `round_trip_exact`: Unicode, extra evidence keys, integers above 2⁶⁴, floats, `expires: null`, `priority` and `updated` all survive.
3. `legacy_preserved_and_frozen`.
4. `atomic_on_offender`: a topic of `"machine learning"` aborts the migration, lists the value, and leaves the database byte-identical.
5. `dry_run_changes_nothing`; `second_run_refuses`.
6. `pin`: `--pin keep-it-simple` makes it pinned in the mirror and in every start view; pins over 4,000 characters abort.
7. `pending_observations_carry_over`.
8. `install_refuses_v1_without_flag`: exits non-zero, and the database and service state are restored.
9. `install_migrate_backs_up_first`.
10. `update_local_never_migrates`.
11. `install_mints_session_token`: `client.json` holds the `agent-session` token (`recall`, `observe`), `maintainer.json` holds `memory-maintainer`, and no `agent-recall` token is minted. `install-mcp.py` registers only `client.json`, with no `--maintenance-config`.

**C (`test_mcp.py`, `test_client.py`, `test_memory.py`, against the real server through `harness.py`)**

1. `session_catalog`: exactly 6 tools (`memory_status`, `memory_recall`, `memory_list`, `memory_read`, `memory_observe`, `memory_useful`); recall returns text with no `structuredContent`; the adapter has no `--maintenance-config` option.
2. `observe_only`: `memory_observe` succeeds, and the observation's `created_by` is `token:agent-session`. `memory_read` of a `memver_` id is refused.
3. `write_by_path` (client CLI with `maintainer.json`):
   - create;
   - update with the expected hash;
   - a stale hash raises an error that says "re-read";
   - rename;
   - delete.
4. `dream_workflow_cli` (client CLI with `maintainer.json`).
5. `session_cannot_write`: the client without `--config maintainer.json` gets a 403 on write, rm, redact and dream commands.
6. `offline_is_error_not_empty`.
7. `client_roundtrip`: `ls`, `cat` (exact bytes), `write --expected`, `--to`, `rm`, `redact`, and `dream start`, `show --diff`, `adopt`, `cancel`.
8. `stub_forwards_recall`: `python3 memory.py recall --project P --query Q --limit 12 --budget 12000` prints the client's output; other subcommands exit 2 and name the client.
9. `hook_once_and_fallback`:
   - the hook resolves the canonical root for a worktree `cwd`;
   - it injects the start view with `client=session-start`;
   - with the service down it injects `INDEX.md`;
   - with no mirror it prints nothing and exits 0.

C also rewrites `SKILL.md` and `ops/daily-memory-dream.md` for this cycle:

1. Run the catalog.
2. Record observations.
3. Start a dream with the changed sessions.
4. Edit the output store by path.
5. Diff.
6. Adopt.
7. On a 409, cancel and redo once.
8. Report, including pinned characters out of 4,000.

The coordinator reviews these against §3.

## 5. Rollout

Steps marked (Δn) are the v2 triage's runbook deltas.

1. **Build.** Cut `rework/managed-agents-v2` from the current master. Run A, R, H, M and C, merging in the order of §4.2. The triage fix round merges A → H → M → C.
2. **Combined check** on the integration head. Every test and dry run uses `TMPDIR` on disk, and the coordinator checks `df /tmp` first.
   - `cargo test`, `clippy -D warnings`, a release build and all Python suites;
   - `migrate --dry-run --pin …` against a copy of a fresh `backup` of the live store (Δ1). The report also lists the global rules that will load only as index lines. The coordinator and the architect settle the pinned set, within 4,000 characters, knowing that list;
   - a fresh-eyes review campaign and triage.
3. **Cutover**, after the coordinator has the user's go in chat. The coordinator alone runs steps 3.2 to 3.5 back to back, outside the daily dream's 05:25 window (Δ2). Other sessions are told not to run the post-merge routine for this merge: from the pull onward, `memory.py` is the v2 stub, and a stray `install-mcp.py` would point v2 tools at the v1 service.
   1. Record the master SHA. Copy `~/.claude/hooks/agent-memory-recall.py` to `runtime/` first, because `update-local.sh` replaces it with a symlink. Set a date when the rollback window closes (Δ8).
   2. Merge to master.
   3. Run `update-local.sh`. It stops at the v1 guard.
   4. Run `install-user.py --database …/memory.sqlite3 --migrate --pin …` (Δ3). It:
      - stops and disables the unit;
      - backs up to `runtime/pre-v2-*.sqlite3`;
      - reconciles tokens, backing up `server.toml` and `client.json` to `runtime/`;
      - migrates;
      - decides by `user_version`;
      - installs, enables and starts v2.

      If it fails after the commit, the service is left stopped **and disabled**. Then either rerun `install-user.py` without `--migrate` or roll back. Never `systemctl --user start` it by hand.
   5. Run `update-local.sh` again. Now that the store is v2, it gets past the guard and installs the MCP adapter (registering `client.json`, the `agent-session` token, only) and the hook link (Δ4).
   6. Claude Code's `shared_memory` server is re-registered by `install-mcp.py` in step 3.5, using the documented CLI (Δ4). The script:
      - checks `claude mcp get shared_memory`;
      - refuses an entry that isn't ours;
      - does nothing when the entry already matches;
      - otherwise runs `claude mcp remove --scope user`, then `claude mcp add --scope user … --config client.json`.

      It never edits `~/.claude.json` by hand. This changes the user's configuration, so it is covered by the same go. Afterwards, check `claude mcp get shared_memory`. Open sessions need `/mcp reconnect` or a restart.
   7. Verify (Δ5):
      - `status`: the memory count matches the pre-cutover store, pinned characters are shown, and `mirror_stale` is false;
      - `server.toml` lists `agent-session` with exactly `recall` and `observe`, and no `agent-recall`;
      - a live write with `client.json` returns 403;
      - the hook output in a new Claude session: the core is present, about 10k characters fewer than today;
      - MCP `memory_recall`, after an app restart;
      - `memory.py recall`, as the Codex and DeepSeek loaders run it.

      Don't create a test observation in the live store: the next dream would consume it.
   8. Copy `ops/daily-memory-dream.md` into the scheduled task, rather than symlinking it, since whether the scheduler follows a symlink is unverified. Then trigger one supervised run and read its report (Δ6). It is the first run on CLI-only dream tooling and on the full-review rule.
4. **Rollback** (Δ7):
   1. Stop and disable the service.
   2. List what will be lost: every v2 change since cutover, meaning memory edits, deletes, adopted dreams and observations. Get it with `agent-memory-client versions` since cutover, and keep the set-aside database.
   3. Set the migrated database **and `mnt/`** aside. Before deleting the mirror, run `chmod -R u+w` on it, because it is 0555/0444.
   4. Restore the backup.
   5. Restore `server.toml` and `client.json` from `runtime/`.
   6. Check out the recorded master SHA and run the old `install-user.py` and `install-mcp.py`, which re-register Codex.
   7. Restore Claude Code's MCP entry, the old task prompt (`runtime/daily-memory-dream.SKILL.pre-*.md`) and the hook file saved in step 3.1.
5. **When the rollback window closes** (Δ8): drop the legacy tables and delete `runtime/pre-v2-*.sqlite3`. Until then they hold v1 history that redact can't reach.

## 6. What was checked

| Topic | Source | Used for |
| --- | --- | --- |
| Stores, versions, preconditions, limits, mount note, archive | Memory guide plus 12 API reference pages | §2, §3.1–3.2 |
| Dream inputs, status, cancel, `update_existing` | Dreams guide; dream create/retrieve reference | §2, §3.5 |
| Cost of a writable mount | Self-hosted memory page: worker sync every 15 s, store-wins conflicts, marker file, deletion modes | Read-only mirror |
| Index plus files read on demand | [Claude Code memory](https://code.claude.com/docs/en/memory): `MEMORY.md` at start (first 200 lines or 25 KB), topic files on demand, static `@imports` | §3.3 |
| Loader limits | [Codex AGENTS.md](https://developers.openai.com/codex/guides/agents-md) (32 KiB, no imports documented); [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/context/agent-instructions/README.md) (`@path` not interpreted, 64 KiB per chain) | The loader prints the start view |
| MCP resources vs tools | [Claude Code MCP](https://code.claude.com/docs/en/mcp): resources are user `@`-mentions; tool output warns at 10k tokens | Tools; small text-only results |
| SQLite history | [sqlite-history](https://github.com/simonw/sqlite-history) (Apache-2.0, early, trigger-based, no actor) | Explicit version rows in the write transaction |
| Files as source of truth | [basic-memory](https://github.com/basicmachines-co/basic-memory) (AGPL-3.0; Markdown truth plus SQLite index, no local history) | SQLite stays the source of truth; the files are a generated mirror |
| Current code and the dump | `memory.py`, `model.rs`, `mcp_server.py`, the hook, the task prompt, d125894 | §1 |

Unverified, and not relied on: whether the scheduler follows a symlinked prompt, and Codex MCP resource support.
