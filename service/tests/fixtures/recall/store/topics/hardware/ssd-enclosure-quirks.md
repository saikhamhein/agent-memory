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
    "storage",
    "usb"
  ],
  "title": "Some SSD enclosures need UAS disabled"
}
---
A few USB SSD enclosures drop off the bus under load with UAS enabled. Add a usb-storage quirk for the enclosure's vendor and product id and confirm the disk stays mounted through a long copy.

How to apply: the agent follows this in every session; this memory outranks habit.
