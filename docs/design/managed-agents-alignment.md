# Managed Agents alignment: design

Status: proposed by the architect, 2026-11-01. Coordinator state: `docs/coordination/managed-agents-rework.md` on branch `claude/managed-agents-memory-dreams-622f97`.

Source docs, read in full on 2026-11-01: [memory](https://platform.claude.com/docs/en/managed-agents/memory), [dreams](https://platform.claude.com/docs/en/managed-agents/dreams), [self-hosted sandbox memory](https://platform.claude.com/docs/en/managed-agents/self-hosted-sandboxes-memory), and the API reference pages for memory stores (create, update, list, archive), memories (create, update, list, retrieve, delete), memory versions (list, retrieve, redact) and dreams (create, retrieve).

## Summary

Today the store holds one JSON array per accepted revision. A dream copies that array into an editable `proposal.json`, and promotion replaces the array as a whole. The rework moves to the Managed Agents model:

- **Many small, path-addressed text memories.** Each memory's scope is encoded in its path, for example `/global/<slug>.md` or `/topics/hardware/<slug>.md`.
- **Immutable versions for every memory.** Writes carry `content_sha256` preconditions.
- **A read-only directory mirror of the live store.** Agents read it with ordinary file tools, and recall output carries a mount note.
- **Dreams as jobs.** A dream freezes its input, clones the live store into a separate output store, and never touches its input. It is adopted by a per-memory, precondition-checked merge.

The Rust service becomes the only writer of the database. `memory.py` becomes a read-only reader of the mirror. Everything that only existed to support whole-array revisions goes: proposals, whole-store promote and rollback, and export.

## 1. Gap table

| Doc concept | Current implementation | Decision | Reason |
| --- | --- | --- | --- |
| Memory store: a named collection of text documents with a `description` shown to the agent | One implicit store; `revisions.body` is a JSON array | **Adopt** | A `stores` table with name and description. The live store's description goes into the mount note. |
| Many small path-addressed memories (`/a/b.md`), case-sensitive, unique per store, with a stable `mem_…` id across renames | A memory is an array element with a slug `id`; scope is a field | **Adopt** | Each memory is a row with `path` and `content`. The scope moves into the path (§2.1), so prefix listing doubles as scope filtering. The id stays stable across renames, so history follows a rescope. |
| Content is opaque UTF-8 text; `content_sha256` is the SHA-256 of the raw bytes with no normalization | Structured JSON object | **Adapt** | Content is Markdown with JSON frontmatter (§2.2). The hash covers the exact bytes, metadata included, so a precondition guards every field. The server validates the frontmatter but stores the bytes unchanged. |
| 100 kB per memory, 10,000 memories per store | 4,000-character body | **Adopt** | 102,400-byte content and 10,000 memories per store. The 4,000-character body cap goes; the skill asks for small focused files instead. |
| Path rules: 1–20 segments, no empty, `.` or `..` segments, NFC, ≤1,024 bytes | n/a | **Adapt (subset)** | Segments are limited to ASCII `[A-Za-z0-9._-]`. That is a strict subset of the doc rules, so no Unicode normalization dependency is needed in Rust or Python. |
| Create doesn't overwrite; update can change content and/or path (rename) | Whole-array replace | **Adopt** | Create returns 409 if the path exists. Update takes optional `content`, `path` and `precondition`. |
| `content_sha256` precondition on update; `expected_content_sha256` on delete; 409 on mismatch | Whole-store base-revision check | **Adopt** | Gives per-memory optimistic concurrency, so an unrelated change no longer makes a whole dream stale. |
| Immutable memory versions (`created`, `modified`, `deleted`), `created_by` actor, kept after the memory is deleted | Whole-store revisions with `reason` | **Adopt** | A `memory_versions` table written in the same transaction as each change. The actor is `token:<name>`, `dream:<id>` or `migration:revision-<n>`. |
| Version retention of 30 days | All revisions kept forever | **Reject** | Local, small and single-user. The constraint is that history must survive, so versions are kept forever. |
| Point-in-time restore = read an old version and write its content back (no restore endpoint) | `rollback N` restores a whole revision | **Adopt** | `rollback` goes. Restoring a memory means reading the version and updating with a precondition. Undoing a dream means restoring each memory whose versions have `created_by=dream:<id>` (the skill documents this). |
| Redact a non-head version (content, path, size and hash become null; the audit fields stay) | None; history is permanent | **Adopt** | One small endpoint. Without it, a secret that slipped past the credential check would stay in history forever. The head of a live memory can't be redacted. |
| List with `path_prefix` (ends with `/`, whole segments) and `depth` 0/1 with `memory_prefix` rollups; `view` `basic`/`full` | Recall only | **Adopt** | The on-demand index is exactly a `depth=1` listing of `/topics/` and `/roles/`. |
| Pagination (`limit`, `page`) | n/a | **Reject** | At most 10,000 small rows and one user. A list returns everything, and `basic` view keeps it small. Revisit above 1,000 memories. |
| Store `metadata` map; store create, update and delete endpoints | n/a | **Reject** | Nothing consumes metadata. The live store is created by `init`/`migrate`, and dream output stores are created by dreams. Nothing is ever hard-deleted locally. |
| Archive a store: one-way, read-only, can't be attached | n/a | **Adapt** | Archiving happens automatically when a dream is adopted or canceled. There is no public endpoint. Writes to an archived store return 409 `store_archived`. |
| Store mounted as a directory; the agent reads and writes with file tools; a mount note (name, path, access, description) is added to the system prompt | Recall text only | **Adapt: read via files, write via API** | The service keeps a read-only mirror of the live store at `<mount>/<slug>/`. Recall text carries the mount note. Writes go through the API or MCP with preconditions. The self-hosted sync semantics (interval sync, store-wins conflicts, deletion modes, marker files) show what a writable mount costs, and ordinary sessions must not write accepted memory anyway (next row). |
| Access `read_only`/`read_write` per attachment; prompt-injection warning about write access | Token capabilities: recall, observe, review, promote | **Adapt** | Capabilities stay as they are, with no config churn. Ordinary sessions read. Session captures go to append-only observations, which are never recalled. Only `promote` writes the live store. The mirror is file mode 0444 inside 0555 directories. |
| Up to 8 stores per session, focused stores | One store | **Reject** | One user, one recall. Path prefixes give project, topic and role separation. The only extra stores are dream outputs. |
| Per-session `instructions` (≤4,096 characters) on attach | n/a | **Reject** | There is no per-session attach step. The hook and loaders are static. |
| Dream: async job; inputs are one store, 1–100 sessions and optional `instructions` | `dream` freezes the revision plus observations into `input.json`/`proposal.json` | **Adapt** | `POST /v1/dreams` freezes a base snapshot, the pending observations and 0–100 session locators (transcripts are never copied, only locator and hash), clones the live store into an output store, and records the instructions. The agent does the synthesis. |
| Dream never modifies inputs; the output is a separate store | The proposal is separate; `input.json` is immutable | **Adopt** | The output store is an ordinary store. The agent edits it through the memory API. |
| Status `pending/running/completed/failed/canceled` | n/a | **Adapt** | `running` (from creation; there is no queue), `completed` (= adopted) and `canceled`. `pending` and `failed` need an async pipeline, which doesn't exist locally. An abandoned dream stays `running` until canceled. |
| Use the output by attaching it in place of the input; discard by archive or delete | Promote replaces the revision | **Adapt: adopt = per-memory merge** | Adoption applies the output's changes to the live store as new versions, with per-memory preconditions against the base snapshot (§2.5). The live store id, mount path and history therefore stay continuous, and only true conflicts block. The output store is then archived. |
| `output_behavior: update_existing` (API reference only) | n/a | **Reject** | It writes without a review gate. Adopt-by-merge gives the same end state after review. |
| Cancel (idempotent on `canceled`, 400 on `completed`) | n/a | **Adopt** | As in the docs. |
| Archive or list dreams, `usage`, `model`, `session_id`, error types | n/a | **Reject** (list kept) | No model run, no tokens, no pipeline failures. `GET /v1/dreams` lists newest first. |
| Memory store as the only session state; transcripts are the dream input | Observations hold candidates; `catalog.py` inventories transcripts | **Keep** | Observations are the append-only inbox the injection warning argues for. `catalog.py` supplies session locators and hashes. |

## 2. Target design

### 2.1 Data model and layout

All memories live in SQLite schema version 2 (`PRAGMA user_version = 2`). The legacy tables `revisions`, `state` and `dreams` stay byte-for-byte and are frozen by triggers. `observations` stays in use, unchanged.

Frozen DDL. Every workstream that needs it adds this file verbatim; identical add/add merges cleanly, and only the architect changes it:

```sql
-- service/src/schema_v2.sql
CREATE TABLE stores (
  id TEXT PRIMARY KEY,                       -- memstore_<32 hex>
  name TEXT NOT NULL, description TEXT NOT NULL DEFAULT '',
  kind TEXT NOT NULL CHECK (kind IN ('live','dream_output')),
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL, archived_at TEXT);
CREATE UNIQUE INDEX one_live_store ON stores(kind) WHERE kind = 'live';
CREATE TABLE memories (
  store_id TEXT NOT NULL REFERENCES stores(id),
  id TEXT NOT NULL,                          -- mem_<32 hex>; same id in a dream's output store
  path TEXT NOT NULL, content TEXT NOT NULL,
  content_sha256 TEXT NOT NULL, content_size_bytes INTEGER NOT NULL,
  memory_version_id TEXT NOT NULL,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
  PRIMARY KEY (store_id, id), UNIQUE (store_id, path));
CREATE TABLE memory_versions (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,     -- store "revision" = max(seq) for that store
  id TEXT NOT NULL UNIQUE,                   -- memver_<32 hex>
  store_id TEXT NOT NULL REFERENCES stores(id), memory_id TEXT NOT NULL,
  operation TEXT NOT NULL CHECK (operation IN ('created','modified','deleted')),
  path TEXT, content TEXT, content_sha256 TEXT, content_size_bytes INTEGER,
  created_at TEXT NOT NULL, created_by TEXT NOT NULL,
  redacted_at TEXT, redacted_by TEXT);
CREATE INDEX versions_by_memory ON memory_versions(store_id, memory_id, seq);
CREATE TABLE dream_jobs (
  id TEXT PRIMARY KEY,                       -- drm_<32 hex>
  status TEXT NOT NULL CHECK (status IN ('running','completed','canceled')),
  input_store_id TEXT NOT NULL REFERENCES stores(id),
  output_store_id TEXT NOT NULL REFERENCES stores(id),
  input TEXT NOT NULL,                       -- frozen JSON: base, observations, sessions, instructions
  adoption TEXT,                             -- JSON: summary, coverage, conflicts, counts
  created_at TEXT NOT NULL, created_by TEXT NOT NULL, ended_at TEXT);
CREATE TRIGGER legacy_revisions_frozen BEFORE INSERT ON revisions
  BEGIN SELECT RAISE(ABORT, 'store migrated to v2; use the v2 API'); END;
CREATE TRIGGER legacy_dreams_frozen BEFORE INSERT ON dreams
  BEGIN SELECT RAISE(ABORT, 'store migrated to v2; use the v2 API'); END;
CREATE TRIGGER legacy_state_frozen BEFORE UPDATE ON state
  BEGIN SELECT RAISE(ABORT, 'store migrated to v2; use the v2 API'); END;
PRAGMA user_version = 2;
```

`init` on a new database creates the v1 tables first (empty, as today) and then applies this file. That keeps one schema shape everywhere.

**Path layout (frozen).** The path decides the scope:

| Path | Scope | Example |
| --- | --- | --- |
| `/global/<slug>.md` | global | `/global/direct-concrete-communication.md` |
| `/projects/<root segments…>/<slug>.md` | project; root = `/` + the segments between `projects` and the file | `/projects/home/sai/Projects/custom-lora-mesh/coordinator-rules.md` |
| `/topics/<topic>/<slug>.md` | topic | `/topics/hardware/pi-usb-power.md` |
| `/roles/<role>/<slug>.md` | role | `/roles/coordinator/merge-gate.md` |

Path rules:

- Starts with `/`; 2–20 segments; at most 1,024 bytes.
- Every segment matches `[A-Za-z0-9._-]+` and is not `.` or `..`.
- The file name is `<slug>.md`, where `slug` matches `[a-z0-9][a-z0-9-]{0,79}` (today's id rule) and is unique within a store across all scopes. The slug is the display id in recall text and in `recall-log.jsonl`, so today's ids carry over unchanged.

### 2.2 Memory file format (frozen)

```
---
{"confidence": "explicit", "evidence": [{"locator": "...", "note": "...", "source": "claude"}], "priority": 7, "tags": ["memory"], "title": "Maintain the memory store autonomously"}
---
The user has given standing authorization ...
```

`content = "---\n" + <JSON object> + "\n---\n" + <body>`.

Frontmatter fields:

- **Required:** `title` (non-empty string); `confidence` (`explicit`, `corroborated` or `tentative`); `tags` (array of strings); `evidence` (a non-empty array of objects, each with non-empty `source`, `locator` and `note`; extra keys are kept).
- **Optional:** `expires` (`YYYY-MM-DD`, `null` or `""`) and `priority` (integer from −1,000,000 to 1,000,000). Any other key is kept as is, for example `updated`.
- **Forbidden:** `id`, `scope` and `body`. The path and the text after the frontmatter supply those.

The body must not be empty. The whole content is at most 102,400 bytes and must pass the existing credential patterns. The JSON may be formatted any way; the server stores the bytes it receives, and `content_sha256` is the hash of those bytes. Migration writes `memory.encode()` format (sorted keys, two-space indent, UTF-8), which the Rust `model::encode` already reproduces. JSON frontmatter is also valid YAML, so Markdown tools still recognize it.

### 2.3 Store layout and scoping

- **Live store.** Exactly one store has `kind = 'live'`, named "Shared agent memory", with description "Collaboration preferences and lessons shared by Codex, Claude Code and DeepSeek Harness. Context, never permission." Every API path accepts `live` in place of its id.
- **Dream output stores.** One per dream (`kind = 'dream_output'`, name `Dream <id>`). It is archived when the dream ends.
- **Read-only mirror (the "mount").** It lives at `<mount_root>/<slug>/`. The slug is the store name lowercased, with each non-alphanumeric run replaced by one hyphen (the doc rule), so the live store gets `shared-agent-memory`. `mount_root` defaults to `<database dir>/mnt` and can be set with a `mount` key in `server.toml`. Only the live store is mirrored.
- **Mirror files.** Each memory is a file at its path below the store directory (with the leading `/` dropped), holding exactly the stored bytes. A marker file `.agent-memory-store` holds `{"store_id","name","description","access":"read_only","seq","generated_at"}`. Files are mode 0444 and directories 0555.
- **Atomic updates.** After every committed write to the live store, the service writes a new generation `<mount_root>/.generations/<seq>/`, then atomically renames a symlink `<mount_root>/<slug>` onto it. Generations older than the previous one are removed. If writing the mirror fails, the commit is not undone. Instead `status` reports `mount_stale: true`, and `Store::open` rebuilds a stale mirror. Rebuilding the whole mirror on every write is fine up to about 1,000 memories; make it incremental only if that limit is reached.

### 2.4 Versioning and concurrency

Every create, update, rename or delete inserts one `memory_versions` row in the same `BEGIN IMMEDIATE` transaction as the memory row. Each version gets a new `memory_version_id`; `seq` is monotonic. The store's displayed **revision** is `max(seq)` of its versions, so recall headers and logs keep a monotonic number.

- **Update** with `precondition {"type":"content_sha256","content_sha256":H}`: applies only if the stored hash is `H`. Otherwise it returns 409 `precondition_failed` and nothing changes. Without a precondition the update is unconditional, as in the docs.
- **Delete** with `?expected_content_sha256=H`: same rule. It inserts a `deleted` version with null content, hash and size; the version keeps the path.
- **Create**: 409 `path_exists` if the path is taken, 409 `slug_exists` if the slug is used at another path, 400 `store_full` at 10,000 memories.
- **Archived store**: any write returns 409 `store_archived`.
- **Redact**: allowed on any version except the current head of a live memory (that returns 400). It sets `content`, `path`, `content_sha256` and `content_size_bytes` to null and records `redacted_at` and `redacted_by`. `created_by` stays.

Both the service and the migration are writers, and both use SQLite transactions. The service serializes requests through one connection. Preconditions protect agent-level read-modify-write across requests and across agents.

### 2.5 How agents read and write

| Who | Read | Write |
| --- | --- | --- |
| Any session (Claude Code hook, Codex/DeepSeek loaders, MCP read-only tools) | The SessionStart recall, `memory_recall`, files under the mirror, `memory_list`/`memory_read` | `memory_observe` (append-only candidate; never recalled) |
| Maintenance (daily dream, or a session acting on an explicit user correction under the standing authorization) | Everything above, plus dream outputs and versions | Direct live edits with preconditions (`promote` capability), dreams |

Research behind this split:

- **Claude Code.** Claude Code's own auto memory is this design: a `MEMORY.md` index (the first 200 lines or 25 KB) is loaded at session start, and topic files are read on demand with file tools ([Claude Code memory](https://code.claude.com/docs/en/memory)). The `start` recall plays the role of that index; the mirror holds the topic files.
- **Imports considered.** Claude Code `@path` imports load at launch and are not project-aware, so they can't replace the hook's per-project selection or its usage log.
- **Codex.** Codex reads `AGENTS.md` up to 32 KiB, and its docs describe no import syntax ([Codex AGENTS.md](https://developers.openai.com/codex/guides/agents-md)).
- **DeepSeek Harness.** The docs say `@path` imports "are not interpreted" ([DeepSeek Harness agent instructions](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/context/agent-instructions/README.md)).
- **Result.** The common ground for all three is a pointer that runs recall, plus plain files they can read. That is why the mirror carries no `INDEX.md` and no import.
- **MCP resources vs tools.** In Claude Code, MCP resources are user-referenced `@server:uri` attachments, plus generic list and read tools ([Claude Code MCP](https://code.claude.com/docs/en/mcp)). Tools behave the same in every host, so memory reads are tools. Codex resource support is **unverified**.
- **Writes.** Writes never go through files. A writable mount needs a sync worker; the self-hosted page describes interval sync, store-wins overwrites, marker files and deletion modes. That is a daemon and a conflict policy this store doesn't need.

### 2.6 Dream lifecycle and adoption

1. **Create** (`POST /v1/dreams`, `review`). Body: `{"sessions": [{"source","locator","sha256"?}], "instructions"?}`.
   - Limits: 0–100 sessions with unique locators; `instructions` of 1–4,096 characters.
   - In one transaction: freeze `input = {base: {memory_id: {path, content_sha256, memory_version_id}}, observations: [pending], sessions, instructions}`; create the output store; copy every live memory with the same `id` (a `created` version, `created_by=dream:<id>`); insert `status = 'running'`.
   - **Pending** means not in the input of any completed v2 dream or any promoted legacy dream.
   - The response is the dream object (§3.1) including its observations.
2. **Work.** The agent reads the base through the mirror, reads sessions and observations, and edits the output store with the memory API (`review` is enough for output stores). Editing never touches the live store.
3. **Diff** (`GET /v1/dreams/{id}/diff`, `review`). Compares the output store with the base by memory id. It returns `added`, `removed` and `changed` (before/after path and content).
4. **Adopt** (`POST /v1/dreams/{id}/adopt`, `promote`). Body: `{summary, coverage, conflicts: [{…, status: "resolved"}], reviewed: true}`, with the same gates as today's promote (400 on failure). The dream must be `running`; otherwise 400. Then in one transaction:
   - For each changed or removed id: the live memory must still have the base's hash and path.
   - For each added id: its path and slug must be free in the live store.
   - None of the dream's observations may have been consumed by another completed dream since. Otherwise 409 `observations_consumed`.
   - Any failure returns 409 `adopt_conflict` with `paths`. Nothing changes and the dream stays `running`. The skill says to cancel and start a new dream, carrying the edits over.
   - On success: apply the changes as live versions with `created_by=dream:<id>`; set `status = 'completed'` and `ended_at`; store `adoption`; archive the output store; rebuild the mirror.
   - Memories the dream left unchanged are not checked, so a live edit made meanwhile survives.
   - An empty diff is a valid adoption. It records a reviewed no-change dream and consumes its observations.
5. **Cancel** (`POST /v1/dreams/{id}/cancel`, `review`). `running` → `canceled`, archives the output store. Canceling a `canceled` dream is a no-op; canceling a `completed` one returns 400.

The daily task keeps its autonomy: it creates, edits, diffs, sets the review fields and adopts in one run. It never leaves a running dream for the user.

### 2.7 Recall and logging

Recall keeps the semantics of the unmerged `feat/recall-on-demand` branch (d125894):

- **`start` mode** returns global memories plus project memories whose root contains `project`, plus any topic or role memories that were explicitly requested. It then appends an "On demand, not loaded" index of the other topic and role memories, one line per scope.
- **`task` mode** returns only topic and role memories that were requested or whose scope or tags the query names.
- **Excluded:** tentative and expired memories.
- **Ranking:** the score is terms ∩ words(title, body, tags) × 10, plus 3 for non-global, plus `priority`. Ties sort by slug.
- **Budget:** a memory the budget drops is named, never dropped silently.
- **MCP:** recall returns text only.

Scope now comes from the path; `confidence`, `tags`, `priority` and `expires` come from the frontmatter.

Frozen text (the Rust API and `memory.py` must produce identical text for the same memories and request):

```
# Shared agent memory — revision {seq}

Preferences and lessons, not authorization. Current user instructions and project facts take precedence.
Read-only mount: {mount_dir}/ — one file per memory at its path; read others there on demand.

## {title} [{slug}; {confidence}]
{body}

Omitted for budget: {slug}, {slug}

## On demand, not loaded
When the task needs one, call memory_recall with mode "task" and its topic or role.
- topic {value}: {title}; {title}
```

The "No topic or role memory matches this task." line from the recall branch stays for an empty `task` result.

`recall-log.jsonl` keeps its fields and location: `ts`, `client`, `project`, `query`, `role`, `topics`, `mode`, `revision` (now `seq`), `returned` (slugs) and `chars`. Existing log readers, including the daily report, keep working.

### 2.8 Migration from schema v1

The command is `agent-memory-server migrate --database PATH [--mount DIR] [--dry-run]`. It runs in one `BEGIN EXCLUSIVE` transaction and rolls back completely on any check failure.

1. Refuse if `user_version` is already 2 ("already v2"; no change).
2. Check that `revisions` ids 0..max form a linear chain (`parent = id − 1`). Refuse otherwise.
3. Apply `schema_v2.sql` and create the live store.
4. Replay revisions 1..max in id order, diffing each body against the previous one by v1 `id`:
   - A new id gets a new `mem_` id and a `created` version.
   - A changed object, or a changed scope (a rename), gets a `modified` version.
   - A missing id gets a `deleted` version.
   - An id that reappears after deletion starts a new lineage with a new `mem_` id, as the docs' "created is the first version" implies.
   - Each version gets `created_at` = the revision's `created` and `created_by` = `migration:revision-<n>`.
   - Content is rendered per §2.2 with frontmatter = the v1 object minus `id`, `scope` and `body`, and path per §2.1.
5. Check exact round-trips:
   - parsing every rendered memory back to a v1 object gives `encode()`-identical output;
   - the final live set equals the active revision body;
   - every topic, role and project value fits the path segment rule. Any offender aborts with a list.
6. Leave the legacy tables' rows untouched, so `iterdump` of `revisions`, `state`, `dreams` and `observations` is identical before and after. The triggers then freeze `revisions`, `state` and `dreams`.
7. Generate the mirror.

`--dry-run` runs everything, prints a report (memory → path table, version counts, pending observations, offenders) and rolls back.

**Safety for the live cutover.** The v2 binary refuses to serve a database with `user_version < 2`, with the message "run agent-memory-server migrate". `install-user.py` refuses to install a v2 binary over a v1 database unless it is given `--migrate`. With `--migrate`, it stops the service, makes a `backup` (integrity-checked) under `runtime/`, migrates, installs, starts and verifies `status`. `update-local.sh` never passes `--migrate`.

### 2.9 What's removed

| Removed | Replacement |
| --- | --- |
| `proposal.json`, `/v1/proposals/{validate,diff,promote}`, `memory_validate`/`memory_diff`/`memory_promote`, `reviewed` on files | The dream output store, `/v1/dreams/{id}/diff` and `/adopt` (validation runs on every write) |
| Whole-store `rollback` (CLI, API, MCP) | Per-memory restore from versions; undo a dream through its `created_by=dream:<id>` versions |
| `export`, `exports/revision-N/{memories.json,PROFILE.md,ALL.md}`, `/v1/export`, `memory_export` | The mirror (always current) and `memory_list view=full`. For a web chat: `memory.py recall --budget 100000 > PROFILE.md` |
| Direct SQLite writes from `memory.py` (`init`, `observe`, `dream`, `diff`, `validate`, `promote`, `rollback`, `export`) | `agent-memory-client` and MCP tools. Old subcommands exit 2 and name the replacement command |
| `memory.py` reading SQLite | `memory.py` reads the mirror; Python has no schema knowledge |
| 4,000-character body cap; scope field | 100 kB content; path |
| `dreams/<id>/` directories | Nothing new is written there. Old directories stay on disk, untracked |

## 3. Workstreams

All workstream PRs target an integration branch, **`rework/managed-agents-v2`**, cut from master after the recall branch merges (§4). Nothing reaches master until cutover. That matters because of the standing "run `update-local.sh` after every merge to master" rule: it would otherwise install half-built code on the live machine.

Every brief includes these steps:

- copy `graphify-out/` from `/home/sai/Projects/agent-memory/.claude/worktrees/design-managed-agents-alignment-abf130` and use `/graphify`;
- research before relying on an unfamiliar mechanism;
- use temporary stores only.

The test engineer commits the acceptance tests first, and they must fail on the integration branch head. The engineer's PR carries them.

### 3.1 Frozen contracts

**HTTP API.** Every path takes `{store}` as an id or `live`. The capability is `recall` when the target is the live store and `review` otherwise; the capability for each write is listed per route.

| Method and path | Capability | Notes |
| --- | --- | --- |
| `GET /v1/status` | recall | `{store_id,name,revision,memories,observations_pending,dreams_running,mount_dir,mount_revision,mount_stale}` |
| `POST /v1/recall` | recall | Request: `{project?,project_id?,role?,topics?,query?,mode:"start"\|"task",limit?,budget?}`. Response: `{revision,store_id,memories:[{id,path,slug,title,confidence}],text}` |
| `GET /v1/memory_stores?include_archived=bool` | review | `{data:[store]}` |
| `GET /v1/memory_stores/{store}` | recall (live) / review | store object |
| `GET /v1/memory_stores/{store}/memories?path_prefix=&depth=0\|1&view=basic\|full` | recall (live) / review | `{data:[memory\|memory_prefix]}`. Default view `basic`; no pagination |
| `GET /v1/memory_stores/{store}/memories/{id}?view=` | recall (live) / review | memory object; default view `full` |
| `POST /v1/memory_stores/{store}/memories` | promote (live) / review | `{path,content}` → memory (basic) |
| `POST /v1/memory_stores/{store}/memories/{id}` | promote (live) / review | `{content?,path?,precondition?}` → memory (basic) |
| `DELETE /v1/memory_stores/{store}/memories/{id}?expected_content_sha256=` | promote (live) / review | `{type:"memory_deleted",id}` |
| `GET /v1/memory_stores/{store}/memory_versions?memory_id=&created_by=&operation=&view=` | recall (live) / review | newest first; default view `basic` |
| `GET /v1/memory_stores/{store}/memory_versions/{id}?view=` | recall (live) / review | version object; default view `full` |
| `POST /v1/memory_stores/{store}/memory_versions/{id}/redact` | promote | version object |
| `POST /v1/observations` | observe | `{path,content}`, validated like a memory → `{id}` |
| `POST /v1/dreams` | review | §2.6 → dream |
| `GET /v1/dreams?limit=20` | review | `{data:[dream]}`, newest first |
| `GET /v1/dreams/{id}`, `GET /v1/dreams/{id}/diff` | review | dream; `{added,removed,changed}` |
| `POST /v1/dreams/{id}/adopt` | promote | §2.6 → dream |
| `POST /v1/dreams/{id}/cancel` | review | dream |

Objects follow the docs' shapes. Fields not listed are absent.

```
memory         {type:"memory", id, memory_store_id, path, content|null, content_sha256, content_size_bytes, memory_version_id, created_at, updated_at}
memory_prefix  {type:"memory_prefix", path}
memory_version {type:"memory_version", id, memory_id, memory_store_id, operation, path, content, content_sha256, content_size_bytes, created_at, created_by, redacted_at, redacted_by}
memory_store   {type:"memory_store", id, name, description, kind, revision, mount_dir|null, created_at, updated_at, archived_at}
dream          {type:"dream", id, status, inputs:[{type:"memory_store",memory_store_id},{type:"sessions",sessions:[...]}],
                outputs:[{type:"memory_store",memory_store_id}], instructions, observations:[{id,created,body}],
                created_at, created_by, ended_at, adoption:null|{summary,coverage,conflicts,added,changed,removed}}
```

**Errors.** 400 invalid (including `depth` other than 0/1, a `path_prefix` that doesn't end with `/`, review gates, and cancel/adopt of a terminal dream); 401; 403 capability; 404; 409 with `{"error": msg, "type": "precondition_failed"|"path_exists"|"slug_exists"|"store_archived"|"adopt_conflict"|"observations_consumed", "paths": [...]?}`; 413 for bodies over 4 MiB. Removed v1 routes return 404.

**Rust storage interface (A1 → A2, B).**

```rust
// lib.rs: Error gains Precondition { kind: &'static str, message: String, paths: Vec<String> } → 409
pub struct Actor(pub String);                       // "token:<name>" | "dream:<id>" | "migration:revision-<n>"
pub struct VersionFilter { pub memory_id: Option<String>, pub created_by: Option<String>, pub operation: Option<String> }
impl Store {
    pub fn init(database: &Path) -> Result<()>;
    pub fn open(database: &Path, mount_root: &Path) -> Result<Self>; // refuses user_version < 2; rebuilds a stale mirror
    pub fn is_live(&self, store: &str) -> Result<bool>;
    pub fn status(&self) -> Result<Value>;
    pub fn recall(&self, request: &Recall) -> Result<Value>;
    pub fn list_stores(&self, include_archived: bool) -> Result<Value>;
    pub fn get_store(&self, store: &str) -> Result<Value>;
    pub fn list_memories(&self, store: &str, path_prefix: Option<&str>, depth: u8, full: bool) -> Result<Value>;
    pub fn get_memory(&self, store: &str, id: &str, full: bool) -> Result<Value>;
    pub fn create_memory(&mut self, store: &str, path: &str, content: &str, actor: &Actor) -> Result<Value>;
    pub fn update_memory(&mut self, store: &str, id: &str, content: Option<&str>, path: Option<&str>,
                         expected_sha256: Option<&str>, actor: &Actor) -> Result<Value>;
    pub fn delete_memory(&mut self, store: &str, id: &str, expected_sha256: Option<&str>, actor: &Actor) -> Result<Value>;
    pub fn list_versions(&self, store: &str, filter: &VersionFilter, full: bool) -> Result<Value>;
    pub fn get_version(&self, store: &str, id: &str, full: bool) -> Result<Value>;
    pub fn redact_version(&mut self, store: &str, id: &str, actor: &Actor) -> Result<Value>;
    pub fn observe(&self, path: &str, content: &str, actor: &Actor) -> Result<Value>;
    pub fn create_dream(&mut self, request: &Value, actor: &Actor) -> Result<Value>;
    pub fn list_dreams(&self, limit: usize) -> Result<Value>;
    pub fn get_dream(&self, id: &str) -> Result<Value>;
    pub fn dream_diff(&self, id: &str) -> Result<Value>;
    pub fn adopt_dream(&mut self, id: &str, review: &Value, actor: &Actor) -> Result<Value>;
    pub fn cancel_dream(&mut self, id: &str, actor: &Actor) -> Result<Value>;
}
// model.rs keeps encode/decode/hash/Document; adds:
pub fn validate_path(path: &str) -> Result<Scope>;          // Scope { kind, value, slug }
pub fn parse_content(content: &str) -> Result<Parsed>;      // Parsed { meta: Value, body: String }
pub fn render_legacy(memory: &Value) -> Result<(String, String)>; // v1 object → (path, content); used by migrate
pub fn parse_legacy(path: &str, content: &str) -> Result<Value>;  // inverse; used by migrate's round-trip check
```

**MCP tools (C).** The adapter works in paths and resolves ids with one list call.

- Read-only set: `memory_status`, `memory_recall` (text only), `memory_list(path_prefix="/", depth=0, store="live", view="basic")`, `memory_read(path, store="live")` (returns content, id, sha and version id), `memory_versions(path?, memory_id?, created_by?, store="live")`.
- The maintenance set adds: `memory_observe(path, content)`, `memory_write(path, content?, expected_sha256?, new_path?, store="live")`, `memory_delete(path, expected_sha256?, store="live")`, `memory_redact(version_id)`, `memory_dream_start(sessions=[], instructions?)`, `memory_dream_get(dream_id?, diff=False)` (no id means list), `memory_dream_adopt(dream_id, summary, coverage, conflicts, reviewed)` and `memory_dream_cancel(dream_id)`.
- `memory_write` without `expected_sha256` creates the memory, and fails if the path exists. With it, it updates the memory.
- On a 409, the error tells the agent to re-read and retry against the fresh state.

**Client CLI (C), `agent-memory-client`.** Subcommands: `status`, `recall` (the current flags plus `--mode`), `ls [PREFIX] [--depth 1] [--store ID] [--full]`, `cat PATH [--store]` (prints exact bytes), `versions [PATH] [--created-by]`, `observe PATH FILE`, `write PATH FILE [--expected SHA] [--store]`, `mv PATH NEW [--expected]`, `rm PATH [--expected]`, `redact VERSION`, `dream start [--sessions FILE] [--instructions TEXT]`, `dream show [ID] [--diff]`, `dream adopt ID REVIEW.json`, `dream cancel ID`.

**`memory.py` (D).**

- Subcommands: `recall` (unchanged flags plus `--mode`, `--client` and `--json`) and `status` (marker contents and file count).
- It finds the single `<root>/mnt/*/.agent-memory-store` and reads `*.md` below it. It never opens SQLite.
- Every other old subcommand prints `moved: use agent-memory-client <cmd>` and exits 2.

### 3.2 Workstreams

| WS | Name | Owns (exclusive) | Depends on |
| --- | --- | --- | --- |
| **A1** | Storage core and mirror | `service/src/{store,model,lib}.rs`, new `service/src/mount.rs`, `service/src/schema_v2.sql`, `service/tests/service.rs` (delete v1-only tests; keep backup/encoding/decoder tests), new `service/tests/store_v2.rs`, new `service/tests/common/mod.rs` | none |
| **A2** | HTTP API and capabilities | `service/src/{api,config}.rs`, new `service/tests/api_v2.rs` | A1's interface (frozen) |
| **B** | Migration, install, cutover tooling | new `service/src/migrate.rs`, `service/src/main.rs`, new `service/tests/migrate.rs`, new `service/tests/legacy/mod.rs`, `service/install-user.py`, `service/tests/test_install.py`, `update-local.sh`, `.gitignore`, `service/deploy/server.example.toml` | A1's `schema_v2.sql`, `render_legacy`/`parse_legacy` (frozen) |
| **C** | MCP adapter and HTTP client | `service/mcp_server.py`, `service/client.py`, `service/install-mcp.py`, `service/tests/test_mcp.py`, `service/tests/test_client.py`, new `service/tests/harness.py`, `service/README.md` | HTTP API (frozen) |
| **D** | Agent surface: reader, hook, skill, docs | `memory.py`, `test_memory.py`, `hooks/agent-memory-recall.py`, `catalog.py`, `skill/dream-memory/SKILL.md`, `README.md`, `AGENTS.md`, new `ops/daily-memory-dream.md` (the repo copy of the scheduled task prompt) | Mirror and file format (frozen); `harness.py` for the parity test |

`lib.rs` is A1's because errors originate in the store. `main.rs` is B's because it adds `migrate` and passes `config.mount` to `Store::open`. A2 adds `mount: Option<PathBuf>` to `Config` in `config.rs`, defaulting to `<database parent>/mnt`; B documents it in `server.example.toml`. A1 also owns the recall golden fixture `service/tests/recall_golden.json` (request → expected text), which D's Python test reads.

**Shared test helpers, each built once:**

- **`service/tests/common/mod.rs` (A1).**
  - `TempStore::new()`: an initialized v2 database with a temporary mount root.
  - `content(title, body)`: builds valid file content.
  - `seed(&mut store, &[(path, content)])`.
  - `race(n, |store, i| …)`: the **precondition harness**. It opens `n` independent `Store`s on one database, starts them together on a barrier, and returns the per-thread results, so a test can assert that exactly one writer wins.
- **`service/tests/legacy/mod.rs` (B).** `v1_store(revisions: &[Value], observations, dreams)` writes a v1 database with real v1 DDL and history.
- **`service/tests/harness.py` (C).** Starts the release server on a temporary database and mount with reader, observer, reviewer and admin tokens. It replaces the setup copies in `test_mcp.py` and `test_client.py`, and D's parity test reuses it.

### 3.3 Acceptance tests (written first; each must fail on the integration branch before the fix)

**A1, `service/tests/store_v2.rs`**

1. `init_creates_v2_with_one_live_store`: `user_version = 2`, exactly one `live` store, `status.revision == 0`.
2. `open_refuses_v1_database`: the error mentions `migrate`.
3. `path_rules`: the four layouts are accepted. Each of these returns 400: no leading `/`, `..`, empty segment, 21 segments, 1,025 bytes, non-ASCII segment, unknown top level, non-`.md` name, bad slug.
4. `content_rules`: each of these returns 400: missing frontmatter, empty evidence, bad confidence, `scope`/`id`/`body` keys, 102,401 bytes, an `api_key=` pattern, empty body.
5. `sha_is_raw_bytes`: the stored content is byte-identical for non-canonical JSON spacing plus Burmese text, and `content_sha256` equals SHA-256 computed in the test.
6. `create_conflicts`: same path → 409 `path_exists`; same slug at another scope → 409 `slug_exists`.
7. `update_precondition`: a stale hash returns 409 `precondition_failed` with no new version; a matching hash creates a new version and a new `memory_version_id`; no precondition updates unconditionally.
8. `concurrent_writers_one_winner` (harness, n = 8, same expected hash): exactly one success, seven `precondition_failed`, version count +1.
9. `rename_keeps_id_and_history`: the id is unchanged, the version is `modified` with the new path, and listing versions by `memory_id` shows both paths.
10. `delete_keeps_history`: a mismatched expected hash returns 409 and keeps the memory; a matching one adds a `deleted` version with null content, and the lineage stays listable.
11. `list_prefix_depth`:
    - `/topics/` with depth 1 returns a `memory_prefix` item `/topics/hardware/`;
    - `/notes/` does not match `/notes-archive/x.md`;
    - a prefix without a trailing `/` returns 400, and so does depth 2;
    - `basic` view has null content with hash and size set.
12. `redact_rules`: the head returns 400; an old version gets null content, path, hash and size, `redacted_*` set, `created_by` kept, and the head is unaffected.
13. `store_full_at_10000`.
14. `dream_create_freezes_and_clones`:
    - the output store has the same ids, paths and hashes;
    - live versions are unchanged;
    - the pending observations are included;
    - status is `running`;
    - 101 sessions, a duplicate locator, or 4,097-character instructions each return 400.
15. `dream_never_modifies_input`: writes to the output store leave the live memories and versions identical.
16. `adopt_merges_with_dream_actor`:
    - added, changed (including a rename) and removed memories are applied to the live store;
    - `created_by = "dream:<id>"`;
    - status is `completed`, `ended_at` is set and `adoption` holds the counts;
    - writes to the output store return 409 `store_archived`.
17. `adopt_conflict_is_per_memory`:
    - a live edit to X after dream start plus a dream change to X returns 409 `adopt_conflict` with `paths = [X]`; the live store is unchanged and the dream still `running`;
    - a dream change to Y with a live edit to X adopts, and keeps the live X.
18. `adopt_added_path_collision`: a path created in the live store after dream start that the dream also adds returns 409.
19. `adopt_review_gates`: `reviewed` false, an empty summary, an empty coverage, or an unresolved conflict each return 400.
20. `observations_consumed_once`: two dreams share an observation. The first adoption succeeds; the second returns 409 `observations_consumed`. A third dream excludes the observation, and an observation made after dream start stays pending.
21. `cancel_semantics`: `running` → `canceled` with the output archived; canceling again is a no-op; canceling a completed dream returns 400; adopting a canceled dream returns 400.
22. `mirror_tracks_live`:
    - after a create, update, rename and delete, the files match the stored bytes, are mode 0444 in 0555 directories, and `marker.seq == status.revision`;
    - deleted and renamed-away paths are gone;
    - output stores are not mirrored.
23. `mirror_swap_atomic`: during 200 writes, a reader thread resolving the symlink always finds a marker, and the file set matches the marker's revision.
24. `mirror_failure_does_not_fail_commit`: with an unwritable mount root, the write succeeds and `mount_stale` is true; reopening with a writable root rebuilds the mirror.
25. `recall_frozen_text`: golden-text check of `start` (with index and an "Omitted for budget" line), `task`, an empty `task`, the mount line, tentative/expired exclusion, and project matching on path components (`/projects/home/a` does not match `/home/ab`).

**A2, `service/tests/api_v2.rs`**

1. `capability_matrix`:
   - reader: allowed live reads, recall and status; 403 on live writes, any dream route, output-store reads and redact;
   - review-only token: allowed to create a dream and write its output; 403 on live writes, adopt and redact;
   - observe token: only `/v1/observations`;
   - promote: allowed live writes, adopt and redact.
2. `live_alias_equals_id`.
3. `error_mapping`: each 409 type returns its `type` and `paths`; 400, 404, and 413 above 4 MiB (a 102,401-byte content returns 400, not 413).
4. `actor_recorded`: a write with token `admin` produces a version with `created_by = "token:admin"`.
5. `removed_routes_404`: `/v1/proposals/promote`, `/v1/rollback`, `/v1/export`.
6. `recall_log_v2`: the entry has `mode`, slug `returned`, `revision == seq` and the token name as `client`.
7. `delete_expected_sha_query`.
8. `http_dream_workflow`: start, edit the output, diff, adopt; the recall text shows the new memory and the mirror holds it.

**B, `service/tests/migrate.rs` and `service/tests/test_install.py`**

1. `replays_history`: fixture r1 creates a and b; r2 modifies a and rescopes b from global to topic; r3 deletes a; r4 is a rollback to r2.
   - a: `created`, `modified`, `deleted`, then a new lineage `created`;
   - b: `created`, then `modified` with the new path;
   - timestamps equal the revisions' `created`, and `created_by = "migration:revision-N"`.
2. `round_trip_exact`: every v1 memory (Unicode, extra evidence keys, integers above 2⁶⁴, floats, `expires: null`, `priority`, `updated`) → content → v1 object is `encode`-identical, and the live set equals the active revision.
3. `legacy_tables_preserved_and_frozen`: `iterdump` of the legacy tables is identical before and after; inserts into `revisions` and `dreams` and updates to `state` abort.
4. `atomic_on_offender`: topic `"machine learning"` aborts with that value listed; `user_version` stays 0 and no v2 tables exist.
5. `dry_run_changes_nothing`: the database is byte-identical and the report lists paths.
6. `second_run_refuses`.
7. `pending_observations_carry_over`: one consumed by a promoted legacy dream is excluded; the unconsumed ones appear in the first v2 dream.
8. `migrate_builds_mirror`.
9. `install_refuses_v1_without_flag`: `install-user.py` without `--migrate` exits non-zero against a v1 database, and the database and service state are restored.
10. `install_migrate_backs_up_first`: with `--migrate`, an integrity-checked backup exists before the migration, and the service is stopped and started.
11. `update_local_never_migrates`: `update-local.sh` contains no `--migrate`.

**C, `service/tests/test_mcp.py` and `service/tests/test_client.py` (against the real server via `harness.py`)**

1. `readonly_catalog`: exactly the five read tools; recall has text content and no `structuredContent`.
2. `maintainer_catalog`: the set adds exactly the eight maintenance tools, and v1 tools (`memory_promote`, `memory_rollback`, `memory_export`, `memory_validate`, `memory_diff`, `memory_dream`, `memory_get_dream`) are absent.
3. `write_by_path`: create; update with the expected hash; a stale hash raises a ToolError mentioning re-reading; rename via `new_path`; delete with the expected hash.
4. `dream_workflow_mcp`: start, write the output, get with diff, adopt; recall shows the result.
5. `reader_cannot_write`: the 403 is surfaced as a ToolError.
6. `offline_is_error_not_empty`.
7. `client_cli_roundtrip`: `ls`, `cat` (exact bytes, hash matches), `write --expected`, `mv`, `rm`, `dream start/show --diff/adopt/cancel`, and `recall --mode task`.

**D, `test_memory.py`**

1. `recall_reads_mirror`: a hand-built mirror fixture per §2.2; `start` returns global plus the matching project; `task` returns the topic.
2. `golden_text`: the Python output equals every case in A1's `service/tests/recall_golden.json`, the same fixture A1 test 25 uses.
3. `no_sqlite_needed`: `recall` works with no `memory.sqlite3` and a read-only mirror, and creates nothing except `recall-log.jsonl` (silently skipped when unwritable).
4. `tentative_expired_excluded`.
5. `recall_log_fields`.
6. `old_subcommands_redirect`: `observe`, `dream`, `promote`, `rollback` and `export` exit 2 and name `agent-memory-client`.
7. `hook_output`: for a worktree `cwd`, the hook uses the canonical root, its `additionalContext` holds the mount line and standing memories, and it fails open (no output, exit 0) without a mirror.
8. `python_rust_parity` (needs the release binary and `harness.py`; skipped otherwise): seed memories over HTTP, then `memory.py recall` on the server's mirror equals `/v1/recall` `text` for the same start and task requests.

D also rewrites `SKILL.md` and `ops/daily-memory-dream.md` for the v2 cycle:

1. catalog;
2. observe;
3. `dream start` with the changed sessions;
4. edit the output store by path;
5. diff;
6. adopt;
7. on a 409, cancel and redo once;
8. report.

The last-dream time comes from `GET /v1/dreams` instead of `sqlite3 … dreams`.

## 4. Rollout

1. **Before the integration branch.** Merge `feat/recall-on-demand` (d125894, v1 schema) to master as its own PR. It fixes about 30,000 characters per MCP recall today and needs no migration. Its owner is the "Daily memory dream autonomous setup" session; it has stopped so as not to clash. Then cut `rework/managed-agents-v2` from master.
2. **Build.** A1, A2, B, C and D work in parallel. Merge them into the integration branch in the order **A1 → A2 → B → C → D**. A2, B and C compile and pass only after A1 lands, and D's parity test needs C's harness.
3. **Combined check** on the integration head:
   - `cargo test --locked`, `cargo clippy -- -D warnings`, a release build, and all Python suites;
   - `migrate --dry-run` against a **copy** of a fresh `agent-memory-server backup` of the live database, with the report reviewed against `memory_export`;
   - a fresh-eyes review campaign and triage.
4. **Live migration** (the coordinator gets the user's go in chat first):
   1. Record the current master SHA in the state file.
   2. Merge the integration branch to master.
   3. Run `update-local.sh`. It builds and links, then stops at the v1 guard.
   4. Run `python3 service/install-user.py --database /home/sai/Projects/agent-memory/memory.sqlite3 --migrate`, which does stop → backup → migrate → install → start.
   5. Verify: `status` (revision = replayed versions, memories = 26), the hook output in a new session, `memory_recall` over MCP, and `memory.py recall`.
   6. Install `ops/daily-memory-dream.md` as the scheduled task's prompt. A symlink from the task directory is **unverified**, so copy it.
   7. Update the loader lines in `~/.codex/AGENTS.md` and `~/.dsh/AGENTS.md` (the recall command is unchanged; only the write instructions change).
5. **Rollback.**
   1. Stop the service.
   2. Move the migrated database aside (keep it).
   3. Restore the backup from step 4.4.
   4. Check out the recorded master SHA in the main checkout and run the old `install-user.py`.
   5. Restore the old task prompt (`runtime/daily-memory-dream.SKILL.pre-*.md`).

   Observations written after the cutover are still in the set-aside database's `observations` table; re-observe them by hand. The v2 binary refuses the restored v1 database, so the old binary must be reinstalled; this is intentional.

## 5. Question for the user

**Should memory writes go only through the running service?** Today `memory.py` can write SQLite directly with no service running. In this design `memory.py` only reads the mirror, and all writes (observations, edits, dreams) go through the systemd service via MCP or `agent-memory-client`. If the service is down, recall still works (mirror and hook), but captures and the daily dream fail with a clear error until it is back. **Recommendation: yes.** Keeping a second writer in Python would double the new versioning, precondition and adoption logic in two languages for no regular benefit; the service is already installed and fronts MCP.

Settled by the architect (engineering):

- versions kept forever;
- no pagination;
- ASCII path segments;
- one live store;
- adopt-by-merge;
- observations kept as the inbox;
- the integration branch;
- the `--migrate` guard.

## 6. What was checked

| Topic | Checked | Outcome |
| --- | --- | --- |
| Managed Agents memory and dreams | Both guides, the self-hosted sync page, 14 API reference pages | §1 and §3.1 shapes and limits come from these. `update_existing` appears only in the API reference |
| Claude Code memory | [Memory docs](https://code.claude.com/docs/en/memory): `@path` imports (four hops, loaded at launch), auto memory (`MEMORY.md` index of 200 lines or 25 KB, topic files on demand) | Index plus on-demand files is the model for `start` recall and the mirror |
| Codex instructions | [AGENTS.md guide](https://developers.openai.com/codex/guides/agents-md): one file per directory, 32 KiB default cap, no import syntax documented | Keep the recall pointer |
| DeepSeek Harness | [Agent instructions README](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/context/agent-instructions/README.md): `@path` not interpreted, 64 KiB per chain in `dsh-base` | Keep the recall pointer |
| MCP resources | [Claude Code MCP](https://code.claude.com/docs/en/mcp): resources are user `@`-mentions plus generic list/read tools; tool output is capped at 25,000 tokens by default | Use tools; Codex resource support unverified |
| SQLite history | [simonw/sqlite-history](https://github.com/simonw/sqlite-history) (Apache-2.0, small and early; trigger shadow tables, no actor field) | Explicit version rows written in the same transaction, because actor attribution and redaction are first-class |
| Files-as-truth stores | [basic-memory](https://github.com/basicmachines-co/basic-memory) (AGPL-3.0, mature; Markdown truth plus SQLite index, no local history) | SQLite stays the truth so versions, preconditions and an atomic multi-memory adoption are possible; files are a generated mirror |
| Existing code | `memory.py`, `catalog.py`, `service/src/*.rs`, `mcp_server.py`, `client.py`, all tests, the hook, the scheduled task prompt, the recall branch diff | Contracts above keep the hook command line, the log fields and token capabilities |

Unverified items:

- whether the desktop scheduler follows a symlinked prompt;
- Codex MCP resource support.

Neither is relied on.
