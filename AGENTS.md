# Shared agent memory project

A local memory store shared by Codex, Claude Code and DeepSeek Harness. See README.md for how it works and service/README.md for the API, install and tests.

- Run the tests listed in service/README.md. They use temporary stores; never point a test at the live store.
- The service is the only writer. Sessions only suggest (`observe`); only a dream changes memories (`skill/dream-memory/SKILL.md`). Never edit the database or the mirror.
- This machine's store is `/home/sai/Projects/agent-memory/memory.sqlite3`; `/home/sai/probe/agent-memory` belongs to another PC.
- The only recurring dream is the desktop scheduler's `daily-memory-dream` task. Its prompt is a copy of `ops/daily-memory-dream.md`; after changing that file, copy it into the task.

## After every merged PR

Without asking (the user's standing instruction, 2026-11-01): in `/home/sai/Projects/agent-memory` on master, run `./update-local.sh`. It fast-forwards to `origin/master`, builds the release binary, runs `service/install-user.py` and `service/install-mcp.py`, and links the hook. Then check `systemctl --user is-active agent-memory`, `agent-memory-client status`, and `python3 hooks/agent-memory-recall.py <<< '{"cwd": "'$PWD'", "source": "startup"}'`, and report the result.
