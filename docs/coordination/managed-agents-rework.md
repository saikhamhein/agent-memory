# Managed Agents alignment rework — coordinator state

Goal (user, 2026-11-01): rework this repository's memory store and dream workflow using Anthropic's Managed Agents [memory](https://platform.claude.com/docs/en/managed-agents/memory) and [dreams](https://platform.claude.com/docs/en/managed-agents/dreams) design as the source idea.

Coordinator: session in worktree `managed-agents-memory-dreams-622f97` (branch `claude/managed-agents-memory-dreams-622f97`). Coordinates only; teammates implement. Teammates send the coordinator an approved PR number and head SHA; the coordinator merges.

## Standing constraints

- Local implementation. No Managed Agents API calls, cloud upload of transcripts, or paid model jobs (existing README design basis). Revisit only with the user.
- The live store `/home/sai/Projects/agent-memory/memory.sqlite3` and the `agent-memory` user service on `127.0.0.1:8787` are not touched by teammates. Tests use temporary stores. Migrating or reinstalling the live installation needs the user's go in chat.
- Existing evidence and revision history must survive any schema change (migration, not rewrite).
- Clients that must keep working: Python CLI `memory.py`, Rust service `service/`, MCP adapter `service/mcp_server.py`, Claude SessionStart hook, Codex/DeepSeek CLI loaders, the `daily-memory-dream` scheduled task.

## Phase

1. Design (architect): gap analysis and target design → `docs/design/managed-agents-alignment.md`, with workstreams that have disjoint file ownership and test-first acceptance criteria.
2. Build: test engineer + engineer per workstream, in parallel.
3. Fresh-eyes review and triage.
4. Live migration (needs the user's go).

## Owners

| Role | Session | Status |
| --- | --- | --- |
| Architect | `local_b602e858-1cf0-40c9-a15d-c78a09d3e1e3` (architect) | PR 6 rev 3 merged @ c9b48a3; reviews workstream PRs |
| Engineer A (store + mirror) | `local_f7b57f17-4e91-4996-ba3b-3fddbd222144` | building, branch from `rework/managed-agents-v2` |
| Test engineer A | `local_c79accd0-cf18-4ce5-9f59-86e28b10f1a7` | tests → `rework/tests-a` |
| Engineer R (recall) | `local_6158a6a3-f625-44c2-9401-7a559884a564` | building |
| Test engineer R | `local_88b6d74f-ee66-441e-a12a-cb3df7e5719c` | tests → `rework/tests-r` |
| Engineer H (HTTP API) | `local_a320fa63-05c9-4d9a-9f6a-ebad5ce08624` | building |
| Test engineer H | `local_7a6bae6a-f710-4da7-a17d-ba7c2ff467f9` | 21 tests: `rework/tests-h` @ `fc956f7`, fail to compile on d9e51c7 as intended; 9 spec questions with architect |
| Engineer M (migration + install) | `local_15a0c48f-71c2-43fe-8db3-cc45fdea948c` | building |
| Test engineer M | `local_9adf3c05-b247-4bd0-97bb-f64f1214b6aa` | tests → `rework/tests-m` |
| Engineer C (clients, hook, docs) | `local_111adbc3-3795-4a66-881a-f9190f3f09ac` | building |
| Test engineer C | `local_51c2d84e-6cc7-487f-9294-8ccccdbd4e05` | 27 tests: `rework/tests-c` @ `620440a`, all failing on the integration head; open: stub start+task output, awaiting architect |

## Merged

- PR 8 (hook-cap rulings: pinned budget 4,000, START_MAX 9,500 under Claude Code's 10,000-char hook cap, error/diff shapes; doc only) → `rework/managed-agents-v2` `8c57c6b`. Leftover: §4.3 R.3 still says "within 8,000 characters"; architect to fix in the next doc PR.
- PR 7 (recall rendering rulings, doc only) → `rework/managed-agents-v2` `d9e51c7`.
- PR 6 (design rev 3, keeps the Rust service, one renderer) → master `83ff1f7`; reinstalled, service active. Integration branch `rework/managed-agents-v2` cut at `83ff1f7`.
- PR 5 (state file + reinstall rule) → master `94f0ab4`; local setup reinstalled, service active, revision 9.

## Decisions and inputs

- 2026-11-01 user: do not trust the existing implementation; retrieval "dumps all". Verified causes: globals always qualify (query only reorders), budget caps text only and drops silently, MCP recall also returns full evidence as `structuredContent`, hook repeats the globals. Forwarded to architect as first-class design input.
- Prior art: unpushed branch `feat/recall-on-demand` @ `d125894` (worktree `.claude/worktrees/recall-on-demand`) from session `local_cc899943-24f5-416f-bb84-4b0a3e64223e`: recall start/task modes, text-only MCP recall, named budget omissions, hook in repo, `update-local.sh`. Architect evaluates it as an input, not a base.
- 2026-11-01 user confirmed: after every merged PR, fast-forward local master and reinstall without asking (rule in `AGENTS.md`; memory `agent-memory-reinstall-after-merge`, revision 9).

- 2026-11-01 memory revision 10: `keep-it-simple` (global, priority 10) replaces `remove-what-is-not-needed` + `small-reusable-solutions`; it was cut from every baseline recall at priority 1. Reinstall memory removed (lives in AGENTS.md).
- PR 6 review: (1) `start` must load only an always-load core + project + requested scopes in full, plus a one-line index of everything else; tests for core never cut and bounded output; (2) don't merge `feat/recall-on-demand` first, cut the integration branch from master; (3) KISS: one recall renderer (Rust), drop dream `instructions`, justify or cut the store list, `kind` and symlink generations; (4) writes only through the service, settled by the coordinator.
- Architect ruling (no contract change): A's PR may carry owner-labelled placeholders `recall.rs` (R), `api.rs` cut to `/healthz` + `/v1/status` (H), `main.rs` cut to init/serve/backup/check (M); owners replace them wholesale. A must not touch `config.rs`, and the integration branch must build and pass `cargo test` at A's head. On merge: check placeholders are labelled and the integration head builds.

## Blockers

- Architect session's outgoing messaging is paused by the app and needs the user to resume it there; seven rulings are undelivered. The coordinator does not relay around the pause.
- Proposed card `task_8b1e5f32`: text-only MCP recall quick win on master.

## Next actions

- On each architect-approved PR (number + head SHA): read the diff, check it's current with `rework/managed-agents-v2` and mergeable, run combined tests, merge with `--match-head-commit` in order A → R → H → M → C, tell later workstreams to merge the integration head. No reinstall: these merges don't touch master.
- Then: combined check, `migrate --dry-run --pin keep-it-simple` on a copy of a fresh live backup, fresh-eyes review + triage, then cutover with the user's go.
