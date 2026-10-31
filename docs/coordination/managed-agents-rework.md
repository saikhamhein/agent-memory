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

## Decisions and inputs

- 2026-11-01 user: do not trust the existing implementation; retrieval "dumps all". Verified causes: globals always qualify (query only reorders), budget caps text only and drops silently, MCP recall also returns full evidence as `structuredContent`, hook repeats the globals. Forwarded to architect as first-class design input.
- Prior art: unpushed branch `feat/recall-on-demand` @ `d125894` (worktree `.claude/worktrees/recall-on-demand`) from session `local_cc899943-24f5-416f-bb84-4b0a3e64223e`: recall start/task modes, text-only MCP recall, named budget omissions, hook in repo, `update-local.sh`. Architect evaluates it as an input, not a base.
- 2026-11-01 user confirmed: after every merged PR, fast-forward local master and reinstall without asking (rule in `AGENTS.md`; memory `agent-memory-reinstall-after-merge`, revision 9).

## Blockers

(none)

## Next actions

- Wait for the architect's design PR; review; merge; reinstall; staff workstreams from it.
