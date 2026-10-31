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
python3 service/install-user.py --database "$PWD/memory.sqlite3"   # add --migrate --pin SLUG… once for a v1 database
python3 service/install-mcp.py                                     # MCP adapter for Codex and Claude Code
ln -sfn "$PWD/hooks/agent-memory-recall.py" ~/.claude/hooks/agent-memory-recall.py
systemctl --user is-active agent-memory && agent-memory-client status
```

`update-local.sh` runs these steps after a merge. It never migrates.

| Item | Location |
| --- | --- |
| Server, client | `~/.local/bin/agent-memory-server`, `~/.local/bin/agent-memory-client` |
| Server config and token hashes | `~/.config/agent-memory/server.toml` |
| Read credential, maintenance credential | `~/.config/agent-memory/client.json`, `maintainer.json` (mode 0600) |
| MCP adapter | `~/.local/share/agent-memory-mcp/` |
| User unit | `~/.config/systemd/user/agent-memory.service` |
| Mirror | `<database dir>/mnt/shared-agent-memory/`, or `mount` in `server.toml` |

Each token has an explicit list of capabilities: `recall`, `observe`, `review` and `promote`. Reads of the live store need `recall`. A dream's output store needs `review`. Writes to the live store need `promote`. To keep the service running after logout, run `loginctl enable-linger "$USER"`.

## Client

The client uses `client.json` by default. Pass `--config ~/.config/agent-memory/maintainer.json` for writes and dreams.

| Command | Does |
| --- | --- |
| `status` | Revision, memory count, pinned characters, pending observations, mirror state |
| `recall [--query Q] [--project ROOT \| --project-id ID] [--role R] [--topic T]… [--client LABEL]` | Prints the start view, or with a query, only the matches |
| `ls [PREFIX] [--store S]`, `cat PATH\|memver_… [--store S]` | Lists memories; prints a memory or version as its exact bytes |
| `versions [PATH] [--created-by ACTOR] [--store S]` | Version history, newest first |
| `observe PATH FILE` | Records a candidate |
| `write PATH FILE [--expected SHA] [--to NEW] [--store S]` | Creates; with `--expected`, updates or renames |
| `rm PATH [--expected SHA] [--store S]`, `redact VERSION` | Deletes a memory; erases an old version's content |
| `dream start [--sessions FILE]`, `dream show [ID] [--diff]`, `dream adopt ID REVIEW.json`, `dream cancel ID` | The dream cycle |

`--store` is `live` or a dream's output store id. Old `--limit`, `--budget` and `--json` flags on `recall` are accepted and ignored. Errors print `Error: …` and exit 1. The client never falls back to another store.

## MCP tools

The MCP host launches `mcp_server.py --config client.json [--maintenance-config maintainer.json]`. The adapter turns tool calls into authenticated API calls. Tools take paths, and the adapter resolves each path to an id with one list call. Recall returns text. Every other tool returns the API's JSON as text.

| Set | Tools |
| --- | --- |
| Read | `memory_status`, `memory_recall(query, project, role, topics)`, `memory_list(path_prefix, store)`, `memory_read(path, store)`, `memory_versions(path, created_by, store)` |
| Maintenance (with `--maintenance-config`) | `memory_observe`, `memory_write(path, content, expected_sha256, new_path, store)` (no `expected_sha256` means create), `memory_delete`, `memory_redact`, `memory_dream_start`, `memory_dream_get`, `memory_dream_adopt`, `memory_dream_cancel` |

A 409 becomes a tool error that names its type and paths and says to re-read and retry. An unreachable service is a tool error, never an empty result. If tools are missing in a desktop app after an install, restart the app.

## HTTP API

Every `/v1` route needs `Authorization: Bearer TOKEN`. `/healthz` is unauthenticated and holds no data. `{store}` is `live` or a store id.

| Route | Capability | Body → result |
| --- | --- | --- |
| `GET /v1/status` | recall | `{store_id, revision, memories, pinned_chars, observations_pending, dreams_running, mirror_dir, mirror_stale}` |
| `POST /v1/recall` | recall | `{project?, project_id?, role?, topics?, query?, client?}` → `{revision, view, pinned, returned, text}` |
| `GET /v1/memory_stores/{store}/memories?path_prefix=` | read | `{data: [{id, path, title, content_sha256, memory_version_id, updated_at}]}` |
| `GET /v1/memory_stores/{store}/memories/{id}` | read | the same, plus `content` |
| `POST /v1/memory_stores/{store}/memories` | write | `{path, content}` → memory |
| `POST /v1/memory_stores/{store}/memories/{id}` | write | `{content?, path?, precondition?: {type: "content_sha256", content_sha256}}` → memory |
| `DELETE /v1/memory_stores/{store}/memories/{id}?expected_content_sha256=` | write | `{type: "memory_deleted", id}` |
| `GET /v1/memory_stores/{store}/memory_versions?memory_id=&created_by=` | read | `{data: [version]}`, newest first, without content |
| `GET /v1/memory_stores/{store}/memory_versions/{id}` | read | version with content |
| `POST /v1/memory_stores/{store}/memory_versions/{id}/redact` | promote | version |
| `POST /v1/observations` | observe | `{path, content}` → `{id}` |
| `POST /v1/dreams`, `GET /v1/dreams`, `GET /v1/dreams/{id}?diff=true` | review | `{sessions}` → dream; `{data: [dream]}`; dream (plus `diff`) |
| `POST /v1/dreams/{id}/adopt` | promote | `{summary, coverage, conflicts, reviewed}` → dream |
| `POST /v1/dreams/{id}/cancel` | review | dream |

"read" means `recall` for the live store and `review` for any other store; "write" means `promote` for the live store and `review` for any other store.

Errors:
- 400: invalid input.
- 401 / 403: authentication or capability.
- 404: an unknown object or route.
- 409 `{error, type, paths?}`: `precondition_failed`, `path_exists`, `slug_exists`, `store_archived`, `pinned_budget_exceeded`, `adopt_conflict`, `observations_consumed`.
- 413: a body over 4 MiB.

Each recall appends a line to `recall-log.jsonl` next to the database: `ts`, `client` (the request label, else the token name), `project`, `query`, `role`, `topics`, `revision`, `view`, `pinned`, `returned`, `chars`.

## Backup and moving to the Pi

```sh
umask 077
~/.local/bin/agent-memory-server backup --database /absolute/path/memory.sqlite3 --output /absolute/path/new-backup.sqlite3
```

To move hosts:
1. Stop the old service, make a final backup and transfer it.
2. Build the same locked source natively on the 64-bit Pi.
3. Install it with `deploy/agent-memory.service` and a `server.toml` based on `deploy/server.example.toml`.
4. Start only the new service, then point each client's `url` at it.

HTTP is unencrypted, so on an untrusted LAN use an SSH tunnel or an HTTPS proxy. ARM hardware is untested.
