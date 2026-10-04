-- Rooted — scripture references found inside notes (Phase 5)
--
-- A note that says "Röm 4,3" is pointing at something the app already has. The
-- link is stored rather than detected in the UI so it obeys the same rules as
-- everything else here: it has offsets into a real chunk, it is found by the
-- worker once, and it is thrown away and re-found when the note changes.
--
-- What is *not* stored is whether the verse exists. A reference is a thing the
-- note says; whether Römer 4 has a verse 3 in the translation you happen to
-- have installed is a different question, answered when the link is rendered.
-- Storing an answer to it would go stale the moment a pack is added or removed.

CREATE TABLE IF NOT EXISTS verse_links (
  link_id    INTEGER PRIMARY KEY AUTOINCREMENT,
  chunk_id   INTEGER NOT NULL REFERENCES chunks(chunk_id) ON DELETE CASCADE,
  char_start INTEGER NOT NULL,          -- offsets within the chunk
  char_end   INTEGER NOT NULL,
  surface    TEXT NOT NULL,             -- "Röm 4,3", exactly as written
  book_osis  TEXT NOT NULL,
  chapter    INTEGER NOT NULL,
  verse      INTEGER,                   -- NULL when the note cites a chapter
  verse_end  INTEGER,                   -- set only for a range
  verse_id   TEXT,                      -- 'Rom.4.3', NULL for a chapter
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_verse_links_span
  ON verse_links(chunk_id, char_start, char_end);
CREATE INDEX IF NOT EXISTS idx_verse_links_verse ON verse_links(verse_id);
