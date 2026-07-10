-- Rooted — canonical schema
-- Phase 0: reading (books, translations, verses, tokens)
-- Phase 1: notes & highlights (notes, note_anchors, highlights)
-- Later phases add: jobs, sources, chunks, concepts, concept_mentions (AI/KG).

PRAGMA foreign_keys = ON;

-- ---------------------------------------------------------------------------
-- Bible text
-- ---------------------------------------------------------------------------

-- Canonical, translation-independent book reference (OSIS codes).
CREATE TABLE IF NOT EXISTS books (
  osis            TEXT PRIMARY KEY,          -- e.g. 'Gen'
  canonical_order INTEGER NOT NULL UNIQUE,   -- 1..66
  name            TEXT NOT NULL,             -- display name, e.g. 'Genesis'
  testament       TEXT NOT NULL              -- 'OT' | 'NT'
);

CREATE TABLE IF NOT EXISTS translations (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  abbrev        TEXT NOT NULL UNIQUE,        -- 'WEB', 'KJV'
  name          TEXT NOT NULL,              -- 'World English Bible'
  language      TEXT NOT NULL DEFAULT 'en',
  license       TEXT,
  source_type   TEXT NOT NULL DEFAULT 'bundled',  -- bundled | api | imported
  versification TEXT NOT NULL DEFAULT 'kjv',
  installed_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS verses (
  verse_id        TEXT NOT NULL,             -- OSIS BCV key, e.g. 'Gen.1.1'
  translation_id  INTEGER NOT NULL REFERENCES translations(id) ON DELETE CASCADE,
  book_osis       TEXT NOT NULL REFERENCES books(osis),
  chapter         INTEGER NOT NULL,
  verse           INTEGER NOT NULL,
  text            TEXT NOT NULL,
  canonical_order INTEGER NOT NULL,          -- global ordering for iteration
  PRIMARY KEY (translation_id, verse_id)
);
CREATE INDEX IF NOT EXISTS idx_verses_bcv
  ON verses(translation_id, book_osis, chapter, verse);

-- Word-level tokens with char offsets into the verse text (for word anchoring).
CREATE TABLE IF NOT EXISTS tokens (
  token_id        INTEGER PRIMARY KEY AUTOINCREMENT,
  translation_id  INTEGER NOT NULL REFERENCES translations(id) ON DELETE CASCADE,
  verse_id        TEXT NOT NULL,
  idx             INTEGER NOT NULL,          -- 0-based word index within verse
  surface         TEXT NOT NULL,
  char_start      INTEGER NOT NULL,
  char_end        INTEGER NOT NULL,
  strongs         TEXT,
  lemma           TEXT
);
CREATE INDEX IF NOT EXISTS idx_tokens_verse
  ON tokens(translation_id, verse_id, idx);

-- ---------------------------------------------------------------------------
-- Notes & highlights (Phase 1)
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS notes (
  note_id     INTEGER PRIMARY KEY AUTOINCREMENT,
  title       TEXT,
  body        TEXT NOT NULL DEFAULT '',
  date        TEXT,     -- entry/talk date (ISO)
  speaker     TEXT,
  context     TEXT,
  created_at  TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

-- A note may anchor to a verse (translation-independent) or a specific word
-- (translation-scoped). Word anchors snapshot the surface for graceful
-- cross-translation degradation.
CREATE TABLE IF NOT EXISTS note_anchors (
  id             INTEGER PRIMARY KEY AUTOINCREMENT,
  note_id        INTEGER NOT NULL REFERENCES notes(note_id) ON DELETE CASCADE,
  anchor_type    TEXT NOT NULL,             -- 'verse' | 'word'
  verse_id       TEXT NOT NULL,             -- BCV key
  translation_id INTEGER,                   -- NULL for verse anchors
  token_idx      INTEGER,                   -- NULL for verse anchors
  surface        TEXT                       -- word snapshot (word anchors)
);
CREATE INDEX IF NOT EXISTS idx_note_anchors_verse ON note_anchors(verse_id);
CREATE INDEX IF NOT EXISTS idx_note_anchors_note ON note_anchors(note_id);

CREATE TABLE IF NOT EXISTS highlights (
  id             INTEGER PRIMARY KEY AUTOINCREMENT,
  anchor_type    TEXT NOT NULL,             -- 'verse' | 'word'
  verse_id       TEXT NOT NULL,
  translation_id INTEGER,                   -- NULL for verse-level highlights
  token_idx      INTEGER,
  surface        TEXT,
  color          TEXT NOT NULL DEFAULT 'yellow',
  created_at     TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_highlights_verse ON highlights(verse_id);
