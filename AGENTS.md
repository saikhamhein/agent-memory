# Shared agent memory project

This project implements a local persistent memory store and on-demand dream workflow shared by Codex, Claude Code and DeepSeek Harness.

Read README.md for architecture and commands. Run `python3 -m unittest -v test_memory.py` for the storage tests.

The optional Rust HTTP service is in `service/`; see `service/README.md` for API, systemd, and portability details. Run `cargo test --locked --manifest-path service/Cargo.toml` and, after a release build, `python3 -m unittest -v service/tests/test_client.py` for service and real HTTP client tests. HTTP tests use isolated temporary stores.

## Current handoff

- This machine uses `/home/sai/Projects/agent-memory`. Remote handoff details for `/home/sai/probe/agent-memory` describe a different PC and are not this store.
- On 2026-10-29 the user requested pulling the remote and migrating to the new service/MCP system. Fast-forwarded to `20bc640`. Installation and validation completed; see `runtime/migration-current` for the backup directory.
- 2026-11-01 (later): revision 8, 26 memories. Added `memory-maintenance-autonomous`, merged role-scoped `coordinator-role` into the global `coordinate-a-team-playbook`, and copied CLAUDE.md "Talking to me" rules into two global memories for Codex/DeepSeek. The `daily-memory-dream` task had dropped out of the desktop app's scheduler (last run 2026-10-30); it is re-registered at 05:25 local in Auto mode with a fully autonomous prompt (no proposals; instruction-wording fixes go into memories). The old prompt is backed up at `runtime/daily-memory-dream.SKILL.pre-20261101.md`.
- 2026-11-01: revision 7, 26 accepted memories. Claude Code also has the `shared_memory` MCP tools and its global instructions prefer them, with CLI fallback. Every recall (API or CLI) appends to the ignored `recall-log.jsonl` next to the database; the SessionStart hook labels its baseline recall `client: session-start`. The daily `daily-memory-dream` scheduled task reports recall usage from both logs.
- Pre-migration state: revision 4, 23 accepted memories, 7 observations, 4 dreams. A consistent SQLite backup, logical dump and the previous local instructions are saved in that directory.
- Global CLI loaders and dream-memory skill links were already active for Codex, Claude Code and DeepSeek Harness. Claude also has a SessionStart recall hook. Preserve these integrations; installation records are in `installation-status.json` and `adapter-install-plan.json`, so recheck destination hashes before any reinstall.
- The initial review parsed 760 local logs (653 unique provider/session identities), with semantic review focused on correction/workflow passages; do not claim exhaustive semantic reading. Original evidence and revision history must be preserved.
- The SQLite database remains authoritative. Use proposal/diff/promotion for accepted memory changes; do not rewrite old evidence or grant permissions based on a memory. The only recurring dream is the `daily-memory-dream` scheduled task. Original provider transcripts stay at their original locations.

- The user systemd service is enabled and active at `127.0.0.1:8787`, using this checkout's existing database. Journaling migrated from WAL to DELETE; the full logical database dump matches the pre-migration backup and integrity_check passed.
- Codex `shared_memory` MCP is registered with ten tools; startup instructions prefer MCP with CLI fallback. Installed-adapter protocol checks returned revision 4 and 11 recall results. Native desktop discovery still requires verification after reloading/restarting Codex; tools were not available in the migration chat. Claude and DeepSeek retain their working CLI access to the same database.
- Validation on upstream `20bc640` (only local documentation changed): 11 Python storage tests, 10 Rust integration tests, 2 HTTP client tests, 3 installer tests and 4 MCP protocol tests passed. Installation tests were initially invoked before the release binary existed; they passed after building. No test writes touched the live store. Release build succeeded.
