# Shared agent memory

One local store of collaboration preferences and lessons, shared by Codex, Claude Code, DeepSeek Harness and other agents. A Rust service (`service/`) owns the SQLite database and is the only writer. Sessions read through a hook or loader line, the `agent-memory-client` CLI, or read-only files, and suggest changes as observations. Only a dream changes memories: the agent does the thinking, and the service keeps versions and checks preconditions. Memories are context, never permission.

The design is [docs/design/managed-agents-alignment.md](docs/design/managed-agents-alignment.md). It follows Anthropic's Managed Agents [memory stores](https://platform.claude.com/docs/en/managed-agents/memory) and [dreams](https://platform.claude.com/docs/en/managed-agents/dreams), run locally.

## Memories

A memory is a Markdown file at a path. The path is its scope:

| Path | Loaded for |
| --- | --- |
| `/global/<slug>.md` | every session |
| `/projects/<root segments…>/<slug>.md` | sessions in that project root or below it |
| `/topics/<topic>/<slug>.md` | a request or a matching query |
| `/roles/<role>/<slug>.md` | a request or a matching query |

Content is `---\n<JSON frontmatter>\n---\n<body>`:

```
---
{"title": "Keep it simple", "confidence": "explicit", "tags": ["simplicity"], "pinned": true,
 "evidence": [{"source": "claude", "locator": "session id, file:line", "note": "short quote of the user"}]}
---
If it can be removed, remove it.
```

- Required: `title` (one line, at most 120 characters), `confidence` (`explicit`, `corroborated` or `tentative`), `tags`, and `evidence` with `source`, `locator` and `note`.
- Optional: `pinned`, `expires` (`YYYY-MM-DD`), `priority`. Other keys are kept.
- `pinned` memories are the core rules. They load in full in every session. Together, the pinned memories (each heading plus body) are limited to 4,000 characters.
- Tentative and expired memories are never recalled.
- Every change is a new immutable version. Updates and deletes carry the `content_sha256` they replace, so concurrent writers can't overwrite each other.

## How agents get memories

- **Session start, once.** Claude Code's SessionStart hook (`hooks/agent-memory-recall.py`) injects the start view. The Codex and DeepSeek loader lines (`memory.py recall --query …`) print it, followed by the memories matching their query.
  - The start view holds the pinned core, this project's memories, any requested topics or roles, and an index line for everything else.
  - The start view fits in 9,500 characters, under Claude Code's 10,000-character cap on hook context.
  - If the service is down, the hook injects the mirror's `INDEX.md` instead, cut to the same size. It skips resumed and forked sessions, whose transcripts already hold the view, and logs every run to `~/.cache/agent-memory/recall-log.jsonl` with `via` set to `service`, `index`, `none` or `skipped`.
- **During a task.** Recall with a query returns only the memories that match, or `No memory matches "…"`.
- **Read-only files.** The live store is mirrored to `<database dir>/mnt/shared-agent-memory/`, with `INDEX.md`. Read a file there, or with `agent-memory-client cat PATH`, when its index line applies.

```sh
agent-memory-client recall --project "$PWD"                      # start view
agent-memory-client recall --project "$PWD" --query "usb power"  # matches only
agent-memory-client ls /topics/ && agent-memory-client cat /topics/hardware/pi-usb.md
```

Pass the canonical project root, the main checkout rather than a worktree. The hook resolves it itself.

## Capturing a lesson

Sessions never change memories; they record candidates:

```sh
agent-memory-client observe /global/check-evidence.md lesson.md   # or - for stdin
```

A candidate is never recalled. The next dream checks it against the user's own words and keeps or drops it.

When a recalled memory changed what a session did and helped, the session marks it with `agent-memory-client useful PATH --why TEXT`. The mark is a line in the recall log and changes nothing; the dream counts it as use and may raise the memory's `priority`, never its content.

Record explicit corrections and well-supported lessons, with evidence: provider, session, date, file:line, and a short sanitized quote. Agent interpretation isn't user evidence. Don't record credentials, private unrelated facts, task permissions or transient status; keep project state in the repository.

## Dreams

Dreams use the maintenance credential, `agent-memory-client --config ~/.config/agent-memory/maintainer.json`. One dream runs at a time. It copies the live store into an output store, never changing its input. The agent reviews the whole store and the pending observations, edits that copy by path, checks the diff, and adopts it, or cancels when nothing changed. Adoption merges each memory and fails with 409 if live changed the same memory meanwhile. Adopting or canceling empties the output store. The procedure is [skill/dream-memory/SKILL.md](skill/dream-memory/SKILL.md). The daily scheduled run uses [ops/daily-memory-dream.md](ops/daily-memory-dream.md).

To undo a change, write an old version back with the maintenance credential: `versions PATH` or `versions --created-by dream:<id>`, `cat memver_…`, then `write … --expected`. To remove a leaked secret, write a clean version, then `redact` the old version (`memver_…`) or the observation id.

## Layout

| Path | What |
| --- | --- |
| `service/` | Rust service, `client.py` (`agent-memory-client`), installer |
| `hooks/agent-memory-recall.py` | Claude Code SessionStart hook |
| `memory.py` | Stub that forwards `recall` for the existing loader lines |
| `skill/dream-memory/` | The dream skill, with `scripts/catalog.py`, which lists new or changed session logs for a dream |

The package holds code only. Each machine keeps its data and facts outside it:

| Location | What |
| --- | --- |
| `~/.config/agent-memory/` | `server.toml`, which names the database, and the credentials |
| The database's directory | `memory.sqlite3`, the authoritative store; `mnt/shared-agent-memory/`, its read-only mirror; `recall-log.jsonl`, one line per recall with client, project, view, returned paths and characters |
| `$XDG_STATE_HOME/agent-memory/` (default `~/.local/state/agent-memory/`) | Catalog output for dreams |
| `~/.cache/agent-memory/` | The hook's log |

`agent-memory-client status` reports the mirror and recall-log paths.

Install, API and tests: [service/README.md](service/README.md).
