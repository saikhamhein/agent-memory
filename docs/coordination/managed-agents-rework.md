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
| Architect | `local_b602e858-1cf0-40c9-a15d-c78a09d3e1e3` (architect) | design started 2026-11-01 |

## Merged

(none)

## Blockers

(none)

## Next actions

- Wait for the architect's design PR; review; merge; staff workstreams from it.
