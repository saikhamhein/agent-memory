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
| Engineer A (store + mirror) | `local_f7b57f17-4e91-4996-ba3b-3fddbd222144` | PR 11 merged |
| Test engineer A | `local_c79accd0-cf18-4ce5-9f59-86e28b10f1a7` | `rework/tests-a` @ `f565521`: 19 tests, 19/19 on engineer WIP, 6 mutants caught; told to adopt PR 10 block_len |
| Engineer R (recall) | `local_6158a6a3-f625-44c2-9401-7a559884a564` | PR 9 merged |
| Test engineer R | `local_88b6d74f-ee66-441e-a12a-cb3df7e5719c` | `rework/tests-r` @ `30114e5`: 19 tests, PR 10 block_len adopted |
| Engineer H (HTTP API) | `local_a320fa63-05c9-4d9a-9f6a-ebad5ce08624` | PR 14 merged |
| Test engineer H | `local_7a6bae6a-f710-4da7-a17d-ba7c2ff467f9` | `rework/tests-h` @ `0a70203`: encodes the architect rulings; fails to compile on the integration head as intended |
| Engineer M (migration + install) | `local_15a0c48f-71c2-43fe-8db3-cc45fdea948c` | PR 15 merged |
| Test engineer M | `local_9adf3c05-b247-4bd0-97bb-f64f1214b6aa` | `rework/tests-m` @ `87ec62f` (4,000 pin budget): 13 migrate + store tests + 5 install tests, all failing on the integration head; 4 rulings asked |
| Engineer C (clients, hook, docs) | `local_111adbc3-3795-4a66-881a-f9190f3f09ac` | PR 17 merged |
| Test engineer C | `local_51c2d84e-6cc7-487f-9294-8ccccdbd4e05` | 27 tests: `rework/tests-c` @ `eea5b60` (stub prints start + task view; hook env overrides), all failing on the integration head; open: stub start+task output, awaiting architect |

## Merged

- PR 17 (WS-C clients, MCP, hook, stub, skill, docs) → `rework/managed-agents-v2`. Coordinator check at head d4633e6 with a release build: 33 Python tests (28 C + 5 install), 79 Rust tests; live service untouched. All five workstreams are merged.
- PR 15 (WS-M migration + install) → `rework/managed-agents-v2`. Coordinator check at head 4b243e4 (release): 79 Rust tests, 5 install tests (sandboxed HOME, fake systemctl), clippy and fmt clean; M's files unchanged since the architect's 4445976 approval; live service untouched.
- PR 14 (WS-H HTTP API) → `rework/managed-agents-v2` `8fbb862`. Coordinator check at head 1eeb4eb: 64 Rust tests, clippy and fmt clean; api.rs/config.rs unchanged since the architect's e548fe2 approval.
- PR 9 (WS-R recall) → `rework/managed-agents-v2` `ef2f07e`. Coordinator reviewed R's blocked local commit, merged in PR 16, re-ran 43 tests + clippy + fmt, pushed as `0ffb46a`, merged.
- PR 11 (WS-A store core + mirror, architect-approved at 529c2d5) → `rework/managed-agents-v2` `76e85fd`. Coordinator check on the merged result: 23 Rust tests pass, clippy -D warnings and fmt clean; placeholders for recall.rs/api.rs/main.rs labelled with owners. Python suites are expected red until C.
- PR 16 (cutover: save the old hook to runtime/ first; rerun update-local.sh after --migrate; doc only) → `rework/managed-agents-v2`.
- PR 13 (index lines still fill beside the collapse line; doc only) → `rework/managed-agents-v2`.
- PR 12 (INDEX.md contents, 255-byte path segments, observation ids in `observations_consumed` paths; doc only) → `rework/managed-agents-v2`.
- PR 10 (budgets count the rendered block incl. heading, `model::block_len`; doc only) → `rework/managed-agents-v2`.
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
- Coordinator ruling: test-M `replays_history` expects 6 versions, not 7 (its own lineages sum to 6). Corrects an arithmetic error; loosens nothing.
- 2026-11-01 user: repo authority was granted at session start; the coordinator reviews and pushes or merges a teammate's blocked commit itself and does not ask. Memory revision 11 (`coordinate-a-team-playbook` item 10).
- 2026-11-01 dry run (integration head 83d8090, release binary) on a sqlite3 backup-API copy of the live store at legacy revision 11: exit 0, copy byte-identical afterwards, 25 memories, versions 29 created / 11 modified / 4 deleted, revision 44, pinned_chars 626 with `--pin keep-it-simple`. Proposed pins: keep-it-simple, direct-concrete-communication, research-unfamiliar-before-relying, autonomy-within-scope, investigate-before-asking (about 2,826 of 4,000). Architect agreed. Re-run: pinned_chars 2,826 (block_len). A real migration of a second scratch copy served on a spare port gives: no-project start view 4,543 chars (5 pinned in full + 13 index lines; other projects excluded); custom-lora-mesh start 7,630 chars (5 pinned + 5 project in full, 15 lines); `kubernetes ingress yaml` → exactly `No memory matches`; hardware query → 1,126 chars of matching topics only.

## Blockers

- Review campaign note: test-C's 28 tests passed first try; the fresh-eyes review should mutation-check them.
- Architect session's outgoing messaging paused again (2026-11-01, second time); rulings for H/test-H and the PR 10 notice undelivered. Needs the user to resume it there. The coordinator does not relay around the pause.
- Proposed card `task_8b1e5f32`: text-only MCP recall quick win on master.

## Next actions

- Fresh-eyes review campaign of `rework/managed-agents-v2` (new sessions, mutation-check the tests) and a fresh triage lead: (a) fix before release, (b) small fix, (c) later.
- Then cutover per design §5 with the user's go, pins: keep-it-simple, direct-concrete-communication, research-unfamiliar-before-relying, autonomy-within-scope, investigate-before-asking.
