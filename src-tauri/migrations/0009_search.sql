-- Rooted — search (Phase 6)
--
-- Search returns passages, never answers: every hit is a chunk of a note or a
-- verse, quoted whole, with the place it came from. So the indexes here are
-- over the two things that can be quoted — `chunks` and `verses` — and nothing
-- else. There is no table of summaries to search because there are none.
--
-- Two kinds of index, because they fail differently:
--
-- * **Words** (FTS5). Always there, needs nothing installed, and exact — it
--   finds what was written. External-content tables, so the text is stored
--   once, in the table it already lives in; the triggers keep them in step no
--   matter which process writes (the worker writes chunks, the app writes
--   verses).
-- * **Meaning** (vectors). Present only once a local embedding model has read
--   the text. Each vector records the model that made it, because vectors from
--   two models are not comparable and a query must be embedded by the same one.
--
-- `remove_diacritics 2` lets "Romer" find "Römer". It does not stem: "Gnade"
-- does not find "Gnaden". The query side asks for prefixes instead, which
-- covers German declension well enough without a second normaliser in Rust.

CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
  text,
  content = 'chunks',
  content_rowid = 'chunk_id',
  tokenize = 'unicode61 remove_diacritics 2'
);
INSERT INTO chunks_fts(chunks_fts) VALUES ('rebuild');

CREATE TRIGGER IF NOT EXISTS chunks_fts_insert AFTER INSERT ON chunks BEGIN
  INSERT INTO chunks_fts(rowid, text) VALUES (new.chunk_id, new.text);
END;
CREATE TRIGGER IF NOT EXISTS chunks_fts_delete AFTER DELETE ON chunks BEGIN
  INSERT INTO chunks_fts(chunks_fts, rowid, text) VALUES ('delete', old.chunk_id, old.text);
END;
CREATE TRIGGER IF NOT EXISTS chunks_fts_update AFTER UPDATE OF text ON chunks BEGIN
  INSERT INTO chunks_fts(chunks_fts, rowid, text) VALUES ('delete', old.chunk_id, old.text);
  INSERT INTO chunks_fts(rowid, text) VALUES (new.chunk_id, new.text);
END;

CREATE VIRTUAL TABLE IF NOT EXISTS verses_fts USING fts5(
  text,
  content = 'verses',
  content_rowid = 'rowid',
  tokenize = 'unicode61 remove_diacritics 2'
);
INSERT INTO verses_fts(verses_fts) VALUES ('rebuild');

CREATE TRIGGER IF NOT EXISTS verses_fts_insert AFTER INSERT ON verses BEGIN
  INSERT INTO verses_fts(rowid, text) VALUES (new.rowid, new.text);
END;
CREATE TRIGGER IF NOT EXISTS verses_fts_delete AFTER DELETE ON verses BEGIN
  INSERT INTO verses_fts(verses_fts, rowid, text) VALUES ('delete', old.rowid, old.text);
END;
CREATE TRIGGER IF NOT EXISTS verses_fts_update AFTER UPDATE OF text ON verses BEGIN
  INSERT INTO verses_fts(verses_fts, rowid, text) VALUES ('delete', old.rowid, old.text);
  INSERT INTO verses_fts(rowid, text) VALUES (new.rowid, new.text);
END;

-- Which model wrote `chunks.embedding`. A chunk is replaced when its note
-- changes, so its vector goes with it and is simply made again.
ALTER TABLE chunks ADD COLUMN embedded_by TEXT;

-- A verse's vector, per translation: the same verse reads differently in
-- Luther and in the WEB, and search runs in the translation being read. Tied to
-- the verse row itself, so removing a pack takes its vectors with it.
CREATE TABLE IF NOT EXISTS verse_vectors (
  translation_id INTEGER NOT NULL,
  verse_id       TEXT NOT NULL,
  model          TEXT NOT NULL,
  vector         BLOB NOT NULL,             -- little-endian f32, unit length
  PRIMARY KEY (translation_id, verse_id),
  FOREIGN KEY (translation_id, verse_id)
    REFERENCES verses(translation_id, verse_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_verse_vectors_model
  ON verse_vectors(translation_id, model);
