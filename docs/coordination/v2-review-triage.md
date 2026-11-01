# v2 review triage

Triage lead, fresh session, 2026-11-01. Code reviewed at `75fabfb` (`origin/rework/managed-agents-v2`; `service/` is unchanged since `3bdfd68`).

**Inputs:**
- Review 1 (store, migration, data safety): `review-store-migration/review-store-migration-findings.md`, ids R1-F*.
- Review 2 (recall, clients, hook): `review-recall-agent-surface.md`, ids R2-F*.
- Review 3 (security, API, simplicity): `review-security-api-simplicity.md`, ids R3-*.
- Design §3.8, the architect's proposals. Treated as one opinion; where I differ, the row says so.
- Coordinator finding T1 and dream handover items D1–D4.

All three reviews are in the shared notes folder.

**Settled decisions applied:**
- Ordinary sessions read and suggest only; only the daily dream holds the maintenance token (§3.4).
- Pinned changes stay autonomous, but need the user's own words as evidence, and every one is reported.
- If it can be removed, remove it.

## How I verified

- **Code reads:** every row below was checked at its cited `file:line` on `75fabfb`.
- **Probes:** I reran Review 1's probes p1–p3 (`probe.rs`) on a scratch copy of `service/`, with `TMPDIR` and `CARGO_TARGET_DIR` inside my worktree. All three still show the bad behaviour: the mirror wedge, the redact gap and the escaped pin.
- **Suites:** I ran the Rust suites the same way. All pass: store_v2 20, api_v2 21, migrate 13, migrate_store 2, recall_v2 20, service 3.
- **T1:** I measured the TempDir leak per suite (row X31).
- **Not rerun:** Review 1's v1-binary repro (X5) and Review 3's HTTP probes. Their claims are confirmed by code reading. I didn't read `~/.claude.json` or `~/.codex/config.toml`, so the claim that today's registrations pass `maintainer.json` rests on Review 3.
- **Upstream docs checked:**
  - `systemd.service(5)`: `Restart=on-failure` restarts only after a non-zero exit, a signal or a timeout.
  - `systemctl(1)`: `disable` removes the enable symlinks.
  - `systemd.special(7)`: the user manager starts `default.target`, and so every enabled `WantedBy=default.target` unit, when it starts.

  Together these confirm X5b. The SQLite, fsync and hook-cap facts I rely on are the reviewers' upstream citations; I didn't need any others.
- **Safety:** nothing touched the live store, `~/.config/agent-memory` or the running service. My temp dirs were deleted afterwards.

**Test-validity note (T1):**
- Review 1 reran its whole mutation table on disk; its results stand.
- Review 2 reran R1 and R4–R13 with an on-disk target. Its R2 and R3 results come from a run that compiled under a full `/tmp`, but they are "caught" results, so they aren't weakened.
- Review 3's probes are behavioural HTTP checks on a debug build. Each probe gave a definite answer, so a full `/tmp` didn't mask anything.

So no finding below depends on an invalid run.

## Findings

Classes:
- **a**: fix before release;
- **b**: small fix in this round;
- **c**: later.

Workstreams (WS-A, WS-H, WS-M, WS-C) are defined in the next section.

| Id | Sources | Summary | Verified | Class | WS |
| --- | --- | --- | --- | --- | --- |
| X1 | R2-F8, R3-B1 | MCP gives every ordinary session the maintenance token. `install-mcp.py:22,56-57` registers `--maintenance-config` by default; with it `mcp_server.py` exposes `memory_write`/`delete`/`redact`/`dream_adopt` on live. Violates the read-and-suggest ruling. Fix: the §4.1 five-tool adapter with no maintenance mode; `install-mcp.py` registers `client.json` only. | yes | a | C |
| X2 | new (triage) | The installer never mints the `agent-session` token. `install-user.py:103-117` still writes `agent-recall = [recall]` on a fresh install, and on an existing install (this machine) it changes no token at all (`else:` branch only checks `database`). After X1 lands, `client.json` holds `[recall]`, so `memory_observe` gets 403: sessions can no longer suggest. Fix: the installer reconciles tokens, see WS-M. | yes | a | M |
| X3 | new (triage), D1 | The daily dream and the skill drive dreams through MCP tools that the settled design removes. `ops/daily-memory-dream.md` (Method 5–6) and `SKILL.md` steps 3–7 call `memory_dream_start`, `memory_write(store=…)`, `memory_dream_get`, `memory_dream_adopt` and `memory_dream_cancel`. Once X1 ships, the next scheduled run fails. Fix: rewrite both for `agent-memory-client --config …/maintainer.json`, and with D1 the skill owns the procedure while the prompt keeps only schedule specifics (authorization, out-of-scope list, last-run time, report). The skill's capture section, which ordinary sessions read, stops naming `maintainer.json`. | yes | a | C |
| X4 | R3-S2 | The settled observation rule isn't in the dream prompt or the skill. §3.5 says the dream checks each observation's locator against the user's own words, drops one with no user message (noting it in `coverage`), and reports every pinned change on its own line. Neither doc says so. Provenance comes free with X1+X2: observations by sessions carry `token:agent-session`. No store rule (settled). | yes | a | C |
| X5 | R1-F1 | The old v1 binary can serve the migrated database. (a) `migrate` commits, then exits non-zero (post-commit `Store::open`, or `println!` on a broken pipe), and `install-user.py:63-68` restarts the old service. (b) Any step after migration fails (`check`, `atomic_copy`, `systemd-analyze verify`, `daemon-reload`): the unit stays enabled, so the next login starts v1 from `~/.local/bin`. v1 then serves frozen memories silently. Fix: disable the unit before migrating, decide by `user_version` rather than exit code (`< 2` → restart old; `≥ 2` → leave stopped and disabled, say "rerun install-user.py or roll back"), and enable only after the v2 binary and unit are installed. | yes (code + systemd docs; R1 repro not rerun) | a | M |
| X6 | R1-F2, R3-S5 | Two valid paths, one a file and one needing a directory of the same name (`/projects/a/b.md` and `/projects/a/b.md/c.md`), wedge the mirror for good: `model.rs:252-265` lets middle segments end in `.md`, and `mirror.rs:88-91` fails with EEXIST. Fix (agrees with §3.8): no segment except the last may end in `.md`. Migration already aborts and lists offenders. | yes (p1) | a | A |
| X7 | R1-F3, R3-S3, R1-F10c | Leaked secrets can't be removed. Every dream clones live into an output store (`store.rs:815-835`), archived stores refuse delete (`:65-71`), and redact refuses heads (`:761-765`). Observation bodies, also copied into `dream_jobs.input`, have no redaction. Fix (agrees with §3.8): on adopt or cancel, delete the output store's memory and version rows (this also ends the growth of one clone per day); diff works only while running. Add observation redact (`promote`), which nulls the body in `observations` and in every `dream_jobs.input` copy. | yes (p2) | a | A (store), H (route) |
| X8 | R3-S4 | The credential check misses common formats (`xox?-`, `AIza`, `github_pat_`, JWT, `Bearer …`, key-labelled 64-hex: `model.rs:182-193`) and skips dream `sessions` and the adopt `summary`, `coverage` and `conflicts`. Unknown `sessions` keys are stored. Fix as §3.8: one check on every stored string, and `sessions` items hold only `source`, `locator` and `sha256`. | yes (regex read) | b | A |
| X9 | R3-S6 | Recall tokens read the whole version history, including deleted memories: `api.rs:205-216` maps every GET on live to `recall`. §4.1 already says version reads need `review`; the code lags. One match arm. | yes | b | H |
| X10 | R3-S7a | Unbounded dreams: no limit on running dreams. Fix: at most one `running`, and a second `dream_start` gets 409 `dream_running`. This removes `observations_consumed` from the contract, the skill and the prompt. **Addition to §3.8:** a run that dies leaves a running dream, so the 409 must carry its id, and the prompt says "cancel the stale dream, then start". Without that, one crashed run blocks every later daily dream. | yes | b | A, C (docs) |
| X11 | R3-S7b | Recall calls `status()` only to read `mirror_dir` (`api.rs:302`), and `status()` decodes every observation and every adopted dream's input under the store mutex. Fix: one line. | yes | b | H |
| X12 | R1-F5 | An escaped JSON key (`"pinned"`) is pinned for recall but not counted by the budget: `store.rs:140` prefilters raw bytes. That breaks the "core always fits" guarantee. Fix: count from parsed memories. | yes (p3) | b | A |
| X13 | R1-F4 (M12) | No test has a v1 store with `active < max`, so the migration's final self-check is unguarded. Test only; the architect decides between refusing and migrating the active set (recommend refuse: v1 can't produce it). | yes (code) | b | M |
| X14 | R3 simplicity | Remove the adopt `reviewed` flag: the same caller sets it and it gates nothing. The body becomes `{summary, coverage, conflicts}`. | yes | b | A, C |
| X15 | R3 simplicity | Remove `project_id`, `[projects]` and `--project-id`; no caller sends them. **Coupling:** `Config` has `deny_unknown_fields`, and this machine's v1 `server.toml` has a `[projects]` table. Dropping the field without the installer stripping it makes `check` fail after migration, which is X5's path. So WS-M strips `[projects]` in the same token reconcile (X2), and WS-H's config change merges only after that. | yes | b | H, M, C |
| X16 | R3 simplicity, R1-F6 | Remove the `mount` key; the mirror is always `<database dir>/mnt`. Also fixes `migrate` ignoring it (R1-F6). The hook's lookup simplifies. | yes | b | H, C |
| X17 | R3 simplicity | Remove the `agent-memory-server token` subcommand; the installer makes its own tokens. | yes | b | M |
| X18 | R3 simplicity | `recall::project_root` duplicates `model::absolute_path`, and the API already normalizes `project`. Remove it. | yes | b | H |
| X19 | R2-F4 | A project memory's scope path counts as title terms (`recall.rs:229`), so "mesh" or "projects" matches every memory of that project. Fix: leave the scope value out of `head` for project memories. Topic and role names stay, since they're useful terms. | yes | b | H |
| X20 | R2-F1 (P7) | MCP `memory_recall` without `project` drops every project memory (`in_scope`, `recall.rs:88-99`), and nothing tells agents to pass it. Fix: the docstring and instructions say "pass the canonical project root", plus a test that sends `project` through MCP. Not relying on the MCP server's cwd, which is unverified. | yes | b | C |
| X21 | R2-F2 | Only the hook canonicalizes worktrees. Codex worktrees (`~/.codex/worktrees/…`) and the CLAUDE.md `--project "$PWD"` fallback get no project memories. Fix: move `canonical_root` into `client.py`'s recall (the hook calls the client anyway). | yes | b | C |
| X22 | R2-F5 | `memory.py` prints the project memories twice (start view, then task view). Fix: drop task-view blocks already in the start view's `returned`. | yes | b | C |
| X23 | R2-F6, R2-F7, R2-F9 | Dream doc errors: observations aren't in the output store (they're in the dream's `observations` list; write each one kept); slugs are unique across the store (prefix project slugs; handle `409 slug_exists`); the recall-usage step must come before the dream. All fold into X3's rewrite. Keep the store-wide slug rule: memories and prompts refer to each other by slug (`self-improve-memory-setup`). | yes | b | C |
| X24 | D2 | Every run reviews the full memory set: always start a dream, and also scan for duplicates, stale entries and failed triggers. Adopt when the diff is non-empty **or** the input held observations (an empty adopt is how rejected observations stop being pending). Otherwise cancel. This replaces the "quiet day, start no dream" step. | n/a (doc) | b | C |
| X25 | R2-F12b | `role` and `topics` on MCP `memory_recall` only affect the start view, which the same docstring says not to request. Drop them from the five-tool set. | yes | b | C |
| X26 | R2-F14 | The hook's fallback banner still points at the service's tools. Add "use the mirror files until it is back". | yes | b | C |
| X27 | R3 simplicity | `catalog.py:61` writes a timestamped `catalog-*.json` every run, and nothing reads them. Remove. | yes | b | C |
| X28 | R3-L3 | `install-mcp.py:41-46` copies `~/.codex/config.toml` into `review/` on every run (after every merged PR). Keep one fixed-name copy under `runtime/`, overwritten each run. | yes | b | C |
| X29 | R1-F9 | §5 rollback says only observations are lost. Every v2 write after cutover is lost: edits, deletes, adopted dreams. See runbook deltas. | yes | b | architect (doc) |
| X30 | R1-F11 | `mirror_swap_atomic` (`store_v2.rs:1729-1779`) hangs forever if the writer panics. Give the reader loop a deadline and a stop flag set on writer exit. | yes (code) | b | A |
| X31 | T1 | Test TempDirs leak. Measured: store_v2 0, api_v2 28 dirs (82 MB), migrate 8, migrate_store 2, recall_v2 and service 0, all per passing run. Cause: `api_v2.rs:99-116` (`Fx`) and the migrate tests hold a raw `TempDir` around a sealed mirror. `TempStore` already unseals on drop (`common/mod.rs:73-78`). Fix: one shared `SealedTempDir` that unseals on drop, used by every suite. Production is fine: `mirror::remove` unseals before deleting. | yes (measured) | b | A (helper), H and M (adopt it) |
| X32 | R1-F7 | The mirror has several writers (`check`, `migrate` and the service) with no lock. Not reproduced by Review 1; narrow install window. | no | c | — |
| X33 | R1-F8 | The mirror isn't fsynced, and `is_current` can't detect a torn generation after power loss. Self-heals on the next write. | yes (code) | c | — |
| X34 | R1-F10a,b | `dream_adopt` doesn't enforce `MAX_MEMORIES`; `dream_start`, `dream_adopt` and `dream_cancel` ignore `_actor`. | yes | c | — |
| X35 | R1 M13, M18–M20 | Uncovered mutants: EXCLUSIVE migration, keep-previous generation, same-revision spare, INDEX refresh. | yes (R1 table) | c | — |
| X36 | R1 research | `backup` opens the source read-only and may refuse with a hot journal after a crash. It fails safely, before migrating. | no (not tested) | c | — |
| X37 | R2-F3 | The match rule misses 2-letter terms (`ui`), single body words, inflections and unsegmented Burmese. These are deliberate rules pinned by tests. Tune them from a week of `recall-log.jsonl` task views with no result, not by guess. | yes | c | — |
| X38 | R2-F10 | The task view never searches pinned memories, and says "No memory matches". Deliberate. | yes | c | — |
| X39 | R2-F11 | Non-pinned global rules load only as index lines. This is the design. It makes the cutover pin set a real choice; see runbook delta 1. | yes | c | runbook |
| X40 | R2-F12a,c | `--limit`, `--budget` and `--json` shims, the `memory.py` shim, and `START_MAX` duplicated in the hook. The shims stay until the loader lines in the user's files change; the `START_MAX` drift is harmless. | yes | c | — |
| X41 | R2-F13, R2-F15 | Index lines interleave topics; role and topic names are case-sensitive. | yes | c | — |
| X42 | R3-S1 | Same-uid shell can read `maintainer.json`. The threat model is now written in §3.4 and accepted. No action. | yes | c | — |
| X43 | R3 simplicity | `/healthz`. Removing it would make WS-H and WS-C (`harness.py`) touch each other's files for two lines. Later. | yes | c | — |
| X44 | R3 simplicity | The `Document` decoder and `arbitrary_precision`, needed only by `migrate`; and the legacy `revisions`, `state` and `dreams` tables, needed for the rollback window. **Addition (T4):** the legacy tables also keep v1 history that no API can redact, and so does the pre-v2 backup. Drop both when the rollback window closes. | yes | c | — |
| X45 | R3-L1, L2, L4, L5, L6 | Anyone can set the recall-log `client` label; query text is logged forever; no HTTP timeouts (loopback only); file modes (`memory.sqlite3` 0644, home is 0700); no hash pins. | yes | c | — |
| X46 | R3 simplicity | Trim design doc §1 and §4 (build history) after cutover. | n/a | c | — |
| X47 | D3, D4 | A shared session-reading script for the three transcript formats; an id-level dream diff summary, and the last dream time from `GET /v1/dreams`. The prompt already reads the last time from `GET /v1/dreams`, with a read-only legacy fallback, and `adoption` already lists the paths. New features: later. | n/a | c | — |

**No action** (checked and fine by the reviewers, and not contested here):
- R3-L7 and R3-L8;
- R3-S2's "refuse observation-only pinned additions", settled by the coordinator;
- the sound areas each review lists.

**Counts:**
- (a): 7, X1–X7;
- (b): 24, X8–X31;
- (c): 16, X32–X47.

**Removals adopted from Review 3's list:**
- this round: `reviewed`, `project_id`/`[projects]`, `mount`, the `token` subcommand, `recall::project_root`, `status()` in recall, extra session keys, `catalog-*.json` and the per-run Codex backup;
- later: `/healthz`, the shims, the decoder and the legacy tables (the last two when the rollback window closes) and the design-doc trim.

## Workstream plan

Staffing order:
1. **First, the architect writes one contract PR to the design doc** (list below). The four workstreams build against it in parallel, each with a test engineer whose tests fail on the integration head first.
2. **Merge order:** A → H → M → C. H needs A's store functions. M's tests reuse A's helper. C needs H's routes and M's tokens.
3. **Checking:** every test run uses `TMPDIR` on disk, and the coordinator checks `df /tmp` before combined runs.

### Architect: contract changes (design doc only)
- **§3.1 path rule:** no segment except the last ends in `.md` (X6).
- **§3.5 dreams:**
  - one running dream; 409 `dream_running` with `paths: [<dream id>]` (X10);
  - remove `observations_consumed`;
  - adopt and cancel purge the output store's rows, and diff only while running (X7);
  - adopt body `{summary, coverage, conflicts}` (X14);
  - the D2 adopt-or-cancel rule (X24);
  - credential check on every stored string, and closed `sessions` keys (X8).
- **§4.1 API:**
  - `POST /v1/observations/{id}/redact` (`promote`);
  - recall body without `project_id`;
  - config without `mount` and `projects`;
  - no `token` subcommand.

  Decide whether removed request keys are rejected (400) or ignored. I recommend 400, consistent with `deny_unknown_fields`.
- **§3.6 installer:**
  - token reconcile (X2);
  - unit disabled during migration and decision by `user_version` (X5);
  - config backup before rewrite.
- **§3.8:** mark each item resolved, citing this file.
- **§5:** the runbook deltas below.
- **X13:** refuse a v1 store whose `active` isn't `max(id)`.

### WS-A: store core and mirror
- **Owns:** `service/src/{store,model,mirror,lib}.rs`, `service/src/schema_v2.sql`, `service/tests/store_v2.rs`, `service/tests/common/mod.rs`.
- **Items:** X6, X7 (store side: purge, observation redact function), X8, X10, X12, X14 (store side), X30, X31 (the helper).
- **Failing-first tests:**
  - `path_md_middle_segment_rejected`: `/projects/a/b.md/c.md` → 400. After the p1 sequence, `mirror_stale` is false.
  - `archived_output_store_purged`: seed a secret, then start and cancel, and separately start and adopt. Afterwards the output store has no memories or versions, `get` returns 404, and diff on an ended dream returns 400. Live `dream:<id>` versions and `adoption` remain.
  - `observation_redact_nulls_every_copy`: observe, start a dream, redact. The `observations` body and the `dream_jobs.input` copy are null; id, time and `created_by` are kept.
  - `one_running_dream`: a second start → 409 `dream_running` naming the first; after cancel, a start succeeds.
  - `escaped_pinned_key_counts`: Review 1's p3, inverted.
  - `credential_check_everywhere`: a table of the X8 formats against content, a `sessions` locator and the adopt `summary`/`coverage`/`conflicts`; an extra `sessions` key → 400.
  - `adopt_body_has_no_reviewed`.
  - `mirror_swap_atomic` gets a deadline: a writer panic fails the test inside 30 s.
- **Reusable helper:** `SealedTempDir` in `common/mod.rs`, a `TempDir` that unseals the tree on drop. `TempStore` holds one. Plus `assert_no_leftovers(tmpdir)` for the end of a suite run, so a leak is a failure.

### WS-H: HTTP API, config and recall
- **Owns:** `service/src/{api,config,recall}.rs`, `service/tests/api_v2.rs`, `service/tests/recall_v2.rs`, `service/tests/fixtures/recall/`.
- **Items:** X7 (redact route), X9, X11, X15 (API and config side), X16 (config side), X18, X19, X31 (`Fx` uses `SealedTempDir`).
- **Merge gate:** the X15 config change merges only after or with WS-M's reconcile.
- **Failing-first tests:**
  - `version_reads_need_review`: a recall token gets 403 on GET live `memory_versions` and `memory_versions/{id}`.
  - `observation_redact_route`: `promote` → 200, `review` → 403, unknown id → 404.
  - `recall_rejects_project_id` (or ignores it, per the architect).
  - `config_has_no_mount_or_projects`: either key fails `Config::load` with a message.
  - `project_words_do_not_match`: in a project with 5 memories, a query made only of its path words returns `No memory matches`.
  - The api_v2 suite leaves nothing in `TMPDIR`.
  - X11 has no behavioural test; review it from the diff (one line).

### WS-M: migration, install and cutover tooling
- **Owns:** `service/src/{main,migrate}.rs`, `service/install-user.py`, `service/tests/{migrate.rs,migrate_store.rs,legacy/mod.rs,test_install.py}`, `update-local.sh`, `service/deploy/*`.
- **Items:** X2, X5, X13, X15 (strip `[projects]`), X17, X31 (the migrate tests use `SealedTempDir`).
- **Token reconcile** (fresh install and existing `server.toml` alike, idempotent):
  1. Back up `server.toml` and `client.json` to `runtime/` (0600, `create_new`).
  2. Ensure `agent-session = [recall, observe]` exists, with a new secret written to `client.json`.
  3. Remove `agent-recall` and `[projects]`.
  4. Leave `memory-maintainer` and `maintainer.json` untouched.
  5. Write each file atomically. The service restart at the end picks up the change.
- **Failing-first tests** (`test_install.py`):
  - `migrate_commits_then_fails_keeps_v1_off`: the shim runs the real migrate, then exits 1. Expect `disable` before migrate, no `start` or `restart` after, the unit not enabled, and an error naming "rerun install-user.py or roll back".
  - `failure_after_migration_leaves_unit_disabled`: make fake `systemd-analyze` fail. Expect the unit disabled, not started.
  - `failure_before_commit_restarts_v1`: the existing OFFENDING case, still restarts.
  - `existing_config_gets_session_token`: a v1-shaped `server.toml` with `agent-recall` and `[projects]`. Afterwards there is an `agent-session` with exactly recall and observe, no `agent-recall`, no `[projects]`, `client.json` matching the new hash, `maintainer.json` byte-identical, and backups in `runtime/`. A second run changes nothing.
  - `fresh_install_mints_session_token`.
  - `token_subcommand_removed`.
- **Failing-first tests** (`migrate.rs`): `refuses_active_below_max` (X13). The suites leave nothing in `TMPDIR`.
- **Reusable helpers:**
  - in `test_install.py`, a `crash_after_commit` fake `agent-memory-server` that execs the real binary for `migrate` and then exits non-zero;
  - `fail_tool(name)`, which makes any fake system tool exit 1.

  Together these form the crash-mid-install harness for every post-commit step.

### WS-C: clients, hook and dream docs
- **Owns:** `service/{mcp_server,client,install-mcp}.py`, `service/tests/{test_mcp,test_client,harness}.py`, `memory.py`, `test_memory.py`, `hooks/agent-memory-recall.py`, `catalog.py`, `skill/dream-memory/SKILL.md`, `ops/daily-memory-dream.md`, `README.md`, `service/README.md`, `AGENTS.md`.
- **Items:** X1, X3, X4, X10 (docs), X14 (client), X15 (`--project-id`), X16 (hook), X20–X28.
- **Research first:** how to re-register Claude Code's user-scope MCP server (`claude mcp add/remove --scope user` vs editing `~/.claude.json`). Runbook step 3.6 depends on it.
- **Failing-first tests:**
  - `mcp_has_session_tools_only`: `list_tools` is exactly the five §4.1 names, and `--maintenance-config` is rejected.
  - `install_mcp_registers_session_only`: fake `codex` records args; `maintainer.json` is absent, and one fixed backup is kept.
  - `mcp_recall_with_project` (P7): a project memory comes back only when `project` is passed.
  - `client_canonicalizes_worktree`: a git worktree outside the repo → the main checkout's memories.
  - `memory_py_no_duplicate_blocks`.
  - `hook_fallback_without_mount`, with the new banner.
  - `catalog_writes_no_timestamped_copy`.
  - `docs_reference_real_commands`: parse `SKILL.md` and `ops/daily-memory-dream.md` for `memory_*` names and `agent-memory-client` subcommands and flags, and fail on any the adapter or client doesn't define. This catches X3 now and any later drift.
  - `dream_cycle_via_cli` on `harness.py` with the maintainer config: observe → start → write the kept observation → diff → adopt; a second start → 409 `dream_running`; a run with no change and no observations cancels.

## Cutover runbook deltas (design §5)

1. **Step 2, pin review.** The dry-run report also lists the global rules that will load only as index lines (X39). The coordinator and the architect settle the pinned set knowing that list. Run the dry-run with `TMPDIR` on disk.
2. **Steps 3.2–3.4 run back to back, by the coordinator only, outside the daily dream's window (05:25).**
   - Master's current AGENTS.md "after every merged PR" lists `install-mcp.py` separately. If another session ran it between the merge and the migration, it would register the v2 adapter against the v1 service.
   - From the moment master is pulled, `memory.py` is the v2 stub, so the Codex and DeepSeek loaders fail until migration.
   - Tell other sessions not to run the post-merge routine for this merge.
3. **Step 3.4.**
   - The installer now disables the unit before migrating and decides by `user_version` (X5).
   - If it fails after the commit, the service is stopped **and disabled**. The operator either reruns `install-user.py` without `--migrate` (the store is v2) or rolls back. Never `systemctl --user start` by hand.
   - The installer also reconciles tokens and backs up `server.toml` and `client.json` to `runtime/` (X2).
4. **Step 3.5** stays, and now registers `client.json` (`agent-session`) only. **Step 3.6** (Claude Code re-registration) uses the mechanism WS-C verified.
5. **Step 3.7, verify.** Add:
   - `mirror_stale == false`;
   - `server.toml` lists `agent-session` with exactly recall and observe, and no `agent-recall`;
   - a live write with `client.json` returns 403.

   Don't create a test observation in the live store: the next dream would consume it.
6. **Step 3.8.** After copying the prompt, trigger one supervised run of the daily dream and read its report. It is the first run on CLI-only dream tooling (X3) and with the full-review rule (X24).
7. **Step 4, rollback.**
   - **What is lost:** every v2 change after cutover (memory edits, deletes, adopted dreams and observations), not only observations (X29). Before restoring, list them with `agent-memory-client versions` (since cutover) and keep the set-aside database.
   - **Set aside `mnt/` too:** move it next to the migrated database, then run `chmod -R u+w` before deleting it, because the mirror is 0555/0444.
   - **Restore config:** `server.toml` and `client.json` from `runtime/`, then the Claude Code MCP entry and the Codex registration. v1 `install-mcp.py` re-registers Codex.
8. **After the rollback window** (set a date at cutover): drop the legacy tables and delete `runtime/pre-v2-*.sqlite3`. Until then they hold v1 history that redact can't reach (X44).
