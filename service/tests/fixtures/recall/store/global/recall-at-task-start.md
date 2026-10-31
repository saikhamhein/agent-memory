---
{"title": "Recall shared memory once at the start of a task", "confidence": "explicit", "tags": ["recall", "context"], "evidence": [{"source": "fixture", "locator": "synthetic:recall-at-task-start.md", "note": "Synthetic evidence for recall tests"}], "priority": 5}
---
Load the start view once per session. Later lookups use a query so that only matching entries come back, which keeps the context short. Applies to every agent that reads the shared memory.
