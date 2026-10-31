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
  "priority": 5,
  "tags": [
    "lora",
    "radio"
  ],
  "title": "Respect the LoRa duty cycle"
}
---
Transmitters stay within the regional duty-cycle limit. Retries back off exponentially and the mesh drops a packet rather than exceed the limit.
