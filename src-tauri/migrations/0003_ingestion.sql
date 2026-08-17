-- Rooted — ingestion pipeline (Phase 3)
--
-- A resumable job state machine. Every uploaded document gets exactly one job,
-- which walks:
--
--   UPLOADED → EXTRACTING → NEEDS_REVIEW → VERIFIED → DONE
--                    ↘ ERROR (retryable)
--
-- Phase 5 extends the tail with EXTRACTING_CONCEPTS → EMBEDDING → LINKED.
--
-- The rule that matters: text only becomes a note after a human has accepted
-- it (`extractions.verified`). Nothing machine-produced is ever stored as fact
-- on its own.

-- The uploaded source file, plus the metadata captured at upload time. That
-- metadata rides along into every citation the note later produces.
CREATE TABLE IF NOT EXISTS documents (
  doc_id      INTEGER PRIMARY KEY AUTOINCREMENT,
  filename    TEXT NOT NULL,            -- as the user named it
  stored_path TEXT NOT NULL,            -- copy kept under the app data dir
  format      TEXT NOT NULL,            -- txt | md | docx | pdf
  byte_size   INTEGER NOT NULL,
  sha256      TEXT NOT NULL,
  title       TEXT,
  doc_date    TEXT,                     -- when the talk/entry happened (ISO)
  speaker     TEXT,
  context     TEXT,
  created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS jobs (
  job_id      INTEGER PRIMARY KEY AUTOINCREMENT,
  doc_id      INTEGER NOT NULL REFERENCES documents(doc_id) ON DELETE CASCADE,
  state       TEXT NOT NULL DEFAULT 'UPLOADED',
  engine_used TEXT,
  confidence  REAL,
  attempts    INTEGER NOT NULL DEFAULT 0,
  last_error  TEXT,
  -- Worker lease. A crash leaves these set; the next worker reclaims the job
  -- once the lease goes stale, which is what makes the pipeline resumable.
  claimed_by  TEXT,
  claimed_at  TEXT,
  note_id     INTEGER REFERENCES notes(note_id) ON DELETE SET NULL,
  created_at  TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_jobs_state ON jobs(state);
CREATE UNIQUE INDEX IF NOT EXISTS idx_jobs_doc ON jobs(doc_id);

-- The text a stage produced. One row per job: re-running a stage replaces it,
-- so a stage is idempotent and a retry can't pile up duplicates.
CREATE TABLE IF NOT EXISTS extractions (
  extraction_id INTEGER PRIMARY KEY AUTOINCREMENT,
  job_id        INTEGER NOT NULL REFERENCES jobs(job_id) ON DELETE CASCADE,
  text          TEXT NOT NULL,
  engine        TEXT NOT NULL,
  confidence    REAL,
  -- 1 only once a human read it and accepted it, possibly after edits.
  verified      INTEGER NOT NULL DEFAULT 0,
  edited        INTEGER NOT NULL DEFAULT 0,
  created_at    TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at    TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_extractions_job ON extractions(job_id);
