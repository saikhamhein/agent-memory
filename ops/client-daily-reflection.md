---
name: client-daily-reflection
description: Once a day, reflect on this machine's agent sessions and send only the lessons to the shared memory store
---

For a machine that isn't the dream machine. Its agents set this up themselves, once, with whatever scheduler the machine has (a desktop scheduled task, cron or a systemd user timer). First check that none exists already, and create one only if the user asked for this schedule. The prompt below is the task. It runs fully autonomously and the user is not present.

## Prompt

Run the "On another machine: reflect here, send lessons" section of the `dream-memory` skill, `~/.agents/skills/dream-memory/SKILL.md`, for this machine's sessions since the last run. It uses `agent-memory-client` with this machine's `~/.config/agent-memory/client.json`, which can recall and observe only.

- Send only distilled lessons through `observe`, each with a quoted user message, provider, machine, session id and date. Never send a log, a transcript or tool output, and never a credential.
- Start no dream, and don't touch memories, git repositories, instruction files, skills, settings or other sessions.
- If the client can't connect, say so in the report and stop. Don't retry in a loop.

Report, short, past tense, for a phone: the observations sent (one line each), the logs read, the day review (went wrong, went right, keep doing, stop doing, one line each), and anything not read or unavailable.
