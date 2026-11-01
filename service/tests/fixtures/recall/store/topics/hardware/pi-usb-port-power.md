---
{"title": "Check whether a Raspberry Pi 5 can switch one USB port before planning on it", "confidence": "explicit", "tags": ["hardware", "raspberry", "usb"], "evidence": [{"source": "fixture", "locator": "synthetic:pi-usb-port-power.md", "note": "Synthetic evidence for recall tests"}], "priority": 5}
---
Per-port power switching depends on the hub chip and the board revision. Read the vendor notes and test on the bench with a meter before writing it into a plan.
