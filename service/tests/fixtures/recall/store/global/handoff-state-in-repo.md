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
  "priority": 3,
  "tags": [
    "handoff",
    "planning"
  ],
  "title": "Keep handoff state in the repository"
}
---
Work that spans sessions keeps a short status file in the repository: commits, owners, blockers, validation and next steps. Update it at each integration checkpoint so a fresh agent can resume from the repository instead of a long chat.

How to apply: when the user corrects this, the agent updates the memory the same day.
