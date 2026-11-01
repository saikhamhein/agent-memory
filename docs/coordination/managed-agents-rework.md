# Managed Agents alignment rework — coordinator state

Goal (user, 2026-11-01): rework this repository's memory store and dream workflow using Anthropic's Managed Agents [memory](https://platform.claude.com/docs/en/managed-agents/memory) and [dreams](https://platform.claude.com/docs/en/managed-agents/dreams) design as the source idea.

Coordinator: session in worktree `managed-agents-memory-dreams-622f97` (branch `claude/managed-agents-memory-dreams-622f97`). Coordinates only; teammates implement. Teammates send the coordinator an approved PR number and head SHA; the coordinator merges.

## Resume here (read this first after a compaction)

- **Phase:** CUTOVER DONE 2026-11-01 12:24 +08. Master `ae966d9` (PR 30). Live store v2 at revision 49: 25 memories, 5 pins (2,826 chars), mirror at `mnt/shared-agent-memory`. Backups in `runtime/cutover-20261101/`: pre-v2 db, config, Codex config, old hook, old task prompt, Claude MCP entry and the master SHA `83ff1f7`; the installer's `runtime/pre-v2-20261101T042421863922Z.sqlite3`. The rollback window closes 2026-11-15. Verified: status, tokens (agent-session = recall+observe), 403 on a session-token write, hook 4,470 chars via service, Codex and Claude MCP registered with client.json only, MCP has 5 tools with text-only recall, loader line, unrelated query gives no match. Task prompt replaced with `ops/daily-memory-dream.md`; the supervised dream run was denied by the auto-mode classifier; left for the user ("Run now" in the app) or the 05:25 run. Before cutover: fix round. Merged into `rework/managed-agents-v2`: R (PR 24), M (PR 25), A (PR 26), H (PR 28). All five fix workstreams merged. Combined check at `915d491` (release): 98 Rust tests, 51 Python tests, clippy clean, TMPDIR empty. Fresh dry run on a backup-API copy of the live store at legacy revision 12: exit 0, copy byte-identical, 25 memories, 49 versions (29 created, 16 modified, 4 deleted), five pins at 2,826 of 4,000. Next: the user's go for the cutover; then (eng C, branch `claude/ws-c-fix`). M's X31 follow-up (PR 29) is merged. Then the combined check, a fresh dry run on a live-store copy with TMPDIR on disk, and the cutover per design §5 with the user's go.
- **Done 2026-11-01:** the first v2 dream (`drm_9060835…`, user's Run now) read 22 changed sessions, reviewed all 25 memories, found nothing to change and cancelled cleanly, with no dream left running and a healthy recall log. All 15 teammate sessions were archived.
- **Remaining:** after 2026-11-15, if there's no rollback, drop the legacy v1 tables and delete `runtime/pre-v2-*.sqlite3` and `runtime/cutover-20261101/memory.pre-v2.sqlite3` (Δ8); this needs the user's go, because it deletes history. Later (class c): the 16 triage items and Tailscale access.
- **Merge routine:** read the diff, check it's current with the integration head (or test a local merge), run all suites with `TMPDIR` and `CARGO_TARGET_DIR` under this worktree (`.check-*`, `.target`), then `gh pr merge N --merge --match-head-commit SHA`. Remove the check worktree with `chmod -R u+w` first, because the mirror is read-only. For C, also check that the real `~/.claude.json` `mcpServers` and `~/.codex/config.toml` are unchanged.
- **Rules learned:**
  - The user's repo authority covers pushing and merging teammates' reviewed commits; don't ask.
  - The app pauses cross-session sends after 10 in a row without a user message.
  - Sessions can be started only from a message the user types in the desktop app.
  - The user wants context kept lean: offload to this file, compact at checkpoints.
- **Live system:** still v1 at revision 11. The MCP adapter still holds the maintainer token (the user chose to wait for the cutover). Don't touch it before the cutover.

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

## Review campaign (started 2026-11-01, on `3bdfd68`)

| Reviewer | Session | Focus |
| --- | --- | --- |
| Review 1 | `local_468439bd-2e59-4b40-a112-732bf9532bfe` | store, migration, data safety |
| Review 2 | `local_33b6d546-b905-477f-946d-24f8b5f4c4c9` | recall, clients, hook end to end |
| Review 3 | `local_4af086ca-a946-4a23-91b8-80c448967abd` | security, API surface, simplicity |

Reports:
- Review 3 done: 1 blocker (default MCP install registers the maintainer token for every session, so any injected session can write, delete, redact or adopt live memory), 7 should-fix, 8 later, 14 removable items. File: `/tmp/claude-1000/-home-sai-Projects-agent-memory--claude-worktrees-sleepy-antonelli-01c285/ec71ef4b-fcb2-4f2d-9792-397bfe038dc0/scratchpad/review-security-api-simplicity.md`, with a copy in the shared notes folder. The coordinator found the same blocker independently and asked the user whether ordinary sessions should be read-and-suggest only.

- Review 1 done (store, migration, data safety). File in the shared notes folder: `review-store-migration/`.
  - Should-fix, borderline blocker: migrate can commit and still exit non-zero, and the installer then restarts the old v1 binary on a v2 database (F1).
  - Should-fix: a file and directory pair wedges the mirror (F2); redact can't reach dream output stores (F3).
  - Later: 7 smaller items.
  - Mutations: 17 of 22 caught. Missed: migration self-check, EXCLUSIVE→DEFERRED, previous generation deleted at once, same-revision rebuild, is_current ignoring INDEX.md.
- Review 2 done (recall, clients, hook end to end): `review-recall-agent-surface.md` in the shared notes folder. Unrelated queries return nothing; the pinned core is always present; the hook stays under 10,000 characters (9,420 at 350 memories). Should-fix:
  - MCP recall drops project memories unless `project` is passed (F1);
  - Codex worktrees get no project memories (F2);
  - observations are silently consumed (F6);
  - plus F4, F7, F10–F13.
- Coordinator finding: Rust tests leak their TempDirs, because the mirror is 0444 inside 0555 directories, so drop can't delete them. About 2,000 dirs (4.9 GB) piled up in /tmp, and /tmp hit 100%. Review 1 reran with TMPDIR on disk; results from that window may be invalid. Fix: the test helper unseals before drop.
- Dream handover from session `local_75db75dc-b488-4b0b-ba64-59e58d5c83fd` (2026-11-01; it had not seen the v2 work, so the user said to weigh it with caution). Items for triage:
  1. the skill owns the procedure, and the task prompt keeps only run specifics (authorization, out of scope, quiet day, recall-usage check, report);
  2. review the full memory set every run, and adopt only when something changes;
  3. a session-reading script (Claude, Codex and DeepSeek formats; files that catalog.py marked changed);
  4. last dream time from `GET /v1/dreams`, and an id-level diff summary;
  5. maintenance via the CLI with files, which PR 18 already settles;
  6. the task prompt hard-codes v1, so the cutover must copy `ops/daily-memory-dream.md` into the task in the same step (design §5 step 8).
  The coordinator keeps 1 and 2 on its own judgment (one source of procedure; stale memories cleaned on quiet days), so they land in `ops/daily-memory-dream.md` and the skill before cutover. Items 3 and 4 are advisory and the triage lead decides them.
  Live task: `daily-memory-dream` at 05:25 local, prompt `~/.claude/scheduled-tasks/daily-memory-dream/SKILL.md`.

- 2026-11-01: with the user's go, the coordinator cleaned the leaked /tmp/.tmp* test dirs and the idle sessions' build caches. /tmp went from 97% to 38% used.
- Triage lead `local_b9c54bf3-2f93-4fb2-ba8b-c5d965ed3970` started. Deliverable: `docs/coordination/v2-review-triage.md` as a PR to `rework/managed-agents-v2`.

Then a fresh triage lead classes the findings: (a) fix before release, (b) small fix, (c) later.

## Fix round (started 2026-11-01, from triage PR 20)

Triage: 47 findings, 7 (a), 24 (b), 16 (c), in `docs/coordination/v2-review-triage.md`. The architect writes one contract PR. Pairs reuse the existing sessions:

| WS | Engineer | Test engineer | Test branch |
| --- | --- | --- | --- |
| A store + mirror | eng A | test A | `rework/tests-fix-a` @ `d39084d` (12 of 28 failing as intended; 28/28 with scratch fixes) |
| H API + config | eng H | test H | `rework/tests-fix-h` @ `5c81ac4` (6 failing on 1b7abfe) |
| H recall part | eng R | test R | `rework/tests-fix-r` |
| M migration + install | eng M | test M | `rework/tests-fix-m` @ `0337498` (6 install + 1 migrate failing on 1b7abfe) |
| C clients, hook, dream docs | eng C | test C | `rework/tests-fix-c` |

Merge order: R, M and A merged; H → C next. Removed request keys are rejected with 400. The stale-dream rule is the architect's call.

## Merged

- Checked: the API rejecting `limit`/`budget` is safe. The client CLI and stub accept and ignore them (design §3.3, §4.1), and `stub_forwards_recall` runs the exact loader line.
- PR 27 (fix round, C: X1 five-tool session MCP, X3/D1 skill owns the CLI dream cycle, X4 observation evidence check and pinned-change report, D2 full review every run, the stale-dream rule, Claude Code re-registration via `claude mcp`) → `rework/managed-agents-v2`. Coordinator check at 5692b26 (release build): 51 Python tests and 97 Rust tests pass; the real `~/.claude.json` mcpServers and `~/.codex/config.toml` are unchanged; the live service is active. Note (c): `test_recall_canonicalizes_worktrees…` assumes TMPDIR is outside any git repo.
- PR 29 (fix round, M X31: migrate tests use SealedTempDir, no leftovers) → `rework/managed-agents-v2`. Coordinator check on the merged result: 98 Rust tests, TMPDIR empty, clippy clean.
- PR 28 (fix round, H: X7 redact route, X9 version reads need review, X11, X15/X16 no project_id/mount/projects, X31) → `rework/managed-agents-v2`. Coordinator check at 5055e15: 97 Rust tests, clippy and fmt clean.
- PR 26 (fix round, A: X6, X7 store side, X8, X10, X12, X14, X30, X31 SealedTempDir, secure_delete) → `rework/managed-agents-v2`. Coordinator check at 1183da5: 91 Rust tests, clippy and fmt clean, TMPDIR empty.
- PR 25 (fix round, M: X2 token reconcile, X5 unit disabled with the decision by user_version, X13, X15, X17) → `rework/managed-agents-v2`, merged ahead of A and H because it depends on neither. Coordinator check on the merged result: 82 Rust tests (release), 10 install tests, clippy clean; the v1 guard runs before any config write. X31 (SealedTempDir in migrate.rs) follows after A.
- PR 24 (fix round, R: X18, X19) → `rework/managed-agents-v2`. Coordinator check at 220ee02: 81 Rust tests, clippy clean, TMPDIR left empty.
- PR 21 (fix-round contract: X1–X14, the stale-dream rule in the skill (cancel a dream running over 1 h, otherwise report), installer disable plus user_version, token reconcile, §5 Δ1–Δ8) → `rework/managed-agents-v2` `fdc326a`.
- PR 20 (triage) → `rework/managed-agents-v2` `1b7abfe`.
- PR 17 (WS-C clients, MCP, hook, stub, skill, docs) → `rework/managed-agents-v2`. Coordinator check at head d4633e6 with a release build: 33 Python tests (28 C + 5 install), 79 Rust tests; live service untouched. All five workstreams are merged.
- PR 15 (WS-M migration + install) → `rework/managed-agents-v2`. Coordinator check at head 4b243e4 (release): 79 Rust tests, 5 install tests (sandboxed HOME, fake systemctl), clippy and fmt clean; M's files unchanged since the architect's 4445976 approval; live service untouched.
- PR 14 (WS-H HTTP API) → `rework/managed-agents-v2` `8fbb862`. Coordinator check at head 1eeb4eb: 64 Rust tests, clippy and fmt clean; api.rs/config.rs unchanged since the architect's e548fe2 approval.
- PR 9 (WS-R recall) → `rework/managed-agents-v2` `ef2f07e`. Coordinator reviewed R's blocked local commit, merged in PR 16, re-ran 43 tests + clippy + fmt, pushed as `0ffb46a`, merged.
- PR 11 (WS-A store core + mirror, architect-approved at 529c2d5) → `rework/managed-agents-v2` `76e85fd`. Coordinator check on the merged result: 23 Rust tests pass, clippy -D warnings and fmt clean; placeholders for recall.rs/api.rs/main.rs labelled with owners. Python suites are expected red until C.
- PR 18 (read-and-suggest-only: `agent-session` token, 5 MCP tools, maintenance via the CLI with maintainer.json, version reads need review, threat model, §3.8 Review 3 contract proposals pending triage; doc only) → `rework/managed-agents-v2`.
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
- 2026-11-01 user product ruling: ordinary sessions read and suggest only. The default MCP install gets recall and observe under an observe-only credential; the maintenance credential goes only to the daily dream; corrections become observations applied by the next dream. The architect is preparing the doc PR. The user chose to keep the live v1 MCP registration (maintainer tools) unchanged until cutover.
- Coordinator ruling on S2 (should pinned changes need the user's request?): no. The user's standing instruction is that dreams run fully autonomously and never leave proposals for approval. So the daily dream may change pinned memories only on evidence of the user's own words, and reports every pinned change (§3.5).
- Architect ruling: A's fix PR carries the minimal edits to 7 api_v2 tests that send `reviewed` or start a second dream, so the integration branch stays green at A's head.
- Sent after the user's message: test C's brief (with PR 21–23 rulings) and the test M ruling (a pre-commit failure restores the old service and re-enables the unit).
- Coordinator ruling: `PRAGMA secure_delete = ON` on every store connection, plus a test that a redacted secret is gone from the file bytes. No VACUUM. The architect agreed. Cutover note: secure_delete covers only the live database; the pre-v2 backup in runtime/ and the legacy v1 tables keep old text until the rollback window closes (Δ8, X44).
- 2026-11-01: the user asked whether the extra Anthropic-style features are worth it. Coordinator verdict, as given to the user: keep history, the mirror and the migration; the dream copy-and-merge is a close call, kept because the fixes shrink it; add no further Anthropic features. The fix round continues.

## Blockers

- Test engineer C's fix-round brief was not delivered: the app pauses cross-session messages after 10 in a row with no user message. Resend it after the user's next message.
- Review campaign note: test-C's 28 tests passed first try; the fresh-eyes review should mutation-check them.
- Architect session's outgoing messaging paused again (2026-11-01, second time); rulings for H/test-H and the PR 10 notice undelivered. Needs the user to resume it there. The coordinator does not relay around the pause.
- Proposed card `task_8b1e5f32`: text-only MCP recall quick win on master.

## Next actions

- Fresh-eyes review campaign of `rework/managed-agents-v2` (new sessions, mutation-check the tests) and a fresh triage lead: (a) fix before release, (b) small fix, (c) later.
- Then cutover per design §5 with the user's go, pins: keep-it-simple, direct-concrete-communication, research-unfamiliar-before-relying, autonomy-within-scope, investigate-before-asking.
