# Shared agent memory

One local store of collaboration preferences and lessons, shared by Codex, Claude Code, DeepSeek Harness and other agents. A Rust service (`service/`) owns the SQLite database and is the only writer. Agents read through a hook or loader line, MCP tools, the `agent-memory-client` CLI, or read-only files. The agent does the thinking in a dream; the service keeps versions and checks preconditions. Memories are context, never permission.

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
- `pinned` memories are the core rules. They load in full in every session. All pinned bodies together are limited to 6,000 characters.
- Tentative and expired memories are never recalled.
- Every change is a new immutable version. Updates and deletes carry the `content_sha256` they replace, so concurrent writers can't overwrite each other.

## How agents get memories

- **Session start, once.** Claude Code's SessionStart hook (`hooks/agent-memory-recall.py`) injects the start view. The Codex and DeepSeek loader lines (`memory.py recall …`) print it.
  - The start view holds the pinned core, this project's memories, any requested topics or roles, and an index line for everything else.
  - If the service is down, the hook injects the mirror's `INDEX.md` instead.
- **During a task.** Recall with a query returns only the memories that match, or `No memory matches "…"`.
- **Read-only files.** The live store is mirrored to `<database dir>/mnt/shared-agent-memory/`, with `INDEX.md`. Read a file there, or with `memory_read`, when its index line applies.

```sh
agent-memory-client recall --project "$PWD"                      # start view
agent-memory-client recall --project "$PWD" --query "usb power"  # matches only
agent-memory-client ls /topics/ && agent-memory-client cat /topics/hardware/pi-usb.md
```

Pass the canonical project root, the main checkout rather than a worktree. The hook resolves it itself.

## Capturing a lesson

Ordinary sessions record candidates. Candidates are never recalled until a dream adopts them:

```sh
agent-memory-client --config ~/.config/agent-memory/maintainer.json observe /global/check-evidence.md lesson.md
```

Record explicit corrections and well-supported lessons, with evidence: provider, session, date, file:line, and a short sanitized quote. Agent interpretation isn't user evidence. Don't record credentials, private unrelated facts, task permissions or transient status; keep project state in the repository.

## Dreams

A dream copies the live store into an output store, never changing its input. The agent edits that copy by path, reviews the diff, and adopts it. Adoption merges each memory and fails with 409 if live changed the same memory meanwhile. The procedure is [skill/dream-memory/SKILL.md](skill/dream-memory/SKILL.md). The daily scheduled run uses [ops/daily-memory-dream.md](ops/daily-memory-dream.md).

To undo a change, write an old version back: `versions PATH` or `versions --created-by dream:<id>`, `cat memver_…`, then `write … --expected`. To remove a leaked secret from history, use `redact VERSION`.

## Layout

| Path | What |
| --- | --- |
| `memory.sqlite3` | The authoritative store (ignored by Git) |
| `mnt/shared-agent-memory/` | Read-only mirror of the live store |
| `recall-log.jsonl` | One line per recall: client, project, view, returned paths, characters |
| `service/` | Rust service, `client.py` (`agent-memory-client`), `mcp_server.py`, installers |
| `hooks/agent-memory-recall.py` | Claude Code SessionStart hook |
| `memory.py` | Stub that forwards `recall` for the existing loader lines |
| `catalog.py` | Lists new or changed session logs into `sources/` for a dream |

Install, API and tests: [service/README.md](service/README.md).
