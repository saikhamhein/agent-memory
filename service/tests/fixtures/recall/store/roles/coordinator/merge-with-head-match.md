---
{"title": "Merge only when the PR head matches the approved commit", "confidence": "explicit", "tags": ["coordination", "pull-request"], "evidence": [{"source": "fixture", "locator": "synthetic:merge-with-head-match.md", "note": "Synthetic evidence for recall tests"}], "priority": 6}
---
Read the diff, confirm the head sha is the one that was approved, and pass it to the merge command. Delete the branch afterwards. Applies to every agent that reads the shared memory.
