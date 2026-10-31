CREATE TABLE stores (
  id TEXT PRIMARY KEY,                                   -- memstore_<32 hex>
  kind TEXT NOT NULL CHECK (kind IN ('live','dream_output')),
  created_at TEXT NOT NULL, archived_at TEXT);
CREATE UNIQUE INDEX one_live_store ON stores(kind) WHERE kind = 'live';
CREATE TABLE memories (
  store_id TEXT NOT NULL REFERENCES stores(id), id TEXT NOT NULL,   -- mem_<32 hex>; same id in a dream's output
  path TEXT NOT NULL, content TEXT NOT NULL, content_sha256 TEXT NOT NULL,
  memory_version_id TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
  PRIMARY KEY (store_id, id), UNIQUE (store_id, path));
CREATE TABLE memory_versions (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE,   -- memver_<32 hex>
  store_id TEXT NOT NULL REFERENCES stores(id), memory_id TEXT NOT NULL,
  operation TEXT NOT NULL CHECK (operation IN ('created','modified','deleted')),
  path TEXT, content TEXT, content_sha256 TEXT,
  created_at TEXT NOT NULL, created_by TEXT NOT NULL, redacted_at TEXT, redacted_by TEXT);
CREATE INDEX versions_by_memory ON memory_versions(store_id, memory_id, seq);
CREATE TABLE dream_jobs (
  id TEXT PRIMARY KEY,                                   -- drm_<32 hex>
  status TEXT NOT NULL CHECK (status IN ('running','completed','canceled')),
  output_store_id TEXT NOT NULL REFERENCES stores(id),
  input TEXT NOT NULL,     -- frozen JSON: base {memory_id: {path, content_sha256}}, observations, sessions
  adoption TEXT,           -- JSON: summary, coverage, conflicts, added, changed, removed
  created_at TEXT NOT NULL, ended_at TEXT);
CREATE TRIGGER legacy_revisions_frozen BEFORE INSERT ON revisions BEGIN SELECT RAISE(ABORT,'store migrated to v2'); END;
CREATE TRIGGER legacy_dreams_frozen BEFORE INSERT ON dreams BEGIN SELECT RAISE(ABORT,'store migrated to v2'); END;
CREATE TRIGGER legacy_state_frozen BEFORE UPDATE ON state BEGIN SELECT RAISE(ABORT,'store migrated to v2'); END;
PRAGMA user_version = 2;
