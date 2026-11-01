---
{"title": "Keep the database in DELETE journal mode", "confidence": "explicit", "tags": ["sqlite", "backup"], "evidence": [{"source": "fixture", "locator": "synthetic:sqlite-journal-delete-mode.md", "note": "Synthetic evidence for recall tests"}], "priority": 6}
---
The store moved off WAL so a plain file copy is a consistent backup. Do not switch the journal mode back.
