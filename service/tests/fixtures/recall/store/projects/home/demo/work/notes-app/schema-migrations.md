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
    "database",
    "migration"
  ],
  "title": "Schema migrations run forward only"
}
---
Each SQLite schema change is a numbered forward migration with a test that upgrades a copy of the previous release's database. There are no down migrations; a bad migration is fixed by a new one.
