# Shared memory service

A Rust HTTP service that owns the memory database. It is the only writer and the only recall renderer, and it keeps the read-only mirror. It runs under systemd on this PC at **127.0.0.1:8787**. SQLite is bundled, so the same source builds on a 64-bit Raspberry Pi. `client.py` (`agent-memory-client`, standard library only) and `mcp_server.py` (the stdio MCP adapter) talk to it over HTTP.

## Build and test

```sh
cargo test --locked --manifest-path service/Cargo.toml
cargo clippy --locked --manifest-path service/Cargo.toml --all-targets -- -D warnings
cargo build --release --locked --manifest-path service/Cargo.toml
python3 -m unittest -v test_memory.py service/tests/test_client.py service/tests/test_install.py
uv venv service/.venv-mcp && uv pip sync --python service/.venv-mcp/bin/python service/mcp-requirements.txt
service/.venv-mcp/bin/python -m unittest -v service.tests.test_mcp
```

The Python suites start the release binary on a temporary store through `service/tests/harness.py`. They never touch the live store, `~/.config/agent-memory` or the running service.

## Install on this machine

```sh
python3 service/install-user.py --database PATH   # first install only; later runs read it from server.toml; add --migrate --pin SLUG… once for a v1 database
python3 service/install-mcp.py                    # MCP adapter for Codex and Claude Code (user scope)
ln -sfn "$PWD/hooks/agent-memory-recall.py" ~/.claude/hooks/agent-memory-recall.py
ln -sfn "$PWD/skill/dream-memory" ~/.claude/skills/dream-memory
ln -sfn "$PWD/skill/dream-memory" ~/.agents/skills/dream-memory
systemctl --user is-active agent-memory && agent-memory-client status
```

`update-local.sh` runs these steps after a merge, and stops rather than replace a skill folder that is not a link. It never migrates.

The package holds code only. Each machine keeps its own facts in the locations below, and its database and logs outside the checkout are never tracked.

| Item | Location |
| --- | --- |
| Server, client | `~/.local/bin/agent-memory-server`, `~/.local/bin/agent-memory-client` |
| Server config and token hashes | `~/.config/agent-memory/server.toml` |
| Session credential, maintenance credential | `~/.config/agent-memory/client.json`, `maintainer.json` (mode 0600) |
| MCP adapter | `~/.local/share/agent-memory-mcp/` |
| User unit | `~/.config/systemd/user/agent-memory.service` |
| Mirror | `<database dir>/mnt/shared-agent-memory/` |

Each token has an explicit list of capabilities: `recall`, `observe`, `review` and `promote`. The session token (`agent-session`, in `client.json`) has `recall` and `observe`; it is what the hook, the loaders and MCP use. The maintenance token (`memory-maintainer`, in `maintainer.json`) has all four; only the dream uses it. Reads of the live store need `recall`. Version history and a dream's output store need `review`. Writes to the live store need `promote`. To keep the service running after logout, run `loginctl enable-linger "$USER"`.

## Client

The client uses `client.json` by default, which allows `status`, `recall`, `ls`, `cat`, `observe` and `useful`. Every other command needs `--config ~/.config/agent-memory/maintainer.json`.

| Command | Does |
| --- | --- |
| `status` | Revision, memory count, pinned characters, pending observations, mirror state |
| `recall [--query Q] [--project DIR] [--role R] [--topic T]… [--client LABEL]` | Prints the start view, or with a query, only the matches. A `DIR` in a Git worktree maps to the main checkout |
| `ls [PREFIX] [--store S]`, `cat PATH\|memver_… [--store S]` | Lists memories; prints a memory or version as its exact bytes |
| `versions [PATH] [--created-by ACTOR] [--store S]` | Version history, newest first |
| `observe PATH FILE` | Records a candidate |
| `useful PATH --why TEXT` | Marks a recalled memory that helped |
| `write PATH FILE [--expected SHA] [--to NEW] [--store S]` | Creates; with `--expected`, updates or renames |
| `rm PATH [--expected SHA] [--store S]`, `redact ID` | Deletes a memory; erases an old version's content (`memver_…`) or an observation's body |
| `dream start [--sessions FILE]`, `dream show [ID] [--diff]`, `dream adopt ID REVIEW.json`, `dream cancel ID` | The dream cycle |

`--store` is `live` or a dream's output store id. Old `--limit`, `--budget` and `--json` flags on `recall` are accepted and ignored. Errors print `Error: …` and exit 1. The client never falls back to another store.

## MCP tools

The MCP host launches `mcp_server.py --config client.json`. The adapter has the session tools only, on the live store: `memory_status`, `memory_recall(query, project)`, `memory_list(path_prefix)`, `memory_read(path)`, `memory_observe(path, content)` and `memory_useful(path, why)`. Pass `project` as the canonical project root, or recall returns no project memories. Recall returns text; every other tool returns the API's JSON as text. There is no maintenance mode: dreams run through the client.

`install-mcp.py` registers it with `codex mcp` and `claude mcp --scope user` (never by editing `~/.claude.json`), leaves a matching entry alone, replaces an older one of its own, refuses an unrelated one, and keeps one copy of the previous Codex config at `runtime/codex-config.toml`. New sessions pick it up; an open one needs `/mcp reconnect`.

A 409 becomes a tool error that names its type and paths and says to re-read and retry. An unreachable service is a tool error, never an empty result. If tools are missing in a desktop app after an install, restart the app.

## HTTP API

Every `/v1` route needs `Authorization: Bearer TOKEN`. `/healthz` is unauthenticated and holds no data. `{store}` is `live` or a store id. Removed request keys are rejected.

| Route | Capability | Body → result |
| --- | --- | --- |
| `GET /v1/status` | recall | `{store_id, revision, memories, pinned_chars, observations_pending, dreams_running, mirror_dir, mirror_stale}` |
| `POST /v1/recall` | recall | `{project?, role?, topics?, query?, client?}` → `{revision, view, pinned, returned, text}` |
| `GET /v1/memory_stores/{store}/memories?path_prefix=` | read | `{data: [{id, path, title, content_sha256, memory_version_id, updated_at}]}` |
| `GET /v1/memory_stores/{store}/memories/{id}` | read | the same, plus `content` |
| `POST /v1/memory_stores/{store}/memories` | write | `{path, content}` → memory |
| `POST /v1/memory_stores/{store}/memories/{id}` | write | `{content?, path?, precondition?: {type: "content_sha256", content_sha256}}` → memory |
| `DELETE /v1/memory_stores/{store}/memories/{id}?expected_content_sha256=` | write | `{type: "memory_deleted", id}` |
| `GET /v1/memory_stores/{store}/memory_versions?memory_id=&created_by=` | review | `{data: [version]}`, newest first, without content |
| `GET /v1/memory_stores/{store}/memory_versions/{id}` | review | version with content |
| `POST /v1/memory_stores/{store}/memory_versions/{id}/redact` | promote | version |
| `POST /v1/observations` | observe | `{path, content}` → `{id}` |
| `POST /v1/feedback` | observe | `{path, why, project?}` → the log line; 404 unless `path` is a live memory, `why` 1–500 characters |
| `POST /v1/observations/{id}/redact` | promote | erases an observation's body everywhere it is stored |
| `POST /v1/dreams`, `GET /v1/dreams`, `GET /v1/dreams/{id}?diff=true` | review | `{sessions: [{source, locator, sha256?}]}` → dream; `{data: [dream]}`; dream (plus `diff` while running) |
| `POST /v1/dreams/{id}/adopt` | promote | `{summary, coverage, conflicts}` → dream |
| `POST /v1/dreams/{id}/cancel` | review | dream |

"read" means `recall` for the live store and `review` for any other store; "write" means `promote` for the live store and `review` for any other store.

Errors:
- 400: invalid input.
- 401 / 403: authentication or capability.
- 404: an unknown object or route.
- 409 `{error, type, paths?}`: `precondition_failed`, `path_exists`, `slug_exists`, `store_archived`, `dream_running` (`paths` names the running dream), `pinned_budget_exceeded`, `adopt_conflict`.
- 413: a body over 4 MiB.

Each recall appends a line to `recall-log.jsonl` next to the database: `ts`, `client` (the request label, else the token name), `project`, `query`, `role`, `topics`, `revision`, `view`, `pinned`, `returned`, `chars`. Each `/v1/feedback` call appends `ts`, `client` (the token name), `project`, `path`, `why`, `revision` and `view: "useful"`; unlike a recall, it fails if the line can't be written. Neither changes memories or recall.

## Backup and moving to the Pi

```sh
umask 077
~/.local/bin/agent-memory-server backup --database /absolute/path/memory.sqlite3 --output /absolute/path/new-backup.sqlite3
```

To move hosts:
1. Stop the old service, make a final backup and transfer it.
2. Build the same locked source natively on the 64-bit Pi.
3. Install it with `deploy/agent-memory.service` and a `server.toml` based on `deploy/server.example.toml`, which shows how to make each token.
4. Start only the new service, then point each client's `url` at it.

HTTP is unencrypted, so on an untrusted LAN use an SSH tunnel or an HTTPS proxy. ARM hardware is untested.
