# Shared agent memory project

This project implements a local persistent memory store and on-demand dream workflow shared by Codex, Claude Code and DeepSeek Harness.

Read README.md for architecture and commands. Run `python3 -m unittest -v test_memory.py` for the storage tests.

## Current handoff

- Revision 1 contains 17 consolidated memories; the general profile is about 544 words.
- The initial pass parsed 760 local logs (653 unique provider/session identities); semantic review focused on correction/workflow passages. Do not claim exhaustive semantic reading of every session. Coverage is in sources/initial-review-coverage.json.
- The user explicitly paused activation and requested this project home. Global instruction adapters and skill links are NOT installed. No recurring schedule exists.
- Continue discussing/reviewing the design with the user. Do not activate global loaders or install skill links until the user authorizes activation.
- Prepared global configuration changes are in review/ and adapter-install-plan.json. Recheck destination file hashes before installation; preserve existing instructions.
- memory.sqlite3 is authoritative. Preserve revisions and original provider transcripts; use proposal/diff/promotion for memory changes. Do not rewrite old evidence or grant permissions based on a memory.
- Prior session scratch extraction remains in the originating Codex task; source logs remain at their original provider locations. This project contains the implementation, persistent store, evidence catalog, review materials and handoff needed to continue.
