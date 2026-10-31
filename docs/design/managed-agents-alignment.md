# Managed Agents alignment: design

Status: architect proposal, revision 2 (2026-11-01). Coordinator state: `docs/coordination/managed-agents-rework.md` on branch `claude/managed-agents-memory-dreams-622f97`.

Source docs, read in full on 2026-11-01: [memory](https://platform.claude.com/docs/en/managed-agents/memory), [dreams](https://platform.claude.com/docs/en/managed-agents/dreams), [self-hosted sandbox memory](https://platform.claude.com/docs/en/managed-agents/self-hosted-sandboxes-memory), and the API reference for memory stores, memories, memory versions and dreams.

Revision 2 follows two coordinator updates:

1. **The current code is something to migrate data out of, not a design to preserve.** Retrieval is the worst part.
2. **KISS applies everywhere.** A small core of always-load rules must reach every session, and nothing may silently cut it.

## Summary

Memory becomes a directory of small, path-named Markdown files, following the docs' model:

- **One live store.** Each memory has immutable versions and `content_sha256` preconditions.
- **A read-only mirror** of the live store on disk.
- **Bounded retrieval.**
  - **At session start:** the agent gets the bodies of a few **pinned** rules, guaranteed to fit, and a bounded **index** of one line per memory.
  - **During the task:** it reads files on demand, or runs a thresholded **search** that returns only memories matching the query.
- **Dreams.** A dream clones the live store into an output store, never touches its input, and is adopted by a per-memory merge with preconditions.

One Python implementation owns the store. The Rust HTTP service, its tokens and the Python-compatible number encoding layer are retired; that retirement is the user's decision (§5). MCP itself can serve remote clients later.

## 1. Why recall dumps everything today

These causes were verified by the coordinator and re-checked in `memory.py` `retrieve()` and `service/src/model.rs` `recall()`:

| Cause | Effect |
| --- | --- |
| Every global memory always qualifies; the query only reorders them | With about 12 globals and `limit 12`, every recall returns the whole store |
| `limit` and `budget` cut by rank, silently | The priority-1 `remove-what-is-not-needed` rule was cut from every baseline, so Codex and DeepSeek never received it |
| MCP recall also returns `structuredContent` with every memory's full evidence (`service/mcp_server.py:84`) | About 7.5k characters of text plus about 23k of structured data per call |
| The hook and the task recall load the same set | 2–3 copies per session |
| Recall bodies are the only view; there is no index or file to read on demand | Recall must either dump or hide things |

**What we take from `feat/recall-on-demand` (d125894):**

- MCP recall returns text only.
- Nothing is dropped without being named.
- The hook moves into the repo.
- `update-local.sh` does the reinstall after a merge.
- Topic and role memories are collapsed into on-demand index lines.

**What we don't take:**

- Its `start` mode still loads every global body. It is the same dump.
- Two modes plus `limit`/`budget` knobs add surface without bounding anything.

## 2. Gap table

| Doc concept | Today | Decision | Reason |
| --- | --- | --- | --- |
| A store is a named collection of text documents whose `description` is shown to the agent | One JSON array per revision | **Adopt (one live store)** | `stores` table; the description goes into the mount note. |
| Many small path-addressed memories with a stable `mem_` id across renames | Array element with a slug id; scope as a field | **Adopt** | Scope is encoded in the path (§3.1), so a directory is a scope. The id is stable so history follows a rescope. |
| Content is opaque text; `content_sha256` of the raw bytes | JSON object | **Adapt** | Markdown with JSON frontmatter (§3.2). The bytes are stored as given and the hash covers the metadata. |
| 100 kB per memory, 10,000 memories per store | 4,000-character body | **Adopt** | Cheap limits. "Many small files" goes in the skill. |
| Path rules (≤20 segments, ≤1,024 bytes, no `.`/`..`, NFC) | n/a | **Adapt (subset)** | ASCII segments only, so no normalization code. |
| Create doesn't overwrite; update changes content and/or path | Whole-array replace | **Adopt** | |
| `content_sha256` precondition on update and delete; 409 | Whole-store base check | **Adopt** | A dream is no longer stale because some other memory changed. |
| Immutable versions (`created`, `modified`, `deleted`) with an actor, kept after deletion | Whole-store revisions | **Adopt** | Actors: `cli:<client>`, `mcp`, `dream:<id>`, `migration:revision-<n>`. |
| 30-day version retention | Kept forever | **Reject** | History must survive; the store is tiny. |
| Restore = write an old version's content back | `rollback N` | **Adopt** | Whole-store rollback goes. |
| Redact a non-head version | None | **Adopt** | The only way to remove a leaked secret from history. It is about 20 lines. |
| List with `path_prefix`; `depth` rollups; `view` basic/full; pagination | Recall only | **Adapt** | `list(path_prefix)` returns metadata only and `read(path)` returns content. `depth`, `view` and pagination are knobs a single user doesn't need; the mirror is the browsable view. |
| Store `metadata`; store create, update and delete endpoints | n/a | **Reject** | Nothing consumes them. The live store comes from `init`/`migrate`, and outputs come from dreams. |
| Archive (one-way, read-only) | n/a | **Adapt** | Automatic when a dream ends. There is no endpoint. |
| Store mounted as a directory; the agent reads and writes with file tools; a mount note in the system prompt | Recall text | **Adapt: read via files, write via tools** | Read-only mirror plus a mount note in the index. A writable mount needs the self-hosted sync worker (interval sync, store-wins overwrites, deletion modes), and ordinary sessions must not write accepted memory (next row). |
| `read_only`/`read_write` per attachment; prompt-injection warning | Token capabilities | **Adapt** | Sessions read. Captures go to append-only observations, which are never retrieved. Maintenance writes are explicit tools. On a single-user machine, separate tokens readable by the same user added no real isolation. |
| Up to 8 stores per session; focused stores; per-session `instructions` | One store | **Reject** | Directories give the scoping. There is no per-session attach step. |
| Dream: one store plus 1–100 sessions plus `instructions` → a new output store; inputs never modified | `dream` → `proposal.json` → promote | **Adapt** | Inputs: the live store, 0–100 session locators with hashes (transcripts stay in place), and pending observations. The agent is the pipeline, so `instructions` go in its prompt and are not stored. |
| Status `pending/running/completed/failed/canceled`; cancel; archive dream; `usage`; error types | n/a | **Adapt** | `running` → `completed` (adopted) or `canceled`. There is no queue, no async pipeline, no token use, and no dream archive. |
| Use the output in place of the input; `update_existing` | Promote replaces everything | **Adapt: adopt = per-memory merge** | Changes are applied to the live store as versions, with preconditions against the frozen base. The live id, mirror path and history stay continuous. |

## 3. Target design

### 3.1 Layout and data

The root directory (`/home/sai/Projects/agent-memory` live, a temporary directory in tests) holds:

- **`memory.sqlite3`**: authoritative, schema version 2.
- **`mnt/shared-agent-memory/`**: the read-only mirror, a symlink into `mnt/.generations/<revision>/`.
- **`recall-log.jsonl`**: the usage log.

The legacy tables `revisions`, `state` and `dreams` keep their rows unchanged and are frozen by triggers. `observations` stays in use as is.

**Path layout (frozen).** The path is the scope.

| Path | Scope |
| --- | --- |
| `/global/<slug>.md` | every session |
| `/projects/<root segments…>/<slug>.md` | sessions whose project path is the root (`/` + the middle segments) or below it, compared by whole components |
| `/topics/<topic>/<slug>.md` | on demand |
| `/roles/<role>/<slug>.md` | on demand |

Path rules:

- Segments match `[A-Za-z0-9._-]+` and are not `.` or `..`.
- 2–20 segments, at most 1,024 bytes.
- The slug matches `[a-z0-9][a-z0-9-]{0,79}` (today's id rule, so slugs equal today's ids) and is unique within a store.

**Frozen DDL** (`agent_memory/schema_v2.sql`; only the architect changes it):

```sql
CREATE TABLE stores (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, description TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('live','dream_output')),
  created_at TEXT NOT NULL, archived_at TEXT);
CREATE UNIQUE INDEX one_live_store ON stores(kind) WHERE kind = 'live';
CREATE TABLE memories (
  store_id TEXT NOT NULL REFERENCES stores(id), id TEXT NOT NULL,
  path TEXT NOT NULL, content TEXT NOT NULL, content_sha256 TEXT NOT NULL,
  memory_version_id TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
  PRIMARY KEY (store_id, id), UNIQUE (store_id, path));
CREATE TABLE memory_versions (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE,
  store_id TEXT NOT NULL REFERENCES stores(id), memory_id TEXT NOT NULL,
  operation TEXT NOT NULL CHECK (operation IN ('created','modified','deleted')),
  path TEXT, content TEXT, content_sha256 TEXT,
  created_at TEXT NOT NULL, created_by TEXT NOT NULL, redacted_at TEXT, redacted_by TEXT);
CREATE INDEX versions_by_memory ON memory_versions(store_id, memory_id, seq);
CREATE TABLE dream_jobs (
  id TEXT PRIMARY KEY,
  status TEXT NOT NULL CHECK (status IN ('running','completed','canceled')),
  output_store_id TEXT NOT NULL REFERENCES stores(id),
  input TEXT NOT NULL,      -- frozen JSON: base {memory_id: {path, content_sha256}}, observations, sessions
  adoption TEXT,            -- JSON: summary, coverage, conflicts, added, changed, removed
  created_at TEXT NOT NULL, ended_at TEXT);
CREATE TRIGGER legacy_revisions_frozen BEFORE INSERT ON revisions BEGIN SELECT RAISE(ABORT,'store migrated to v2'); END;
CREATE TRIGGER legacy_dreams_frozen BEFORE INSERT ON dreams BEGIN SELECT RAISE(ABORT,'store migrated to v2'); END;
CREATE TRIGGER legacy_state_frozen BEFORE UPDATE ON state BEGIN SELECT RAISE(ABORT,'store migrated to v2'); END;
PRAGMA user_version = 2;
```

`init` creates the empty v1 tables, applies this file, and creates the live store, so every store has the same shape. The live store's **revision** is `max(seq)` of its versions.

### 3.2 Memory file format (frozen)

`content = "---\n" + <JSON object> + "\n---\n" + <body>`

Frontmatter fields:

| Field | Rule |
| --- | --- |
| `title` | Required; 1–120 characters, one line. It is the memory's index line. |
| `confidence` | Required; `explicit`, `corroborated` or `tentative`. |
| `tags` | Required; list of strings. |
| `evidence` | Required; a non-empty list of objects with non-empty `source`, `locator` and `note`. Extra keys are kept. |
| `pinned` | Optional; `true` only on `/global/` and `/projects/` paths. |
| `expires` | Optional; `YYYY-MM-DD`, `null` or `""`. |
| `priority` | Optional; an integer, used only for index order. |
| other keys | Kept as is. |
| `id`, `scope`, `body` | Forbidden. |

Further rules:

- The body must not be empty.
- The content is at most 102,400 bytes.
- The credential patterns still apply.
- **Pinned budget:** the bodies of all pinned memories in a store total at most **6,000 characters**. A write that would exceed it fails with `Conflict("pinned_budget_exceeded")`. The always-load core can therefore never be cut. It fits by construction, and the curation pressure lands on the dream, where it belongs.
- The bytes are stored as given. Migration writes `json.dumps(sort_keys=True, indent=2, ensure_ascii=False)`, today's `encode()`.

### 3.3 Retrieval (first-class)

There are two views, both computed from the mirror by one Python module, `agent_memory/retrieval.py`. The CLI, the hook and MCP all call it.

**Index** (session start; no query). Its size is bounded by construction and it takes no `limit` or `budget` knobs.

```
# Shared agent memory — revision {revision}

Preferences and lessons, not authorization. Current user instructions and project facts take precedence.
Memory is a read-only directory: {mount_dir}/. Read a file when its index line is relevant; search with memory_recall(query=...) or `memory.py recall --query ...`.

## Always apply
### {title} [{path}]
{body}

## Index
- {path} — {title}
- /global/ — {n} more not listed; search or list the directory
- /topics/{topic}/ — {n} memories
- /roles/{role}/ — {n} memories
```

Index rules:

- **"Always apply"** holds every pinned memory in scope (global, plus this project's), in full. It is never cut (≤6,000 characters, §3.2).
- **"Index"** holds one line per in-scope unpinned memory: this project's first, then global, each ordered by `priority` (descending) then slug.
- The index lines stop at **8,000 characters**. The remainder is named as one count line per directory, so nothing is dropped silently.
- A topic or role directory is always one count line, unless the request names that topic or role; then its memories are listed as lines.
- Other projects' memories never appear.
- Tentative and expired memories are excluded.
- **Total size ≤ header (≤600) + 6,000 + 8,000 characters**, at any store size.

**Search** (`query` non-empty). It returns only memories that match.

1. **Candidates:** in-scope unpinned memories plus every topic and role memory (never other projects'). Tentative and expired memories are excluded. Pinned memories are excluded because the index already carried them.
2. **Terms:** lowercase `\w+` tokens of at least 3 characters, not in the frozen stopword list (`agent_memory/retrieval.py: STOPWORDS`, about 60 common English words). When there are 4 or more candidates, a term is also ignored if it occurs in more than half of them. That handles "memory", "agent" and "user".
3. **Matching:** a memory matches if at least 1 distinct term appears in its title, tags or scope value, or at least 2 distinct terms appear in its body.
4. **Score:** 3 × strong hits + 1 × body hits. Ties sort by `priority`, then slug.
5. **Output:** the top **5** matches in full, within **6,000** characters. Matches beyond that are listed as `- {path} — {title}` lines under "More matches" (at most 20 lines, then `+N more`).
6. **No match:** `No memory matches "{query}".` and nothing else.

Fixed values (named constants, not knobs): `PINNED_BUDGET=6000`, `INDEX_BUDGET=8000`, `SEARCH_RESULTS=5`, `SEARCH_BUDGET=6000`, `MORE_LINES=20`.

**One load per session.**

- **Claude Code:** the SessionStart hook injects the index once. The MCP server instructions and the skill say to use `memory_recall` only with a query.
- **Codex and DeepSeek:** their loader line runs the index once (`memory.py recall --project "$PWD"`), then `--query` when the task needs it.
- MCP returns text only.

**Usage log.** `recall-log.jsonl` keeps its location. Each line has `ts`, `client`, `project`, `query`, `role`, `topics`, `revision`, `kind` (`index` or `search`), `pinned` (paths), `returned` (paths) and `chars`. The daily report's counts by `client` keep working.

**What was checked.**

- Claude Code's own auto memory is this model: a `MEMORY.md` index (first 200 lines or 25 KB) is loaded at start, and topic files are read on demand ([memory docs](https://code.claude.com/docs/en/memory)).
- The Managed Agents docs give the agent a mount note and file tools, with no search API. We add search only because MCP-only and loader-driven agents need it.
- BM25 was considered and rejected. Its scores have no natural threshold, so "unrelated returns nothing" would need a tuned cutoff. A distinct-term rule with a document-frequency filter is deterministic and testable.

### 3.4 Reading and writing

- **Ordinary sessions read.**
  - The hook or loader index.
  - Files in the mirror, via the Read tool, `cat` or `grep`.
  - `memory_recall(query)`.
  - `memory_list` and `memory_read` for MCP-only hosts.
- **They write only observations.** `memory_observe(path, content)` appends a validated candidate, and candidates are never retrieved.
- **Maintenance writes.** The daily dream, or a session acting on an explicit user correction under the standing authorization, uses `memory_write`, `memory_delete`, `memory_redact` and the dream tools.
- **Every write** runs one `BEGIN IMMEDIATE` transaction (memory row plus version row, with the precondition checked inside). It then rebuilds the mirror under `flock(mnt/.lock)`: write `.generations/<revision>/` (files 0444, directories 0555, marker `.agent-memory-store` = `{store_id, name, description, access:"read_only", revision}`), and atomically swap the symlink only if this generation is newer. If the mirror fails, the commit stands and `status` reports `mirror_stale`. Opening the store rebuilds a stale mirror.
- **Writers are multiple processes** (CLI and MCP adapter). SQLite serializes them, as it does today with the 30-second timeout. Preconditions protect read-modify-write across processes and agents.

Research behind the read paths:

- **Codex** reads `AGENTS.md` up to 32 KiB and documents no import syntax ([Codex AGENTS.md](https://developers.openai.com/codex/guides/agents-md)).
- **DeepSeek Harness** does not interpret `@path` imports ([agent instructions](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/context/agent-instructions/README.md)).
- **Claude Code** `@imports` are static and not project-aware ([memory docs](https://code.claude.com/docs/en/memory)).
- **MCP resources** in Claude Code are user `@`-mentions ([Claude Code MCP](https://code.claude.com/docs/en/mcp)). Reads are therefore tools, and loaders and the hook print the index.
- Codex MCP resource support is unverified, and the design doesn't rely on it.

### 3.5 Dreams

1. **`dream_start(sessions=[{source, locator, sha256?}])`** (0–100 sessions, unique locators). In one transaction it:
   - freezes `input` = base snapshot `{memory_id: {path, content_sha256}}`, pending observations (not in any completed v2 dream or promoted legacy dream) and the sessions;
   - creates output store `Dream <id>`;
   - copies every live memory with the same ids (`created` versions, `created_by=dream:<id>`);
   - sets status `running`.

   It returns the dream with its observations.
2. **The agent** reads the mirror, the sessions and the observations, then edits the output store by path with `memory_write` and `memory_delete` (`store=<output id>`).
3. **`dream_get(id, diff=True)`** returns the dream plus `{added, removed, changed}` against the base.
4. **`dream_adopt(id, summary, coverage, conflicts, reviewed)`** keeps today's gates: `reviewed is True`, a non-empty summary and coverage, and every conflict `resolved`. The dream must be `running`. Then, in one transaction:
   - each changed or removed id must still have the base hash and path in the live store;
   - each added path and slug must be free;
   - no observation of this dream may have been consumed by another adopted dream since;
   - the pinned budget is checked on the result.

   Any failure raises `Conflict("adopt_conflict" | "observations_consumed" | "pinned_budget_exceeded", paths)` and nothing changes. On success it applies the changes as live versions (`created_by=dream:<id>`), sets `completed`, archives the output store and rebuilds the mirror. An empty diff is a valid adoption.
5. **`dream_cancel(id)`** turns `running` into `canceled` and archives the output. Canceling a `canceled` dream is a no-op; canceling a `completed` one fails.

On a conflict the skill says: cancel, start a new dream, and carry the edits over once.

### 3.6 Migration from v1

The command is `memory.py migrate [--dry-run] [--pin SLUG]…`. It runs in one exclusive transaction and rolls back on any failure.

1. **Refuse** if the store is already v2, or if `revisions` is not a linear chain (`parent = id − 1`).
2. **Apply the schema and create the live store.**
3. **Replay revisions 1..max**, diffing by v1 id:
   - a new id gets a new `mem_` id and a `created` version;
   - a changed object or scope gets a `modified` version (a scope change is a rename);
   - a missing id gets a `deleted` version;
   - an id that reappears after deletion starts a new lineage.

   Each version gets `created_at` = the revision's `created` and `created_by` = `migration:revision-<n>`. Content uses frontmatter = the v1 object minus `id`, `scope` and `body`.
4. **Apply `--pin` slugs** as `pinned: true` on the final versions (one extra `modified` version, `created_by=migration:pin`). The pinned budget applies.
5. **Check round-trips:** parsing back gives `encode()`-identical v1 objects (ignoring `pinned`), and the final set equals the active revision. Topic, role or project values that don't fit the path rule abort the migration with a list.
6. **Check the legacy tables are untouched:** `iterdump` of `revisions`, `state`, `dreams` and `observations` is unchanged.
7. **Build the mirror.**

`--dry-run` prints the report (slug → path, version counts, pinned characters, offenders) and rolls back.

### 3.7 What's removed

| Removed | Replacement |
| --- | --- |
| The Rust service (`service/src`, Cargo, systemd units, tokens, HTTP API, `client.py`, `install-user.py`, Python-compatible float encoding) | One Python package used by the CLI and the MCP stdio adapter. Remote access later: run the same MCP server with the streamable-http transport (checked in the installed MCP SDK 1.30.0) |
| `proposal.json`, validate/diff/promote, the `reviewed` file flag | The output store, `dream_get(diff=True)`, `dream_adopt` |
| Whole-store rollback | Per-memory restore from versions; undo a dream through its `dream:<id>` versions |
| `export` and `exports/` | The mirror. For a web chat: `memory.py recall > PROFILE.md` |
| Recall `limit`, `budget`, modes, `--json`; MCP `structuredContent` | Bounded index and search (§3.3). Old flags are accepted and ignored so existing loader lines keep working |
| The 4,000-character body cap; the scope field | The 100 kB content limit; the path |

## 4. Workstreams

All PRs target the integration branch **`rework/managed-agents-v2`**, cut from master. Nothing reaches master before cutover, because the standing "run `update-local.sh` after every merge" rule would otherwise reinstall half-built code on the live machine.

Every brief includes these steps:

- copy `graphify-out/` from `/home/sai/Projects/agent-memory/.claude/worktrees/design-managed-agents-alignment-abf130` and use `/graphify`;
- research before relying on an unfamiliar mechanism;
- use temporary stores only.

The test engineer commits the acceptance tests first, and they fail on the integration head. The engineer's PR carries them. Run the tests with `python3 -m unittest discover -s tests -v`; the MCP tests run in the MCP venv.

### 4.1 Frozen interfaces

```python
# agent_memory/content.py   (A)
def validate_path(path: str) -> Scope            # Scope(kind, value, slug); raises Invalid
def parse(content: str) -> Parsed                # Parsed(meta: dict, body: str); raises Invalid
def validate(path: str, content: str) -> Parsed  # path + frontmatter + size + credential rules
def render_legacy(memory: dict) -> tuple[str, str]   # v1 object -> (path, content)    (used by B)
def parse_legacy(path: str, content: str) -> dict    # inverse                          (used by B)

# agent_memory/store.py     (A)
class Invalid(ValueError); class NotFound(KeyError)
class Conflict(Exception): kind: str; paths: list[str]
# kinds: precondition_failed path_exists slug_exists store_archived store_full
#        pinned_budget_exceeded adopt_conflict observations_consumed
def init(root: Path) -> None
class Store:
    @classmethod
    def open(cls, root: Path, readonly: bool = False) -> "Store"   # refuses user_version < 2
    def status(self) -> dict      # {revision, memories, pinned_chars, observations_pending, dreams_running, mirror_dir, mirror_stale}
    def list(self, path_prefix: str = "/", store: str = "live") -> list[dict]    # {id, path, title, content_sha256, memory_version_id, updated_at}
    def read(self, path: str, store: str = "live") -> dict                        # + content
    def write(self, path: str, content: str | None = None, *, expected_sha256: str | None = None,
              new_path: str | None = None, store: str = "live", actor: str) -> dict
        # no expected_sha256 -> create (path must be free); with it -> update content and/or rename
    def delete(self, path: str, *, expected_sha256: str | None = None, store: str = "live", actor: str) -> dict
    def versions(self, path: str | None = None, *, created_by: str | None = None, store: str = "live") -> list[dict]
    def version(self, version_id: str) -> dict
    def redact(self, version_id: str, *, actor: str) -> dict
    def observe(self, path: str, content: str, *, actor: str) -> str
    def dream_start(self, sessions: list[dict] = (), *, actor: str) -> dict
    def dream_get(self, dream_id: str | None = None, *, diff: bool = False) -> dict   # None -> {data: [...]} newest first
    def dream_adopt(self, dream_id: str, review: dict, *, actor: str) -> dict
    def dream_cancel(self, dream_id: str, *, actor: str) -> dict
    def rebuild_mirror(self) -> None
# agent_memory/mirror.py    (A): build(db, root) and the generation/symlink/flock rules of §3.4

# agent_memory/retrieval.py (C)
def load(mirror_dir: Path) -> Mirror             # Mirror(revision, store_name, description, memories: list[Memory])
def index(m: Mirror, project: str = "", role: str = "", topics: Sequence[str] = ()) -> Result
def search(m: Mirror, query: str, project: str = "", role: str = "", topics: Sequence[str] = ()) -> Result
def recall(m: Mirror, query: str = "", **scope) -> Result    # index if not query.strip() else search
# Result(text: str, kind: str, pinned: list[str], returned: list[str])
def log(root: Path, client: str, request: dict, result: Result, revision: int) -> None   # never raises

# agent_memory/migrate.py   (B)
def run(root: Path, *, dry_run: bool = False, pin: Sequence[str] = ()) -> dict    # report
```

**MCP tools** (`agent_memory/mcp_server.py`, D). It is launched as `python -m agent_memory.mcp_server --root DIR [--maintenance]`.

- Read set: `memory_status`, `memory_recall(query="", project="", role="", topics=[])` (text only), `memory_list(path_prefix="/", store="live")`, `memory_read(path, store="live")`, `memory_versions(path=None, created_by=None, store="live")`.
- `--maintenance` adds: `memory_observe`, `memory_write`, `memory_delete`, `memory_redact`, `memory_dream_start`, `memory_dream_get`, `memory_dream_adopt` and `memory_dream_cancel`.
- A `Conflict` becomes a ToolError naming its `kind` and `paths`, plus "re-read and retry".

**CLI** (`memory.py`, D):

- Reads: `recall [--query Q] [--project P] [--role R] [--topic T]… [--client NAME]`; `--limit`, `--budget` and `--json` are accepted and ignored. `recall` reads only the mirror and never opens SQLite.
- Store commands: `status`, `ls [PREFIX]`, `cat PATH` (exact bytes), `versions [PATH]`.
- Writes: `observe PATH FILE`, `write PATH FILE [--expected SHA] [--to NEW]`, `rm PATH [--expected SHA]`, `redact VERSION`.
- Dreams: `dream start [--sessions FILE]`, `dream show [ID] [--diff]`, `dream adopt ID REVIEW.json`, `dream cancel ID`.
- Setup: `init`, `migrate [--dry-run] [--pin SLUG]…`, `backup OUT`. `backup` uses SQLite's backup API, refuses to overwrite and runs an integrity check.
- Every write takes `actor="cli:" + client`.

### 4.2 Workstreams and ownership

| WS | Name | Owns (exclusive; includes deletions) | Needs |
| --- | --- | --- | --- |
| **A** | Store core and mirror | `agent_memory/{__init__,content,store,mirror}.py`, `agent_memory/schema_v2.sql`, `tests/helpers.py`, `tests/test_store.py`, `tests/test_mirror.py` | none |
| **C** | Retrieval | `agent_memory/retrieval.py`, `hooks/agent-memory-recall.py`, `tests/test_retrieval.py`, `tests/test_hook.py`, `tests/fixtures/retrieval/` | `content.parse` (frozen) |
| **B** | Migration, install, retirement | `agent_memory/migrate.py`, `install.py`, `update-local.sh`, `.gitignore`, `tests/legacy.py`, `tests/test_migrate.py`, `tests/test_install.py`. Deletes `service/src`, `service/Cargo.*`, `service/deploy`, `service/tests/service.rs`, `service/install-user.py`, `service/install-mcp.py`, `service/tests/test_install.py` | A's store and content (frozen) |
| **D** | Interfaces: CLI and MCP | `memory.py`, `agent_memory/mcp_server.py`, `agent_memory/mcp-requirements.{in,txt}`, `tests/test_cli.py`, `tests/test_mcp.py`. Deletes `test_memory.py`, `service/mcp_server.py`, `service/client.py`, `service/mcp-requirements.*`, `service/tests/test_mcp.py`, `service/tests/test_client.py` | A, C, `migrate.run` (frozen) |
| **E** | Docs, skill, task prompt | `README.md`, `AGENTS.md`, `skill/dream-memory/SKILL.md`, new `ops/daily-memory-dream.md`, `catalog.py`. Deletes `service/README.md` | the interfaces above |

`install.py` (B) installs the MCP venv from D's requirements, registers `shared_memory` in Codex (as `install-mcp.py` does today, with a config backup), links the hook, and with `--retire-service` stops and disables the `agent-memory` unit (it keeps its files for rollback). `update-local.sh` runs `install.py` without `--retire-service` and never migrates.

Merge order into the integration branch: **A → C, B (parallel) → D → E.** Everyone develops from the start against the frozen interfaces.

**Shared helpers, each built once:**

- `tests/helpers.py` (A):
  - `temp_root()` makes an initialized root with its mirror;
  - `content(title, body, **meta)` builds valid file content;
  - `seed(store, {path: content})`;
  - `race(n, fn)` is the **precondition harness**: `n` processes open the same root, wait on a barrier, call `fn(store, i)`, and the per-process outcomes are returned.
- `tests/legacy.py` (B): `v1_root(revisions, observations=(), dreams=())` builds a real v1 database with history using the v1 DDL.
- `tests/fixtures/retrieval/` (C): a realistic synthetic store of about 30 memories, modeled on the shape of today's live set without copying it, plus golden texts.

### 4.3 Acceptance tests (each fails on the integration head before its fix)

**A, `tests/test_store.py` and `tests/test_mirror.py`**

1. `init_v2_one_live_store`: `user_version = 2`, one live store, revision 0. `open` on a v1 root fails and names `migrate`.
2. `path_rules`: the four layouts are accepted. Each of these is rejected: `..`, an empty segment, 21 segments, 1,025 bytes, a non-ASCII segment, an unknown top level, a non-`.md` name, a bad slug.
3. `content_rules`: each of these is rejected: no frontmatter, empty evidence, bad confidence, an `id`/`scope`/`body` key, a 121-character title, 102,401 bytes, an `api_key=` pattern, an empty body, `pinned` on a `/topics/` path.
4. `sha_is_raw_bytes`: non-canonical JSON spacing plus Burmese text round-trips byte-exact, and the hash equals `hashlib.sha256(bytes)`.
5. `create_conflicts`: same path → `path_exists`; same slug elsewhere → `slug_exists`.
6. `update_precondition`: stale → `precondition_failed` with no new version; matching → new version id; rename keeps the id and the version history shows both paths.
7. `concurrent_writers_one_winner` (`race(8)`, same expected hash): exactly 1 success, 7 `precondition_failed`, and the versions grow by 1.
8. `delete_and_redact`: delete adds a `deleted` version and keeps the lineage. Redacting the head fails; an old version gets null content, path and hash, the redaction fields set, and `created_by` kept.
9. `limits`: the 10,001st memory → `store_full`.
10. `pinned_budget_enforced`: pinning past 6,000 total body characters → `pinned_budget_exceeded` and nothing written. The same check applies at adoption.
11. `dream_start_freezes_and_clones`: same ids, paths and hashes; live untouched; pending observations included; 101 sessions or a duplicate locator rejected.
12. `dream_never_modifies_input`.
13. `adopt_merges`: added, changed (including a rename) and removed memories are applied with `created_by=dream:<id>`; status `completed`; output writes → `store_archived`.
14. `adopt_conflict_per_memory`: a live edit to X plus a dream change to X → `adopt_conflict` with paths `[X]`, nothing changed, still `running`. A live edit to X plus a dream change to Y succeeds and keeps the live X. A path added in both the live store and the dream → conflict.
15. `adopt_review_gates` and `observations_consumed_once`.
16. `cancel_semantics`.
17. `mirror_tracks_live`: files equal the stored bytes, mode 0444 in 0555 directories, and the marker revision equals `status.revision` after each write type. Output stores are not mirrored.
18. `mirror_swap_atomic`: a reader resolving the symlink during 200 writes from 2 processes always sees a complete generation, and the revision never goes backwards.
19. `mirror_failure_keeps_commit`: with an unwritable `mnt`, the write succeeds and `mirror_stale` is true; reopening rebuilds the mirror.

**C, `tests/test_retrieval.py` and `tests/test_hook.py`**

1. `pinned_core_always_present`: with the fixture plus 5,000 extra memories, every pinned in-scope memory (including `/global/keep-it-simple.md`) appears in full in the index for any project, role and topic. Old `limit=1, budget=300` arguments passed through the CLI change nothing.
2. `index_size_bounded`: stores of 30, 500 and 5,000 memories all give an index of at most 600 + 6,000 + 8,000 characters. The count lines add up exactly to the unlisted memories.
3. `index_has_no_unpinned_bodies`.
4. `index_scope`: another project's memories are absent; topic and role directories are count lines; a requested topic is listed line by line.
5. `unrelated_query_returns_nothing`: on the fixture, "kubernetes ingress yaml" → exactly `No memory matches "kubernetes ingress yaml".`
6. `common_and_stop_words_ignored`: "the memory agent" → no match, since those words occur in more than half of the fixture.
7. `match_rule`: one term in the title matches; one term only in the body doesn't; two distinct body terms match.
8. `search_bounded`: 300 matching memories give at most 5 bodies, at most 6,000 characters plus ≤20 "More matches" lines plus `+N more` with the exact N.
9. `search_finds_topic_by_query`: "raspberry usb power" finds `/topics/hardware/…` without `--topic`.
10. `excluded_kinds`: tentative, expired and pinned memories never appear in search; tentative and expired never in the index.
11. `golden_texts`: the index and search output equal `tests/fixtures/retrieval/*.txt`.
12. `log_entry`: fields per §3.3; an unwritable log is silently skipped.
13. `hook_injects_index_once`: for a worktree `cwd` it resolves the canonical root, `additionalContext` equals the index plus the hook note, it makes one log entry, and it fails open (no output, exit 0) with no mirror.

**B, `tests/test_migrate.py` and `tests/test_install.py`**

1. `replays_history`: r1 creates a and b; r2 modifies a and rescopes b; r3 deletes a; r4 restores r2. The version sequences, timestamps and `created_by` match §3.6, and a gets a new lineage at r4.
2. `round_trip_exact`: Unicode, extra evidence keys, integers above 2⁶⁴, floats, `expires: null`, `priority` and `updated` all survive.
3. `legacy_preserved_and_frozen`: `iterdump` is equal; legacy inserts and updates abort.
4. `atomic_on_offender`: topic `"machine learning"` aborts with that value listed, and the database is byte-identical.
5. `dry_run_changes_nothing`. `second_run_refuses`.
6. `pin_applies_and_respects_budget`: `--pin keep-it-simple` → pinned in the mirror and in the index; pins over 6,000 characters abort.
7. `pending_observations_carry_over`.
8. `migrate_builds_mirror`.
9. `install_registers_mcp_and_links_hook` (temporary HOME and fake `codex`/`systemctl` on PATH), with the Codex config backed up.
10. `retire_service_only_with_flag`: `update-local.sh` contains no `--retire-service` and no `migrate`.

**D, `tests/test_cli.py` and `tests/test_mcp.py`**

1. `cli_recall_reads_mirror_only`: works with `memory.sqlite3` absent and a read-only mirror. Its output equals `retrieval.recall`.
2. `cli_roundtrip`: `ls`, `cat` (exact bytes), `write --expected`, `--to`, `rm`, `redact`, `dream start/show --diff/adopt/cancel`, `backup` (refuses an existing output; integrity ok).
3. `mcp_read_catalog`: exactly the 5 read tools; recall has no `structuredContent`, and its text equals `retrieval.recall`.
4. `mcp_maintenance_catalog`: exactly 13 tools, and no v1 names.
5. `mcp_conflict_is_tool_error`: a stale write → ToolError naming `precondition_failed` and "re-read".
6. `mcp_dream_workflow`: start, write the output, diff, adopt; then search finds the new memory and the mirror holds it.
7. `read_mode_cannot_write`: without `--maintenance`, the write tools are absent.

**E** has no code tests. The coordinator checks the skill, README and task prompt against §3 and §4.1 during review. The task prompt covers:

1. catalog;
2. observe;
3. `dream start` with the changed sessions;
4. edit by path;
5. `show --diff`;
6. adopt;
7. on a conflict, cancel and redo once;
8. report (with the pinned characters used out of 6,000).

## 5. Rollout

1. **Optional quick win on master now:** a one-line PR to the current `service/mcp_server.py` that drops `structuredContent` from `memory_recall`, saving about 23k characters a call. `feat/recall-on-demand` is not merged; §1 says what we take from it.
2. **Build:** cut `rework/managed-agents-v2` and run A–E as in §4.2.
3. **Combined check** on the integration head: all tests, then `memory.py migrate --dry-run --pin keep-it-simple` on a copy of a fresh backup of the live store. The report is reviewed against the current memories. The pinned set is settled here: `keep-it-simple` plus whatever else fits the budget, as chosen by the coordinator with the architect. Then a fresh-eyes review and triage.
4. **Cutover** (the coordinator gets the user's go in chat first):
   1. Record the master SHA.
   2. Merge to master.
   3. Run `update-local.sh`.
   4. Stop the `agent-memory` service.
   5. `memory.py backup runtime/pre-v2-<date>.sqlite3`.
   6. `memory.py migrate --pin …`.
   7. `python3 install.py --retire-service`.
   8. Verify: `status`, the hook output in a new Claude session (pinned core present), `memory_recall` over MCP after an app restart, and `memory.py recall` from Codex and DeepSeek.
   9. Install `ops/daily-memory-dream.md` as the task prompt. Copy it; following a symlink is unverified.
   10. Update the memory sections of the user's instruction files (`~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md`, `~/.dsh/AGENTS.md`): index once at start, `--query` when needed, writes through the maintenance tools. These are the user's files, so this needs their go.
5. **Rollback:**
   1. Restore the backup over `memory.sqlite3` (keep the migrated copy aside).
   2. Check out the recorded master SHA.
   3. Re-enable the service with the old `install-user.py`.
   4. Restore the old task prompt and the old instruction-file sections.

   Observations written after the cutover remain in the set-aside copy; re-observe them by hand.

## 6. Question for the user

**Retire the Rust service?** Today a systemd service on 127.0.0.1:8787 owns the database behind an HTTP API with tokens, and the MCP adapter calls it.

**What retiring it means:** the CLI and the MCP adapter use one Python package directly, and the service, tokens, HTTP client and installers go.

**What is lost:** a network API. If the planned Raspberry Pi move happens, the same MCP server can listen over HTTP instead of stdio. Nothing else is lost: the same user can already read the token files.

**Recommendation: yes.** Keeping it means writing the versioning, preconditions, adoption and mirror twice (Rust and Python) and keeping the Python-compatible float encoding layer, for no current use.

If the answer is no, everything in §3 stands. Workstreams A and D then move into Rust/HTTP, with the same tool names and the same retrieval output.

## 7. What was checked

| Topic | Source | Used for |
| --- | --- | --- |
| Memory stores, versions, preconditions, limits, mount note, archive | Managed Agents memory guide plus 12 API reference pages | §2, §3.1–3.4 |
| Dream inputs, statuses, cancel and archive, `update_existing` | Dreams guide, dreams create/retrieve reference | §2, §3.5 |
| Writable mount costs | Self-hosted sandbox memory page (worker sync every 15 s, store-wins conflicts, marker file, deletion modes) | Read-only mirror; writes via tools |
| Index plus on-demand files | [Claude Code memory](https://code.claude.com/docs/en/memory): `MEMORY.md` first 200 lines or 25 KB at start, topic files on demand; `@imports` are static | §3.3 |
| Loader limits | [Codex AGENTS.md](https://developers.openai.com/codex/guides/agents-md) (32 KiB, no imports documented); [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/context/agent-instructions/README.md) (`@path` not interpreted, 64 KiB per chain) | Index printed by the loader command |
| MCP resources vs tools; output cap | [Claude Code MCP](https://code.claude.com/docs/en/mcp): resources are user `@`-mentions; tool output warning at 10k tokens | Tools; small text-only results |
| MCP over HTTP | Installed MCP SDK 1.30.0: FastMCP supports `streamable-http` | Remote access without a custom service |
| SQLite history | [sqlite-history](https://github.com/simonw/sqlite-history) (Apache-2.0, early, triggers, no actor) | Explicit version rows in the write transaction |
| Files as truth | [basic-memory](https://github.com/basicmachines-co/basic-memory) (AGPL-3.0; Markdown truth plus SQLite index, no local history) | SQLite stays the truth; files are a generated mirror |
| Current code and the dump | `memory.py`, `model.rs`, `mcp_server.py`, the hook, the task prompt, `feat/recall-on-demand` d125894 | §1 |

Unverified items (the design relies on neither):

- whether the desktop scheduler follows a symlinked task prompt;
- Codex MCP resource support.
