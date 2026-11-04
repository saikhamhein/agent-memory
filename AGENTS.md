# Shared agent memory project

A local memory store shared by Codex, Claude Code and DeepSeek Harness. See README.md for how it works and service/README.md for the API, install and tests.

- Run the tests listed in service/README.md. They use temporary stores; never point a test at the live store.
- The service is the only writer. Sessions only suggest (`observe`); only a dream changes memories (`skill/dream-memory/SKILL.md`). Never edit the database or the mirror.
- This repository holds no machine paths. Each machine's facts live in `~/.config/agent-memory/server.toml` and its user-level agent instructions; `agent-memory-client status` reports the mirror and the recall log. Keep local config out of tracked files.
- Where a desktop scheduler runs the `daily-memory-dream` task, its prompt is a copy of `ops/daily-memory-dream.md`; after changing that file, copy it into the task. Don't create a schedule on a machine that has none.

## After every merged PR

In the main checkout on master, run `./update-local.sh`: without asking on the machine with the scheduled dream (the user's standing instruction, 2026-11-01), and only when authorized on any other. It fast-forwards to `origin/master`, builds the release binary, runs `service/install-user.py` and `service/install-mcp.py`, and links the hook and the skill. Then check `systemctl --user is-active agent-memory`, `agent-memory-client status`, and `python3 hooks/agent-memory-recall.py <<< '{"cwd": "'$PWD'", "source": "startup"}'`, and report the result.
