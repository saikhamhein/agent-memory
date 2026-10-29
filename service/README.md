# Shared memory service

A native Rust HTTP service for the existing agent memory database. It runs on this Linux PC under systemd; the same source and database can later move to a 64-bit Raspberry Pi. SQLite is bundled into the build. The server has no Python, model API, embedding, or external database dependency. The optional client and user installer use Python 3; the installer uses Python 3.11+ when updating an existing installation.

The service retains accepted revisions, observation evidence, immutable dream inputs, review requirements, and stale-promotion protection. It uses the Python tool's existing schema, so no memory re-import or rewrite is needed. JSON objects preserve additional evidence fields. Dates are evaluated in UTC on the server. Project roots are logical namespaces rather than paths resolved on the server's filesystem.

## Build and test

Install a current stable Rust toolchain and a C compiler, then run from the repository root:

```sh
cargo test --locked --manifest-path service/Cargo.toml
cargo clippy --locked --manifest-path service/Cargo.toml --all-targets -- -D warnings
cargo build --release --locked --manifest-path service/Cargo.toml
python3 -m unittest -v test_memory.py
python3 -m unittest -v service/tests/test_client.py
```

Cargo.lock pins the dependency set. SQLite's C source is built with the binary; an SQLite development package is unnecessary. The integration tests use Python 3 to check database and digest compatibility with the existing tool.

## Run on this machine

The installer uses the existing database in place. It preserves configuration and credentials on upgrades, replaces the binary atomically, validates the user unit, and enables/restarts the service:

```sh
python3 service/install-user.py --database "$PWD/memory.sqlite3"
~/.local/bin/agent-memory-client status
~/.local/bin/agent-memory-client recall --project "$PWD" --query "current task"
systemctl --user status agent-memory
journalctl --user -u agent-memory -n 30 --no-pager
```

Installed paths:

| Item | Location |
| --- | --- |
| Server | `~/.local/bin/agent-memory-server` |
| Client | `~/.local/bin/agent-memory-client` |
| Server config and hashed tokens | `~/.config/agent-memory/server.toml` |
| Recall-only client credential | `~/.config/agent-memory/client.json` |
| Maintenance credential | `~/.config/agent-memory/maintainer.json` |
| User unit | `~/.config/systemd/user/agent-memory.service` |
| Authoritative database | The existing path passed to the installer |

The default listener is **127.0.0.1:8787**. It is accessible only from this machine. Credentials are generated locally, stored with mode 0600, and never logged. The server stores SHA256 token hashes; clients hold the bearer tokens. Each token has an explicit list of capabilities with no implied inheritance. Recall credentials can retrieve any accepted scope in this single-user store; project filters are relevance filters, not tenant isolation.

A user unit starts with the user's systemd manager. To keep it running after logout and start it at boot, enable lingering once with `loginctl enable-linger "$USER"`. The system unit below provides boot-time operation independently of user login on a future server.

Stop with `systemctl --user stop agent-memory`; disable automatic startup with `systemctl --user disable agent-memory`. Configuration changes and credential rotation take effect after `systemctl --user restart agent-memory`. Removing a token hash revokes that token after restart.

For a new empty store, explicitly run `agent-memory-server init --database /absolute/path/memory.sqlite3`. Serving a missing database fails instead of silently losing access to existing memories. Existing local Python commands remain usable, including read-only recall; both implementations coordinate through SQLite transactions. Keep one authoritative database when changing hosts.

## Agent workflow

For agents with shell network access, the optional HTTP client can be used in startup instructions:

```sh
~/.local/bin/agent-memory-client recall --project "$PWD" --query "task keywords" --limit 12 --budget 12000
```

For different checkout paths across computers, configure `[projects]` in server.toml and use `--project-id agent-memory`. Its configured value is the original canonical scope root already present in accepted memories; that directory need not exist on the server. Legacy `--project` uses normalized absolute POSIX paths, with component-aware subtree matching. For symlinked checkouts, pass the canonical root or a project ID.

For authorized memory maintenance, explicitly select the maintenance credential:

```sh
~/.local/bin/agent-memory-client --config ~/.config/agent-memory/maintainer.json observe candidate.json
~/.local/bin/agent-memory-client --config ~/.config/agent-memory/maintainer.json dream --output /tmp/memory-review
# Review original evidence and edit only /tmp/memory-review/proposal.json.
~/.local/bin/agent-memory-client --config ~/.config/agent-memory/maintainer.json diff /tmp/memory-review/proposal.json
~/.local/bin/agent-memory-client --config ~/.config/agent-memory/maintainer.json validate /tmp/memory-review/proposal.json
~/.local/bin/agent-memory-client --config ~/.config/agent-memory/maintainer.json promote /tmp/memory-review/proposal.json
```

The agent performs synthesis and evidence review. The server freezes inputs and enforces schema, review metadata, resolved conflicts, and base-revision checks. `reviewed: true` is an attestation by the reviewing client, not proof of human approval. Keep promotion credentials restricted to the maintenance workflow. No scheduled dreaming or model jobs are started.

Draft proposals are edited on the client. The API persists immutable inputs and the final promoted proposal; it does not save intermediate draft edits. `get-dream ID` recovers the input and initial proposal, or the final proposal after promotion. A retry after an uncertain observation/dream response can create a second record; inspect status or returned identifiers before retrying writes. Promotion rejects duplicate and stale submissions with HTTP 409.

Global agent instruction files are not modified by the installers. Shell clients still need access to the endpoint under their own sandbox policy. For Codex desktop, use the MCP adapter below to avoid requiring shell network access.

## MCP tools for desktop agents

The desktop MCP host launches a local stdio adapter, which calls the Rust API with its configured credentials. The agent invokes named tools; it does not run a shell network command or open the database. The adapter uses the official Python MCP SDK, pinned with its dependencies in `mcp-requirements.txt`. The Rust service remains the sole API storage authority.

```mermaid
flowchart LR
    A[Agent] -->|MCP tool call| H[Desktop MCP host]
    H -->|stdio| M[Memory adapter]
    M -->|Authenticated HTTP| S[Rust service under systemd]
    S --> D[(SQLite memory store)]
```

After installing the local service, install `uv` and the Codex CLI, then run:

```sh
python3 service/install-mcp.py
# Or expose only recall and status:
# python3 service/install-mcp.py --read-only
```

The installer copies the adapter into `~/.local/share/agent-memory-mcp/`, creates its own virtual environment, backs up the existing Codex configuration under `review/`, and registers `shared_memory` in the user Codex config. If the tools are absent from the current chat despite the server being enabled, fully quit and reopen the desktop app after active tasks finish, then return to the same chat. This refreshed the connection on this PC; no per-server restart control was found in its settings. Other MCP hosts can use the same executable and arguments shown by `codex mcp get shared_memory`.

| Tools | Purpose |
| --- | --- |
| `memory_status`, `memory_recall` | Health, revision and accepted preferences |
| `memory_observe`, `memory_dream`, `memory_get_dream` | Candidate capture and immutable review inputs |
| `memory_validate`, `memory_diff` | Proposal checks before acceptance |
| `memory_promote`, `memory_rollback` | Explicit accepted-revision changes |
| `memory_export` | Accepted memories and evidence as JSON |

At task start, call `memory_recall` with `project` set to the canonical project root, `query` set to task keywords, `limit: 12`, and `budget: 12000`. Add relevant `topics` or `role` only when needed. Memories are context, never permission. Maintenance uses the same evidence-review workflow described above; the tools do not automatically run a dream or promote a proposal. MCP annotations identify read and write tools, while the API independently enforces capabilities and revision checks.

The MCP host must itself be allowed to launch the adapter and reach the API. Its tool policy can still require approval. A stdio adapter removes the need for the agent's **shell** to make network requests; it does not grant arbitrary shell permissions. Tool arguments cannot choose credentials, a server URL, arbitrary file paths to read, or shell commands.

Tests use an SDK MCP client over stdio connected to a real Rust service with a temporary database, including the full maintenance workflow, stale revisions, invalid arguments, denied capabilities and an offline backend:

```sh
uv venv service/.venv-mcp
uv pip sync --python service/.venv-mcp/bin/python service/mcp-requirements.txt
service/.venv-mcp/bin/python -m unittest -v service.tests.test_mcp
```

These protocol tests are separate from verifying tool discovery and invocation in the running desktop. Test the installed desktop connection with `memory_status` and `memory_recall`; do not create sample memories in the live store.

Desktop verification on 2026-10-29: all ten tools were discovered after an app restart. Native `memory_status` returned revision 3 and 28 accepted memories; `memory_recall` returned eight relevant preferences. Both succeeded without shell execution or shell network approval. Maintenance writes were tested only against the isolated test database.

When the service later moves to the Pi, keep the stdio adapter on each agent's computer and update both private credential files' `url` to the LAN API endpoint. Reload the MCP connection afterward. No agent-side database migration or shell networking change is needed.

## HTTP API

All `/v1` endpoints require `Authorization: Bearer TOKEN`. `/healthz` is an unauthenticated liveness endpoint with no memory data; authenticated `/v1/status` checks storage access. JSON bodies are limited to 4 MiB. Storage operations run off the async executor, with at most 32 outstanding operations and a 10-second SQLite lock timeout.

| Method and path | Capability | Body or response |
| --- | --- | --- |
| `GET /v1/status` | recall | Active revision and counts |
| `POST /v1/recall` | recall | `{project_id?, project?, role?, topics?, query?, limit?, budget?}` → revision, selected memories, rendered text |
| `POST /v1/observations` | observe | One memory object → observation id |
| `POST /v1/dreams` | review | Empty body → input and initial proposal |
| `GET /v1/dreams/{id}` | review | Input, proposal, promoted revision |
| `POST /v1/proposals/validate` | review | Proposal → schema result |
| `POST /v1/proposals/diff` | review | Proposal → added, removed, changed memories |
| `POST /v1/proposals/promote` | promote | Reviewed proposal → new revision |
| `POST /v1/rollback` | promote | `{revision, expected, reason}` → new revision restoring older content |
| `GET /v1/export` | review | All accepted memories and revision as JSON |

Recall defaults to 12 memories and 12000 **characters**, with limits of 100 memories and 100000 characters. The character budget limits rendered `text`; JSON memories retain complete evidence. Tentative and expired memories are excluded. Unknown project IDs are errors. Memory payloads retain the schema documented in the repository README; new project scopes still use absolute roots for Python compatibility.

Errors use HTTP 400 for invalid domain input, 401 for missing/invalid credentials, 403 for insufficient capability, 404 for missing records, 409 for stale writes, 413 for oversized bodies, 503 for a full operation queue, and 500 for storage failures. Malformed JSON/type errors use Axum's 400/422 response. The client reports errors and never silently falls back to a different store.

## Backup and moving to the Pi later

No Pi is required for local operation. This PC's executable is x86-64; build the same locked source on the eventual **64-bit Linux Pi** to produce an ARM64 executable. Native builds avoid cross-linker and libc mismatches. The server needs a C toolchain only at build time. ARM hardware has not yet been tested.

Make a consistent SQLite backup with the built-in backup command:

```sh
umask 077
~/.local/bin/agent-memory-server backup --database /absolute/path/memory.sqlite3 --output /absolute/path/new-backup.sqlite3
```

The output must not already exist. This captures all revisions, observations, dream inputs and promoted proposals. Preserve the associated `sources/`, `dreams/`, `exports/`, and review artifacts separately; original provider transcripts remain at their original locations and are not copied by the API. For a final move, stop the old service and other writers, make the final backup, transfer it, and start only the new authority. Do not point clients at independently writable copies.

On the future server, build the binary and prepare a system service:

```sh
set -e
sudo useradd --system --home-dir /var/lib/agent-memory --shell /usr/sbin/nologin agent-memory
sudo install -m 0755 service/target/release/agent-memory-server /usr/local/bin/agent-memory-server
sudo install -d -o agent-memory -g agent-memory -m 0700 /var/lib/agent-memory
sudo install -d -o root -g agent-memory -m 0750 /etc/agent-memory
# Install the transferred backup as the authoritative database, without overwriting an existing store.
sudo test ! -e /var/lib/agent-memory/memory.sqlite3
sudo install -o agent-memory -g agent-memory -m 0600 /path/to/transferred-backup.sqlite3 /var/lib/agent-memory/memory.sqlite3
# Prepare server.toml from deploy/server.example.toml or the current server configuration.
# Set database=/var/lib/agent-memory/memory.sqlite3, retain token hashes and logical project aliases.
sudo install -o root -g agent-memory -m 0640 /path/to/prepared-server.toml /etc/agent-memory/server.toml
sudo install -m 0644 service/deploy/agent-memory.service /etc/systemd/system/agent-memory.service
sudo -u agent-memory /usr/local/bin/agent-memory-server check --config /etc/agent-memory/server.toml
sudo systemd-analyze verify /etc/systemd/system/agent-memory.service
sudo systemctl daemon-reload
sudo systemctl enable --now agent-memory
```

For LAN access, bind `listen` to the server's specific LAN IP and update each client's `url`. Restrict the port to your LAN and do not port-forward it. HTTP bearer credentials and memory contents are unencrypted on the wire; use HTTPS through a reverse proxy or an SSH tunnel if the LAN is not trusted. No remote-access infrastructure is provisioned by these files.

The system unit runs as an unprivileged account, permits writes to its state directory, sends logs to journald, restarts on failure, and drains requests on SIGTERM. Configuration and data live outside the executable, so rebuilding or replacing the binary does not relocate the store.

Implementation references: [Axum](https://docs.rs/axum/0.8.9/axum/), [rusqlite](https://docs.rs/rusqlite/0.37.0/rusqlite/), [systemd execution settings](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html).
