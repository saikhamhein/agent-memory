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
  "priority": 4,
  "tags": [
    "release"
  ],
  "title": "Release checklist for the notes app"
}
---
Bump the version in one place, run the full test suite and the offline sync scenario, build the signed bundle, then tag. The changelog entry is written before the tag, never after.
