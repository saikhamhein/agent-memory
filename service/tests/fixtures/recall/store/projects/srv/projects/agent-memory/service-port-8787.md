---
{"title": "The local service listens on port 8787 only on loopback", "confidence": "explicit", "tags": ["service", "network"], "evidence": [{"source": "fixture", "locator": "synthetic:service-port-8787.md", "note": "Synthetic evidence for recall tests"}], "priority": 4}
---
Clients reach the service on 127.0.0.1. Never bind it to another interface, and keep tokens in the config file.
