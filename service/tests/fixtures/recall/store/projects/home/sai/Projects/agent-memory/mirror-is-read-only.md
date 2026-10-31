---
{"title": "The mirror directory is read-only; write through the service", "confidence": "explicit", "tags": ["mirror", "files"], "evidence": [{"source": "fixture", "locator": "synthetic:mirror-is-read-only.md", "note": "Synthetic evidence for recall tests"}], "priority": 3}
---
Files under the mirror have mode 0444. Editing them changes nothing, because the next generation replaces them. Permissions are part of the contract.
