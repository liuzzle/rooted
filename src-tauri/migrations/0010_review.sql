-- Rooted — decisions a person makes about the graph (Phase 5, second pass)
--
-- Two things the machine can only suggest: that two topics are one, and that a
-- verse belongs with a note. Neither is ever applied on its own. What these
-- tables record is the *decision*, made by a person, so it survives the worker
-- reading the notes again — which it does every time a note changes.

-- A spelling that now means another topic. Written when two topics are merged:
-- the merged topic's key points at the one it joined, so a note read again
-- later files that spelling under the surviving topic instead of bringing the
-- old one back. Deleting an alias undoes the merge.
CREATE TABLE IF NOT EXISTS concept_aliases (
  key        TEXT PRIMARY KEY,
  concept_id INTEGER NOT NULL REFERENCES concepts(concept_id) ON DELETE CASCADE,
  label      TEXT NOT NULL,                -- the merged topic's label, as it was
  merged_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_concept_aliases_concept ON concept_aliases(concept_id);

-- "These are not the same", so the pair is never suggested again.
CREATE TABLE IF NOT EXISTS concept_distinct (
  a          INTEGER NOT NULL REFERENCES concepts(concept_id) ON DELETE CASCADE,
  b          INTEGER NOT NULL REFERENCES concepts(concept_id) ON DELETE CASCADE,
  decided_at TEXT NOT NULL DEFAULT (datetime('now')),
  PRIMARY KEY (a, b),
  CHECK (a < b)
);

-- Which model embedded `concepts.embedding` (the label), as for chunks.
ALTER TABLE concepts ADD COLUMN embedded_by TEXT;

-- A verse suggested for a note, and what the person said about it. Keyed by
-- the canonical verse id, so the decision holds in every translation; the
-- suggestion itself is computed fresh and only the answer is kept.
CREATE TABLE IF NOT EXISTS verse_suggestions (
  note_id    INTEGER NOT NULL REFERENCES notes(note_id) ON DELETE CASCADE,
  verse_id   TEXT NOT NULL,
  status     TEXT NOT NULL CHECK (status IN ('accepted', 'dismissed')),
  score      REAL,                         -- similarity when it was decided
  model      TEXT,
  decided_at TEXT NOT NULL DEFAULT (datetime('now')),
  PRIMARY KEY (note_id, verse_id)
);
CREATE INDEX IF NOT EXISTS idx_verse_suggestions_verse ON verse_suggestions(verse_id);
