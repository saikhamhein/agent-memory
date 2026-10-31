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
  "tags": [
    "serial",
    "console"
  ],
  "title": "Serial console runs at 115200 baud"
}
---
Boards in this lab use 115200 baud, 8 data bits, no parity, one stop bit. A garbled console almost always means a baud mismatch.

How to apply: an agent that finds this memory outdated records an observation for the next dream.
