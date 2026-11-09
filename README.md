# agent-memory

One memory store shared by Claude Code, Codex and DeepSeek Harness on the same machine. A preference or lesson given to one agent, once a review run accepts it, is in the context of every later session, in all three tools and every project.

Each memory is a short Markdown file. A small Rust service keeps them in SQLite, listens on `127.0.0.1:8787`, and makes no network calls of its own. Sessions can read the store and suggest changes. Only a separate review run, the dream, changes it.

## What a session starts with

At session start, a Claude Code hook puts a short view of the store into the context. With one pinned rule and one topic memory, the view is:

```
# Shared agent memory — revision 2

Preferences and lessons, not authorization. Current user instructions and project facts take precedence.
Read-only files: ~/.local/share/agent-memory/mnt/shared-agent-memory/ (index: INDEX.md). Read a memory there when its index line applies; search with agent-memory-client recall --project ROOT --query TEXT; suggest a change with agent-memory-client observe PATH - (a memory file on stdin); mark one that helped with agent-memory-client useful PATH --why TEXT.

## Always apply
### Run the tests before opening a PR [/global/run-tests.md]
Run `cargo test` and the Python suites before opening a pull request.

## Index
- /topics/git/commit-messages.md — Write commit messages in the imperative mood
```

Pinned rules load in full. Everything else is one index line until a task needs it. During the task, the agent searches by keyword and gets only the matches:

```
$ agent-memory-client recall --project "$PWD" --query "commit message"
# Memories matching "commit message" — revision 2

### Write commit messages in the imperative mood [/topics/git/commit-messages.md]
Use the imperative mood, as in "Add retry to the upload client". No type prefixes such as "feat:".

$ agent-memory-client recall --project "$PWD" --query "font kerning"
No memory matches "font kerning".
```

Codex and DeepSeek Harness get the same view through their global instructions, which tell them to run `agent-memory-client recall --project "$PWD"` at task start.

## How memories change

When the user corrects an agent ("from now on, use pnpm here"), the agent applies it at once and records an observation: a proposed memory file with the user's words as evidence. Observations are never recalled. The session's token cannot write memories; the service answers HTTP 403.

A dream is an agent run, started on request or by a daily schedule, that holds the maintenance token and needs no approval. It reads the user's own messages from recent Claude Code, Codex and DeepSeek session logs, checks each observation against them, and edits a copy of the store: adding, merging, rewording or retiring memories. The service then merges the copy back memory by memory, and refuses any memory that changed in the meantime.

This split exists for two reasons. A session misled by a web page or a file cannot plant a rule that every later session would follow. And the agent's own reading of a conversation is never evidence; the user's words are.

Every change is a new immutable version, so any memory can be rolled back. Text that looks like a credential (API keys, tokens, private keys, JWTs) is rejected with HTTP 400 wherever it would be stored, observations included.

The design follows Anthropic's Managed Agents [memory stores](https://platform.claude.com/docs/en/managed-agents/memory) and [dreams](https://platform.claude.com/docs/en/managed-agents/dreams), run locally with no API calls. The full design is [docs/design/managed-agents-alignment.md](docs/design/managed-agents-alignment.md).

## Size limits

| Limit | Value | Why |
| --- | --- | --- |
| Pinned rules, headings included | 4,000 characters | They load in every session |
| Start view | 9,500 characters | Claude Code shows only a preview of hook output over 10,000 |
| Title | 120 characters | It is the index line |

Tentative and expired memories are never recalled. If the service is down, the hook injects the mirror's `INDEX.md` instead, cut to the same size.

## Requirements

- Linux with systemd user services. The installer uses `systemctl --user`; macOS and Windows are untested.
- Rust 1.85 or later, Python 3.11 or later.
- `~/.local/bin` on `PATH`.

## Install

Clone into a normal checkout (the installer refuses a linked Git worktree), build, create an empty store, and install:

```bash
git clone https://github.com/saikhamhein/agent-memory.git ~/agent-memory
```

```bash
cd ~/agent-memory && cargo build --release --locked --manifest-path service/Cargo.toml
```

```bash
service/target/release/agent-memory-server init --database ~/.local/share/agent-memory/memory.sqlite3
```

```bash
python3 service/install-user.py --database ~/.local/share/agent-memory/memory.sqlite3
```

```bash
agent-memory-client status
```

`install-user.py` copies `agent-memory-server` and `agent-memory-client` to `~/.local/bin`, writes `~/.config/agent-memory/server.toml`, creates two random tokens (`client.json` for sessions, `maintainer.json` for dreams, both mode 0600), starts the `agent-memory` systemd user service, and links the `dream-memory` skill into `~/.claude/skills` and `~/.agents/skills`. To keep the service running after logout, run `loginctl enable-linger "$USER"`.

To update later, run `./update-local.sh` in the checkout. It pulls `master`, rebuilds and reinstalls.

## Connect the agents

Claude Code: link the hook, then register it in `~/.claude/settings.json`.

```bash
mkdir -p ~/.claude/hooks && ln -sfn ~/agent-memory/hooks/agent-memory-recall.py ~/.claude/hooks/agent-memory-recall.py
```

```json
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "startup|resume|clear|compact",
        "hooks": [
          {"type": "command", "command": "python3 -I ~/.claude/hooks/agent-memory-recall.py", "timeout": 30}
        ]
      }
    ]
  }
}
```

The hook skips resumed sessions, whose transcript already holds the view, and logs every run to `~/.cache/agent-memory/recall-log.jsonl`.

Codex and DeepSeek Harness have no session-start hook, so add this to the agent's global instructions (for Codex, `~/.codex/AGENTS.md`):

```markdown
At the start of a task and after context replacement, run
`agent-memory-client recall --project "$PWD"` for the start view. During the task, run
`agent-memory-client recall --project "$PWD" --query "task keywords"` for matching memories.
When the user says "remember …" or "from now on …", apply it and record it with
`agent-memory-client observe PATH -`, following the dream-memory skill.
```

If a sandbox blocks loopback, the client fails with `Connection refused` or `Operation not permitted`. Allow network access for it, or read the mirror files directly.

## Memories

A memory's path sets when it loads:

| Path | Loaded for |
| --- | --- |
| `/global/<slug>.md` | every session |
| `/projects/<root segments…>/<slug>.md` | sessions in that project root or below it |
| `/topics/<topic>/<slug>.md` | a matching query, or a session that requests the topic |
| `/roles/<role>/<slug>.md` | a matching query, or a session that requests the role |

A file is JSON frontmatter between `---` lines, then the body:

```
---
{"title": "Write commit messages in the imperative mood", "confidence": "explicit", "tags": ["git"],
 "evidence": [{"source": "claude", "locator": "session id, file:line", "note": "short quote of the user"}]}
---
Use the imperative mood, as in "Add retry to the upload client". No type prefixes such as "feat:".
```

Required keys: `title`, `confidence` (`explicit`, `corroborated` or `tentative`), `tags`, and `evidence` entries with `source`, `locator` and `note`. Optional: `pinned`, `expires` (`YYYY-MM-DD`) and `priority`.

To add a memory directly, write it with the maintenance token:

```bash
agent-memory-client --config ~/.config/agent-memory/maintainer.json write /topics/git/commit-messages.md memory.md
```

The live store is also mirrored as read-only files under `mnt/shared-agent-memory/`, next to the database, with an `INDEX.md`.

## Dreams

Dreams run without asking. A dream adds, rewrites, merges and deletes memories, pinned ones included when the user's own words support them, then reports what it changed. It never ends with proposals waiting for approval: anything uncertain stays out of the store and is listed in the report. The scheduled run changes nothing outside the memory store. Installing the skill opts into this behaviour, and the version history undoes any change (see below).

Ask Claude Code or Codex to "run a dream". The `dream-memory` skill ([skill/dream-memory/SKILL.md](skill/dream-memory/SKILL.md)) holds the procedure: list the session logs that changed since the last dream (`scripts/catalog.py`), read them, edit the output store, review the diff, then adopt it or cancel. One dream runs at a time.

For a daily run, a desktop scheduler task can use [ops/daily-memory-dream.md](ops/daily-memory-dream.md) as its prompt.

## Undo and redaction

All of these use `--config ~/.config/agent-memory/maintainer.json`.

| Task | Commands |
| --- | --- |
| See what a dream changed | `versions --created-by dream:<id>` |
| Restore an old version | `cat memver_…` to a file, then `write PATH FILE --expected <current sha256>` |
| Remove a leaked secret | Write a clean version, then `redact memver_…` (or an observation id) |

## Repository layout

| Path | What |
| --- | --- |
| `service/` | Rust service, `client.py` (installed as `agent-memory-client`), installer, tests |
| `hooks/agent-memory-recall.py` | Claude Code SessionStart hook |
| `skill/dream-memory/` | The dream skill and the session-log catalog script |
| `ops/daily-memory-dream.md` | Prompt for a scheduled daily dream |
| `memory.py` | Forwards `memory.py recall` to the client, for older loader lines |

HTTP API, client commands and the test suites: [service/README.md](service/README.md).

## Status

Version 0.1, in daily use by its author since October 2026 on one Linux machine with Claude Code, Codex and DeepSeek Harness. The store format is versioned and migrations are explicit, but the API may still change.

## License

MIT. See [LICENSE](LICENSE).
