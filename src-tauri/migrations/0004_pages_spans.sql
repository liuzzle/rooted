-- Rooted — pages and positioned spans (Phase 4)
--
-- A scanned page is not a paragraph. Handwritten notes carry meaning in their
-- layout: arrows, sidenotes in the margin, bullets nested by indentation,
-- fragments that aren't sentences. Flattening that into prose would silently
-- invent reading order and connections that were never written.
--
-- So a scan is stored as **the page image plus text spans with their positions
-- on it**. Every span keeps its box and its confidence; the review UI draws
-- them over the scan; nothing is reordered or joined behind the user's back.
--
-- Audio uses the same table with time instead of space: `start_s`/`end_s` and
-- a speaker label rather than a box.

-- One row per page of a document. Single images have exactly one.
CREATE TABLE IF NOT EXISTS pages (
  page_id    INTEGER PRIMARY KEY AUTOINCREMENT,
  doc_id     INTEGER NOT NULL REFERENCES documents(doc_id) ON DELETE CASCADE,
  page_no    INTEGER NOT NULL,          -- 1-based
  image_path TEXT NOT NULL,             -- the page as shown in review
  width      INTEGER NOT NULL,
  height     INTEGER NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_pages_doc_no ON pages(doc_id, page_no);

-- A recognised piece of text, anchored where it was found.
--
-- Spans belong to the *document*, not the job: the job is pipeline bookkeeping
-- and can be deleted, but the page and what was read off it belong to the note.
CREATE TABLE IF NOT EXISTS spans (
  span_id    INTEGER PRIMARY KEY AUTOINCREMENT,
  doc_id     INTEGER NOT NULL REFERENCES documents(doc_id) ON DELETE CASCADE,
  page_id    INTEGER REFERENCES pages(page_id) ON DELETE CASCADE,  -- NULL for audio
  idx        INTEGER NOT NULL,          -- reading order as the engine found it
  text       TEXT NOT NULL,
  confidence REAL,
  -- Position on the page, normalised 0..1 from the top-left. NULL for audio.
  x          REAL,
  y          REAL,
  w          REAL,
  h          REAL,
  -- Position in time, seconds. NULL for images.
  start_s    REAL,
  end_s      REAL,
  speaker    TEXT,
  -- 1 once a human changed this span's text.
  edited     INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_spans_doc_idx ON spans(doc_id, idx);
CREATE INDEX IF NOT EXISTS idx_spans_page ON spans(page_id);

-- A note made from a document keeps a way back to the page it was read off, so
-- the reader can always show the original beside the text.
ALTER TABLE notes ADD COLUMN doc_id INTEGER REFERENCES documents(doc_id) ON DELETE SET NULL;
