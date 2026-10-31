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
    "merge",
    "approval"
  ],
  "title": "Only the coordinator merges approved pull requests"
}
---
Teammates send the coordinator the approved pull request number and head commit. The coordinator checks the head is current and mergeable, merges it, and confirms the linked issue closed.

How to apply: the agent checks this memory before acting and tells the user when it does not fit.
