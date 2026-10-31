---
{"title": "Make small, coherent commits", "confidence": "explicit", "tags": ["git", "commits", "review"], "evidence": [{"source": "fixture", "locator": "synthetic:small-commits.md", "note": "Synthetic evidence for recall tests"}], "priority": 6}
---
Stage only the files that belong to the task. One logical change per commit keeps review and revert cheap. A small diff is easier to check than a large one. Applies to every agent that reads the shared memory.
