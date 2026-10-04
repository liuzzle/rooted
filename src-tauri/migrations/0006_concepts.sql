-- Rooted — the provenance ledger (Phase 5)
--
-- A topic exists *because* something in a real note says it, at an exact place.
-- That is the whole design: `concept_mentions` points at a character range in a
-- `chunk`, which is a character range in a `source`, which is a note (later, a
-- verse). Follow any concept far enough and you land on words someone actually
-- wrote. Nothing can be displayed that doesn't end at a resolvable citation.
--
-- Which means the tables deliberately cannot express a concept that came from
-- nowhere: there is no text column on `concepts` holding a generated summary,
-- and no way to attach a mention to anything but a real range of a real chunk.
-- A label is stored, but every use of it is backed by its mentions.

-- What a citation can point at. Notes now; verses when search arrives, which is
-- why `kind` exists rather than this being a notes-only table.
CREATE TABLE IF NOT EXISTS sources (
  source_id  INTEGER PRIMARY KEY AUTOINCREMENT,
  kind       TEXT NOT NULL,                -- 'note' | 'verse'
  note_id    INTEGER REFERENCES notes(note_id) ON DELETE CASCADE,
  verse_id   TEXT,                         -- BCV key, for kind = 'verse'
  -- What was actually indexed. A note edited afterwards no longer matches, and
  -- is re-read: stale mentions would cite offsets into text that changed.
  text_hash  TEXT NOT NULL,
  indexed_at TEXT,
  indexed_by TEXT,                         -- which extractors ran
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_sources_note ON sources(note_id)
  WHERE note_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_sources_verse ON sources(verse_id)
  WHERE verse_id IS NOT NULL;

-- A retrievable piece of a source, with its offsets *into the source text*.
-- Notes are chunked by paragraph: small enough to quote as a citation, large
-- enough to still mean something on its own.
CREATE TABLE IF NOT EXISTS chunks (
  chunk_id   INTEGER PRIMARY KEY AUTOINCREMENT,
  source_id  INTEGER NOT NULL REFERENCES sources(source_id) ON DELETE CASCADE,
  idx        INTEGER NOT NULL,
  char_start INTEGER NOT NULL,
  char_end   INTEGER NOT NULL,
  text       TEXT NOT NULL,
  embedding  BLOB,                         -- Phase 5 second pass / Phase 6
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_chunks_source_idx ON chunks(source_id, idx);

-- A topic. `label` is a surface form that was really written somewhere; `key`
-- is only for lookup, so two spellings don't make two rows on day one. Real
-- alias handling and embedding-based merging come with the dedup pass.
CREATE TABLE IF NOT EXISTS concepts (
  concept_id INTEGER PRIMARY KEY AUTOINCREMENT,
  label      TEXT NOT NULL,
  key        TEXT NOT NULL UNIQUE,         -- casefolded, whitespace-collapsed
  embedding  BLOB,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- The link, and the evidence for it. `surface` is the exact substring found at
-- (char_start, char_end) of the chunk — stored so the gate can be re-checked
-- later without re-running anything, and so a citation can be rendered without
-- trusting the offsets alone.
CREATE TABLE IF NOT EXISTS concept_mentions (
  mention_id   INTEGER PRIMARY KEY AUTOINCREMENT,
  concept_id   INTEGER NOT NULL REFERENCES concepts(concept_id) ON DELETE CASCADE,
  chunk_id     INTEGER NOT NULL REFERENCES chunks(chunk_id) ON DELETE CASCADE,
  char_start   INTEGER NOT NULL,           -- offsets within the chunk
  char_end     INTEGER NOT NULL,
  surface      TEXT NOT NULL,
  extracted_by TEXT NOT NULL,              -- 'phrases' | 'ollama/<model>' | ...
  confidence   REAL,
  -- A person confirmed this mention. Nothing requires it yet; the column exists
  -- because merges and verse suggestions will, and they need somewhere to say so.
  verified     INTEGER NOT NULL DEFAULT 0,
  created_at   TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_mentions_span
  ON concept_mentions(chunk_id, char_start, char_end, concept_id);
CREATE INDEX IF NOT EXISTS idx_mentions_concept ON concept_mentions(concept_id);
