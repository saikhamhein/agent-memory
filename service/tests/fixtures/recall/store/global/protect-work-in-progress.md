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
    "git",
    "safety"
  ],
  "title": "Protect other people's uncommitted work"
}
---
Inspect the working tree before staging. Separate the user's temporary edits and other agents' changes from the task, stage only intended files, and make small commits that each make sense alone. Read the actual diff before any merge the user authorised.

How to apply: when the user corrects this, the agent updates the memory the same day.
