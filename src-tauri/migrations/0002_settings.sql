-- Rooted — small key/value store for app state that outlives a session
-- (Phase 2: which translation the reader was last using).

CREATE TABLE IF NOT EXISTS settings (
  key        TEXT PRIMARY KEY,
  value      TEXT NOT NULL,
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
