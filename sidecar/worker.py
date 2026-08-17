#!/usr/bin/env python3
"""
Rooted ingestion worker.

Owns the *machine* stages of the pipeline. A person owns the rest: the app
writes UPLOADED jobs and moves NEEDS_REVIEW → VERIFIED; this worker moves
UPLOADED → EXTRACTING → NEEDS_REVIEW, and VERIFIED → DONE.

    UPLOADED → EXTRACTING → NEEDS_REVIEW → VERIFIED → DONE
                     ↘ ERROR (retryable)

Three properties are deliberate:

* **Resumable.** A job is claimed with a lease (`claimed_by`/`claimed_at`).
  If this process dies mid-stage, the next worker reclaims the job once the
  lease goes stale and starts that stage again from its inputs.
* **Idempotent.** Each stage recomputes from the document on disk and upserts a
  single extraction row, so re-running a stage can't duplicate or half-apply.
* **Never authors content.** The publish stage refuses any job whose extraction
  is not marked verified by a human. Extraction copies bytes out of a file; it
  never rewrites, summarises, or fills in.

Usage:
    python3 sidecar/worker.py                 # follow the queue
    python3 sidecar/worker.py --once          # drain what's ready, then exit
    python3 sidecar/worker.py --db ./rooted.db
"""
from __future__ import annotations

import argparse
import os
import sqlite3
import sys
import time
import uuid
from pathlib import Path
from typing import Optional

from engines import (
    EngineUnavailable,
    Extraction,
    ExtractionError,
    confidence_of_decode,
    extract_docx,
    extract_pdf_text_layer,
    extract_plain_text,
    ocr_image,
    ocr_pdf,
    transcribe_audio,
)

REPO_ROOT = Path(__file__).resolve().parent.parent

# Job states (mirrored in src-tauri/src/ingest.rs).
UPLOADED = "UPLOADED"
EXTRACTING = "EXTRACTING"
NEEDS_REVIEW = "NEEDS_REVIEW"
VERIFIED = "VERIFIED"
DONE = "DONE"
ERROR = "ERROR"

# A job that has failed this many times stops being retried automatically.
MAX_ATTEMPTS = 3
# Seconds before another worker may steal a claimed job.
DEFAULT_LEASE = 120
# Poll interval when following the queue.
DEFAULT_INTERVAL = 2.0

# ---------------------------------------------------------------------------
# Database
# ---------------------------------------------------------------------------

def default_db_path() -> Path:
    if os.environ.get("ROOTED_DB"):
        return Path(os.environ["ROOTED_DB"])
    if sys.platform == "darwin":
        base = Path.home() / "Library" / "Application Support"
    elif sys.platform.startswith("win"):
        base = Path(os.environ.get("APPDATA", Path.home()))
    else:
        base = Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local" / "share"))
    return base / "com.rooted.app" / "rooted.db"


def connect(db_path: Path) -> sqlite3.Connection:
    """Open the shared database. WAL + a busy timeout let the app read while
    this process writes."""
    conn = sqlite3.connect(db_path, isolation_level=None, timeout=10)
    conn.row_factory = sqlite3.Row
    conn.execute("PRAGMA foreign_keys = ON")
    conn.execute("PRAGMA journal_mode = WAL")
    conn.execute("PRAGMA busy_timeout = 5000")
    return conn


def apply_migrations(conn: sqlite3.Connection) -> None:
    """Apply the app's migrations so the worker can run against a fresh
    database on its own.

    Uses the same `schema_migrations` ledger the Rust side keeps — not every
    migration is idempotent (`ALTER TABLE ADD COLUMN` isn't), so whichever
    process gets there first records it and the other skips it.
    """
    conn.execute(
        """CREATE TABLE IF NOT EXISTS schema_migrations (
             name       TEXT PRIMARY KEY,
             applied_at TEXT NOT NULL DEFAULT (datetime('now'))
           )"""
    )
    applied = {
        row["name"] for row in conn.execute("SELECT name FROM schema_migrations")
    }
    folder = REPO_ROOT / "src-tauri" / "migrations"
    for path in sorted(folder.glob("*.sql")):
        name = path.stem
        if name in applied:
            continue
        conn.executescript(path.read_text(encoding="utf-8"))
        conn.execute("INSERT INTO schema_migrations (name) VALUES (?)", (name,))


# ---------------------------------------------------------------------------
# Extraction — typed documents only (Phase 3)
# ---------------------------------------------------------------------------

IMAGE_FORMATS = {"jpg", "jpeg", "png", "heic", "tiff", "tif"}
AUDIO_FORMATS = {"mp3", "m4a", "wav", "aiff", "flac"}


def kind_of(fmt: str) -> str:
    """Which engine family reads this format (mirrors src-tauri/src/ingest.rs)."""
    if fmt in IMAGE_FORMATS:
        return "image"
    if fmt in AUDIO_FORMATS:
        return "audio"
    return "typed"


def read_document(path: Path, fmt: str, pages_dir: Path) -> Extraction:
    """Dispatch a document to the engine that can read it.

    A PDF is tried as a typed document first and falls back to OCR: a scan
    saved as PDF is the common case, and the two are indistinguishable by
    extension.
    """
    if fmt in ("txt", "md"):
        return extract_plain_text(path)
    if fmt == "docx":
        return extract_docx(path)
    if fmt == "pdf":
        try:
            return extract_pdf_text_layer(path)
        except EngineUnavailable:
            raise
        except ExtractionError:
            # No text layer: it's a scan. OCR it, keeping the rendered pages.
            return ocr_pdf(path, pages_dir)
    if kind_of(fmt) == "image":
        return ocr_image(path)
    if kind_of(fmt) == "audio":
        return transcribe_audio(path)
    raise ExtractionError(f"no engine reads '{fmt}' files")


# ---------------------------------------------------------------------------
# Worker
# ---------------------------------------------------------------------------

class Worker:
    def __init__(
        self,
        conn: sqlite3.Connection,
        lease: int = DEFAULT_LEASE,
        auto_verify: bool = False,
        worker_id: Optional[str] = None,
    ) -> None:
        self.conn = conn
        self.lease = lease
        # Typed documents can skip review when extraction is certain. Off by
        # default: the human-in-the-loop state is the point of the pipeline.
        self.auto_verify = auto_verify
        self.id = worker_id or f"{os.getpid()}-{uuid.uuid4().hex[:6]}"

    # -- state helpers ------------------------------------------------------

    def set_state(self, job_id: int, state: str, **fields) -> None:
        assignments = ["state = ?", "updated_at = datetime('now')"]
        params: list = [state]
        for key, value in fields.items():
            assignments.append(f"{key} = ?")
            params.append(value)
        params.append(job_id)
        self.conn.execute(
            f"UPDATE jobs SET {', '.join(assignments)} WHERE job_id = ?", params
        )

    def heartbeat(self) -> None:
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES ('worker_heartbeat', datetime('now'))"
            " ON CONFLICT(key) DO UPDATE SET value = excluded.value,"
            " updated_at = datetime('now')"
        )

    def reclaim_stale(self) -> int:
        """Return jobs whose worker died to the queue. This is what makes the
        pipeline survive a kill mid-stage."""
        cur = self.conn.execute(
            """UPDATE jobs
                  SET state = CASE WHEN attempts >= ? THEN ? ELSE ? END,
                      last_error = CASE WHEN attempts >= ? THEN
                          'gave up after ' || attempts || ' interrupted attempts' END,
                      claimed_by = NULL, claimed_at = NULL,
                      updated_at = datetime('now')
                WHERE state = ?
                  AND claimed_at IS NOT NULL
                  AND claimed_at <= datetime('now', ?)""",
            (MAX_ATTEMPTS, ERROR, UPLOADED, MAX_ATTEMPTS, EXTRACTING, f"-{self.lease} seconds"),
        )
        return cur.rowcount or 0

    def claim(self) -> Optional[sqlite3.Row]:
        """Atomically take the oldest queued job. `BEGIN IMMEDIATE` means two
        workers can never claim the same one."""
        self.conn.execute("BEGIN IMMEDIATE")
        try:
            row = self.conn.execute(
                "SELECT job_id FROM jobs WHERE state = ? AND claimed_at IS NULL"
                " ORDER BY created_at, job_id LIMIT 1",
                (UPLOADED,),
            ).fetchone()
            if row is None:
                self.conn.execute("COMMIT")
                return None
            job_id = row["job_id"]
            self.conn.execute(
                """UPDATE jobs
                      SET state = ?, claimed_by = ?, claimed_at = datetime('now'),
                          attempts = attempts + 1, updated_at = datetime('now')
                    WHERE job_id = ?""",
                (EXTRACTING, self.id, job_id),
            )
            self.conn.execute("COMMIT")
        except Exception:
            self.conn.execute("ROLLBACK")
            raise
        return self.job(job_id)

    def job(self, job_id: int) -> sqlite3.Row:
        return self.conn.execute(
            """SELECT j.*, d.filename, d.stored_path, d.format, d.title, d.doc_date,
                      d.speaker, d.context
                 FROM jobs j JOIN documents d ON d.doc_id = j.doc_id
                WHERE j.job_id = ?""",
            (job_id,),
        ).fetchone()

    # -- stages -------------------------------------------------------------

    def extract(self, job: sqlite3.Row) -> None:
        """UPLOADED → NEEDS_REVIEW: get the text out of the file, verbatim."""
        job_id = job["job_id"]
        path = Path(job["stored_path"])
        try:
            if not path.exists():
                raise ExtractionError(f"the uploaded file is missing: {path}")
            extraction = read_document(path, job["format"], self.pages_dir(job))
            extraction.confidence = min(
                extraction.confidence, confidence_of_decode(extraction.text)
            )
        except ExtractionError as exc:
            self.set_state(job_id, ERROR, last_error=str(exc),
                           claimed_by=None, claimed_at=None)
            return
        except Exception as exc:  # unexpected: keep the message, allow a retry
            self.set_state(job_id, ERROR, last_error=f"{type(exc).__name__}: {exc}",
                           claimed_by=None, claimed_at=None)
            return

        self.store_extraction(job, extraction)

        # A scan or a recording is never accepted on the machine's say-so,
        # however confident it claims to be: on-device OCR reports 1.0 for
        # readings that are plainly wrong, and a transcript is a reading of
        # sound, not a copy of text. Only typed documents can auto-pass.
        auto = (
            self.auto_verify
            and extraction.confidence >= 1.0
            and kind_of(job["format"]) == "typed"
        )
        if auto:
            self.conn.execute(
                "UPDATE extractions SET verified = 1, updated_at = datetime('now')"
                " WHERE job_id = ?",
                (job_id,),
            )
        self.set_state(
            job_id,
            VERIFIED if auto else NEEDS_REVIEW,
            engine_used=extraction.engine,
            confidence=extraction.confidence,
            last_error=None,
            claimed_by=None,
            claimed_at=None,
        )

    def pages_dir(self, job: sqlite3.Row) -> Path:
        """Where rendered page images live, beside the uploaded documents."""
        return Path(job["stored_path"]).parent / "pages"

    def store_extraction(self, job: sqlite3.Row, extraction: Extraction) -> None:
        """Write the text, its pages and its spans as one transaction.

        Re-running a stage replaces all three, so an extraction can never be
        half-old: the spans always describe the text that is stored with them.
        """
        job_id, doc_id = job["job_id"], job["doc_id"]
        self.conn.execute("BEGIN IMMEDIATE")
        try:
            self.conn.execute(
                """INSERT INTO extractions (job_id, text, engine, confidence, verified)
                   VALUES (?, ?, ?, ?, 0)
                   ON CONFLICT(job_id) DO UPDATE SET
                     text = excluded.text, engine = excluded.engine,
                     confidence = excluded.confidence,
                     verified = 0, edited = 0, updated_at = datetime('now')""",
                (job_id, extraction.text, extraction.engine, extraction.confidence),
            )
            self.conn.execute("DELETE FROM spans WHERE doc_id = ?", (doc_id,))
            self.conn.execute("DELETE FROM pages WHERE doc_id = ?", (doc_id,))

            page_ids: dict[int, int] = {}
            for page in extraction.pages:
                cur = self.conn.execute(
                    """INSERT INTO pages (doc_id, page_no, image_path, width, height)
                       VALUES (?, ?, ?, ?, ?)""",
                    (doc_id, page.page_no, page.image_path, page.width, page.height),
                )
                page_ids[page.page_no] = cur.lastrowid

            for span in extraction.spans:
                self.conn.execute(
                    """INSERT INTO spans
                         (doc_id, page_id, idx, text, confidence, x, y, w, h,
                          start_s, end_s, speaker)
                       VALUES (?,?,?,?,?,?,?,?,?,?,?,?)""",
                    (doc_id, page_ids.get(span.page_no or -1), span.idx, span.text,
                     span.confidence, span.x, span.y, span.w, span.h,
                     span.start_s, span.end_s, span.speaker),
                )
            self.conn.execute("COMMIT")
        except Exception:
            self.conn.execute("ROLLBACK")
            raise

    def publish(self, job: sqlite3.Row) -> None:
        """VERIFIED → DONE: turn human-accepted text into a note.

        Refuses anything not verified. That check is the last line of the
        no-hallucination guarantee: machine text never becomes a note by itself.
        """
        job_id = job["job_id"]
        row = self.conn.execute(
            "SELECT text, verified FROM extractions WHERE job_id = ?", (job_id,)
        ).fetchone()
        if row is None or not row["verified"]:
            self.set_state(
                job_id, ERROR,
                last_error="refusing to write a note from unverified text",
            )
            return

        title = job["title"] or Path(job["filename"]).stem
        body = row["text"]

        self.conn.execute("BEGIN IMMEDIATE")
        try:
            if job["note_id"]:
                # Idempotent: a re-run updates the note it already made.
                self.conn.execute(
                    """UPDATE notes SET title = ?, body = ?, date = ?, speaker = ?,
                              context = ?, updated_at = datetime('now')
                        WHERE note_id = ?""",
                    (title, body, job["doc_date"], job["speaker"], job["context"],
                     job["note_id"]),
                )
                note_id = job["note_id"]
            else:
                cur = self.conn.execute(
                    """INSERT INTO notes (title, body, date, speaker, context, doc_id)
                       VALUES (?, ?, ?, ?, ?, ?)""",
                    (title, body, job["doc_date"], job["speaker"], job["context"],
                     job["doc_id"]),
                )
                note_id = cur.lastrowid
            self.conn.execute(
                """UPDATE jobs SET state = ?, note_id = ?, last_error = NULL,
                          claimed_by = NULL, claimed_at = NULL,
                          updated_at = datetime('now')
                    WHERE job_id = ?""",
                (DONE, note_id, job_id),
            )
            self.conn.execute("COMMIT")
        except Exception:
            self.conn.execute("ROLLBACK")
            raise

    # -- loop ---------------------------------------------------------------

    def tick(self) -> int:
        """One pass: reclaim, extract what's queued, publish what's verified.
        Returns how many jobs were advanced."""
        advanced = 0
        self.reclaim_stale()

        while True:
            job = self.claim()
            if job is None:
                break
            self.extract(job)
            advanced += 1

        for row in self.conn.execute(
            "SELECT job_id FROM jobs WHERE state = ? ORDER BY updated_at", (VERIFIED,)
        ).fetchall():
            self.publish(self.job(row["job_id"]))
            advanced += 1

        self.heartbeat()
        return advanced

    def run(self, interval: float = DEFAULT_INTERVAL) -> None:
        while True:
            try:
                self.tick()
            except sqlite3.OperationalError as exc:
                # Usually the app holding a write lock; try again next tick.
                print(f"[worker] database busy: {exc}", file=sys.stderr, flush=True)
            time.sleep(interval)


def main() -> int:
    ap = argparse.ArgumentParser(description="Rooted ingestion worker")
    ap.add_argument("--db", default=None, help="path to rooted.db")
    ap.add_argument("--once", action="store_true", help="drain the queue and exit")
    ap.add_argument("--interval", type=float, default=DEFAULT_INTERVAL)
    ap.add_argument("--lease", type=int, default=DEFAULT_LEASE,
                    help="seconds before an interrupted job is reclaimed")
    ap.add_argument("--auto-verify", action="store_true",
                    help="skip review for perfectly extracted typed documents")
    args = ap.parse_args()

    db_path = Path(args.db) if args.db else default_db_path()
    db_path.parent.mkdir(parents=True, exist_ok=True)
    conn = connect(db_path)
    apply_migrations(conn)

    worker = Worker(conn, lease=args.lease, auto_verify=args.auto_verify)
    print(f"[worker] {worker.id} watching {db_path}", flush=True)
    if args.once:
        advanced = worker.tick()
        print(f"[worker] advanced {advanced} job(s)", flush=True)
        return 0
    try:
        worker.run(interval=args.interval)
    except KeyboardInterrupt:
        print("[worker] stopped", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
