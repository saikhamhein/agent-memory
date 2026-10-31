---
{
  "confidence": "explicit",
  "evidence": [
    {
      "locator": "fixture",
      "note": "synthetic recall fixture",
      "source": "workstream-r"
    }
  ],
  "priority": 2,
  "tags": [
    "raspberry-pi",
    "usb",
    "power"
  ],
  "title": "Raspberry Pi 5 USB ports share one power switch"
}
---
On a Raspberry Pi 5 the USB ports cannot be power-cycled one at a time; switching power affects all of them together. Plan device resets around a powered hub with per-port switching instead.

How to apply: when the user corrects this, the agent updates the memory the same day.
