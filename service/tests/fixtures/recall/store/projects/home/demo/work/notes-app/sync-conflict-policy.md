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
  "pinned": true,
  "priority": 9,
  "tags": [
    "sync",
    "conflicts"
  ],
  "title": "Sync conflicts keep both copies"
}
---
When two devices edit the same note offline, the notes app keeps both versions and marks the newer one as current. Never merge note bodies automatically, because an automatic merge silently loses text the user typed.
