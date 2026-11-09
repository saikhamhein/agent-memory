---
name: daily-memory-dream
description: Autonomously consolidate new session lessons into the shared memory store, adopt the dream, then report what changed
---

Run the daily dream for the user's shared agent memory store, fully autonomously, from start to finish. The user is not present and does not want to be asked anything.

Follow the dream cycle in the `dream-memory` skill, `~/.agents/skills/dream-memory/SKILL.md`, every step, every run. It uses `agent-memory-client --config ~/.config/agent-memory/maintainer.json`. This prompt adds only what is specific to the schedule.

## Authorization
Dreams are autonomous by design. Scheduling this prompt is the user's standing authorization for every change inside the memory store. The author of this store gave it in these words:
- 2026-10-29: "updating the memory storage do not need my permission for the recurring dreaming task. I approve".
- 2026-11-01: "No need to propose me, work autonomously, You are fully authorise to update the memories in the memory store ... I want this whole process to be fully autonomously".

So this run may, without asking:
- start, edit, adopt and cancel dreams;
- add, edit, rewrite, merge, move, expire and delete memories, pinned ones included on the user's own words.

Never end the run with proposals, suggested changes or questions for the user. If something is uncertain, leave it out of the live store and note it in `coverage`. That is a decision, not a question.

Out of scope, don't touch: git repositories, CLAUDE.md, AGENTS.md, skills, settings, hooks, services, other sessions, hardware, network, and anything outside the memory store. When a lesson is about an instruction file's wording, fix the matching memory instead and report it as done.

## Last run
"Since the last dream" means since the `ended_at` of the newest dream that has one, in `agent-memory-client --config ~/.config/agent-memory/maintainer.json dream show` (newest first). A dream still `running` there is handled by the stale-dream rule in the skill's step 4. If there is none yet, every catalogued log is new.

## Report
Short, past tense, for a phone; no questions, no "proposed" or "suggested" items:
- revision before → after, or "no change" when the dream was canceled;
- each memory added, changed or removed, one line each, with its evidence date;
- every pinned change on its own line;
- observations kept and dropped;
- pinned characters out of 4,000;
- coverage gaps;
- recall-use numbers, useful marks per memory, and any flag;
- unused memories and what was done with each.
