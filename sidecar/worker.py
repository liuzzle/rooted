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
import contextlib
import hashlib
import json
import os
import sqlite3
import sys
import time
import uuid
from pathlib import Path
from typing import Optional

import concepts
import embeddings
import engines
import references
from engines import (
    EngineUnavailable,
    escalate_extraction,
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
# Local settings
# ---------------------------------------------------------------------------

# Settings that must be the same for the app and the worker are deliberately
# not read from a file: if the two processes disagreed about which database
# they share, uploads would vanish into a second one. Those stay real
# environment variables, set before the app starts.
ENV_FILE_REFUSES = {"ROOTED_DB", "ROOTED_WORKER", "ROOTED_PYTHON"}


def parse_env_file(text: str) -> list[tuple[str, str]]:
    """`KEY=value` lines, as a person would write them.

    Blank lines and `#` comments are skipped, a leading `export` is tolerated,
    and one layer of matching quotes is stripped. Deliberately not a shell: no
    interpolation, no command substitution, nothing that makes the file able to
    do more than name a value.
    """
    pairs: list[tuple[str, str]] = []
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("export "):
            line = line[len("export "):].lstrip()
        key, sep, value = line.partition("=")
        if not sep:
            continue
        key, value = key.strip(), value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            value = value[1:-1]
        if key:
            pairs.append((key, value))
    return pairs


def load_env_file(path: Path) -> list[str]:
    """Apply one `.env` file. Returns the keys it set.

    A real environment variable always wins: the file is a convenience for
    things a GUI app can't inherit from your shell, not an override of what you
    explicitly exported.
    """
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as exc:
        print(f"[worker] could not read {path}: {exc}", file=sys.stderr, flush=True)
        return []

    applied = []
    for key, value in parse_env_file(text):
        if key in ENV_FILE_REFUSES:
            print(
                f"[worker] ignoring {key} in {path}: it has to match what the app"
                " uses, so set it as an environment variable instead",
                file=sys.stderr,
                flush=True,
            )
            continue
        if key in os.environ:
            continue
        os.environ[key] = value
        applied.append(key)
    return applied


def load_local_env(db_path: Optional[Path] = None) -> list[Path]:
    """Read `.env` from, in order: `ROOTED_ENV`, the repo, and beside the
    database.

    The last one is the point: a packaged app launched from the Finder gets
    launchd's environment, not your shell's, so a token exported in `.zshrc` is
    invisible to it. A file next to `rooted.db` is somewhere both a development
    checkout and an installed app can find.
    """
    candidates = []
    if os.environ.get("ROOTED_ENV"):
        candidates.append(Path(os.environ["ROOTED_ENV"]).expanduser())
    candidates.append(REPO_ROOT / ".env")
    candidates.append((db_path or default_db_path()).parent / ".env")

    loaded, seen = [], set()
    for path in candidates:
        resolved = path.resolve() if path.exists() else path
        if resolved in seen or not path.is_file():
            continue
        seen.add(resolved)
        if load_env_file(path):
            loaded.append(path)
    return loaded


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


def read_document(
    path: Path, fmt: str, pages_dir: Path, escalate: bool = False
) -> Extraction:
    """Dispatch a document to the engine that can read it.

    A PDF is tried as a typed document first and falls back to OCR: a scan
    saved as PDF is the common case, and the two are indistinguishable by
    extension.

    `escalate` re-reads a scan's lines off the machine afterwards. On-device
    OCR still runs first and still supplies the boxes — escalation is a second
    reading of the same lines, never a different way of finding them.
    """
    extraction = _read_document(path, fmt, pages_dir)
    if escalate:
        if not extraction.pages:
            raise ExtractionError(
                f"a {fmt} file is read on this machine; there is nothing to "
                "send to the cloud"
            )
        extraction = escalate_extraction(extraction)
    return extraction


def _read_document(path: Path, fmt: str, pages_dir: Path) -> Extraction:
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
        self._engines: Optional[str] = None
        # When the embedding model was last looked for, and whether it was
        # there. Asked at most once a minute: Ollama may be started after the
        # app, and polling it every tick would be noise in its log.
        self._embed_checked = 0.0
        self._embed_ready = False
        self._verses_idle_until = 0.0

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
        self.publish_engines()

    def publish_engines(self) -> None:
        """Report what this machine can read, so the app can say so *before* a
        file is uploaded rather than failing the job afterwards.

        Computed once per process: availability changes when someone installs a
        package, and the worker restarts with the app. Probing it every tick
        would import torch on every pass.
        """
        if self._engines is None:
            self._engines = json.dumps(engines.describe())
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES ('worker_engines', ?)"
            " ON CONFLICT(key) DO UPDATE SET value = excluded.value,"
            " updated_at = datetime('now')",
            (self._engines,),
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
        # Cleared whatever happens next. Sending a page off the machine is a
        # decision made once, per job; a retry afterwards must not repeat it
        # silently.
        escalating = bool(job["escalate"])
        if escalating:
            self.conn.execute(
                "UPDATE jobs SET escalate = 0 WHERE job_id = ?", (job_id,)
            )
        try:
            if not path.exists():
                raise ExtractionError(f"the uploaded file is missing: {path}")
            extraction = read_document(
                path, job["format"], self.pages_dir(job), escalate=escalating
            )
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

    # -- concepts -----------------------------------------------------------

    def index_notes(self, limit: int = 5) -> int:
        """Read topics out of notes that haven't been read yet.

        Works from `notes`, not from `jobs`: a note typed in the app is as much
        a source as one that arrived as a scan, and the graph would be a lie if
        it only covered what came through ingestion.

        A note is re-read when its text changes — mentions cite character
        ranges, and an edited note moves them. Re-reading replaces that note's
        chunks outright rather than patching, so a mention can never point into
        text that is no longer there.
        """
        stale = self.conn.execute(
            """SELECT n.note_id, n.title, n.body, s.source_id, s.text_hash, s.scheme
                 FROM notes n
                 LEFT JOIN sources s ON s.note_id = n.note_id
                WHERE n.body <> ''
                ORDER BY n.updated_at DESC
                LIMIT 200"""
        ).fetchall()

        indexed = 0
        for note in stale:
            digest = hashlib.sha256(note["body"].encode("utf-8")).hexdigest()
            unchanged = (
                note["source_id"] is not None
                and note["text_hash"] == digest
                # Improving the extractor has to reach notes already read, or
                # the graph is a mix of rules nobody can reason about.
                and note["scheme"] == concepts.SCHEME
            )
            if unchanged:
                continue
            self.index_note(note["note_id"], note["body"], digest)
            indexed += 1
            if indexed >= limit:
                break
        return indexed

    def index_note(self, note_id: int, body: str, digest: str) -> None:
        """Find this note's topics, then find where each one is written.

        Two steps on purpose. Topics are proposed from the *whole* note, since
        that is the unit a person writes about something in; each one is then
        located in each paragraph, so every stored mention has offsets into the
        chunk that will cite it. A topic that doesn't appear in a paragraph
        simply has no mention there.
        """
        lang = concepts.detect_language(body)
        # Found first, so neither extractor mistakes a citation for a subject.
        cited = [(r.start, r.end) for r in references.find_references(body)]
        candidates: list[tuple[str, str]] = [
            (label, "phrases")
            for label in concepts.deterministic_labels(body, lang, cited)
        ]
        extractors = ["phrases"]

        if concepts.ollama_available():
            model = f"ollama/{concepts.OLLAMA_MODEL}"
            try:
                labels, rejected = concepts.ollama_labels(body)
                candidates.extend((label, model) for label in labels)
                extractors.append(model)
                if rejected:
                    # The gate doing its job, not an error — but worth seeing:
                    # a rising count is how you learn an extractor has started
                    # inventing rather than reading.
                    print(f"[worker] note {note_id}: the gate rejected "
                          f"{len(rejected)} non-verbatim candidate(s) from "
                          f"{model}: {rejected[:5]}", file=sys.stderr, flush=True)
            except concepts.ExtractionRejected as exc:
                # Optional by design; the baseline already ran.
                print(f"[worker] local model unavailable: {exc}",
                      file=sys.stderr, flush=True)

        # First proposer of a label wins, so the deterministic pass sets the
        # spelling and the model can only add topics, never rename one.
        seen: set[str] = set()
        unique: list[tuple[str, str]] = []
        for label, by in candidates:
            key = concepts.key_of(label, lang)
            if key and key not in seen:
                seen.add(key)
                unique.append((label, by))

        self.conn.execute("BEGIN IMMEDIATE")
        try:
            source_id = self.upsert_source(note_id, digest, ",".join(extractors), lang)
            # Replacing wholesale is what keeps offsets honest; the cascade
            # takes the old mentions with the old chunks.
            self.conn.execute("DELETE FROM chunks WHERE source_id = ?", (source_id,))
            for idx, (start, end, text) in enumerate(concepts.chunk_text(body)):
                refs = references.find_references(text)
                cited_here = [(r.start, r.end) for r in refs]
                cur = self.conn.execute(
                    """INSERT INTO chunks (source_id, idx, char_start, char_end, text)
                       VALUES (?,?,?,?,?)""",
                    (source_id, idx, start, end, text),
                )
                chunk_id = cur.lastrowid
                for label, by in unique:
                    # Not every topic is in every paragraph; absence here is
                    # ordinary, unlike absence from the note as a whole.
                    for mention in concepts.locate_mentions(
                        text, label, by, lang=lang, skip=cited_here
                    ):
                        self.record_mention(chunk_id, mention, lang, label)
                for ref in refs:
                    self.record_reference(chunk_id, ref)
            self.conn.execute("COMMIT")
        except Exception:
            self.conn.execute("ROLLBACK")
            raise

    def upsert_source(self, note_id: int, digest: str, indexed_by: str,
                      lang: str) -> int:
        self.conn.execute(
            """INSERT INTO sources
                 (kind, note_id, text_hash, indexed_at, indexed_by, scheme, lang)
               VALUES ('note', ?, ?, datetime('now'), ?, ?, ?)
               -- The unique index is partial (notes only), so the conflict
               -- target has to repeat its predicate.
               ON CONFLICT(note_id) WHERE note_id IS NOT NULL DO UPDATE SET
                 text_hash = excluded.text_hash,
                 indexed_at = excluded.indexed_at,
                 indexed_by = excluded.indexed_by,
                 scheme = excluded.scheme,
                 lang = excluded.lang""",
            (note_id, digest, indexed_by, concepts.SCHEME, lang),
        )
        return self.conn.execute(
            "SELECT source_id FROM sources WHERE note_id = ?", (note_id,)
        ).fetchone()["source_id"]

    def record_mention(self, chunk_id: int, mention: concepts.Mention,
                       lang: str, label: str) -> None:
        """One mention, filed under its concept.

        `label` is the form the extractor chose for the topic as a whole — the
        one the note uses most — while the mention keeps the form written at
        *this* spot. Without the distinction a note saying "Herrn" before
        "Herr" would name the topic after the declined form it happened to
        write first.
        """
        key = concepts.key_of(label, lang)
        # A spelling a person merged into another topic goes to that topic;
        # without this, the next re-read would quietly undo the merge.
        alias = self.conn.execute(
            "SELECT concept_id FROM concept_aliases WHERE key = ?", (key,)
        ).fetchone()
        if alias is not None:
            concept_id = alias["concept_id"]
        else:
            self.conn.execute(
                "INSERT INTO concepts (label, key) VALUES (?, ?)"
                " ON CONFLICT(key) DO NOTHING",
                (label, key),
            )
            concept_id = self.conn.execute(
                "SELECT concept_id FROM concepts WHERE key = ?", (key,)
            ).fetchone()["concept_id"]
        self.conn.execute(
            """INSERT INTO concept_mentions
                 (concept_id, chunk_id, char_start, char_end, surface,
                  extracted_by, confidence)
               VALUES (?,?,?,?,?,?,?)
               ON CONFLICT(chunk_id, char_start, char_end, concept_id) DO NOTHING""",
            (concept_id, chunk_id, mention.start, mention.end, mention.surface,
             mention.extracted_by, mention.confidence),
        )

    def record_reference(self, chunk_id: int, ref: references.Reference) -> None:
        """A scripture reference the note wrote, kept where it was written.

        Whether the verse exists in an installed translation is deliberately
        not decided here — that answer changes when a pack is added or removed,
        and a stored one would go stale.
        """
        self.conn.execute(
            """INSERT INTO verse_links
                 (chunk_id, char_start, char_end, surface, book_osis, chapter,
                  verse, verse_end, verse_id)
               VALUES (?,?,?,?,?,?,?,?,?)
               ON CONFLICT(chunk_id, char_start, char_end) DO NOTHING""",
            (chunk_id, ref.start, ref.end, ref.surface, ref.book_osis,
             ref.chapter, ref.verse, ref.verse_end, ref.verse_id),
        )

    # -- vectors ------------------------------------------------------------

    EMBED_RECHECK = 60.0

    @contextlib.contextmanager
    def transaction(self):
        self.conn.execute("BEGIN IMMEDIATE")
        try:
            yield
            self.conn.execute("COMMIT")
        except Exception:
            self.conn.execute("ROLLBACK")
            raise

    def embedding_ready(self) -> bool:
        now = time.monotonic()
        if now - self._embed_checked >= self.EMBED_RECHECK:
            self._embed_checked = now
            self._embed_ready = embeddings.model_available()
            self.publish_embedding()
        return self._embed_ready

    def publish_embedding(self) -> None:
        """Tell the app which model the vectors come from, and where it runs.

        The app embeds the query itself, and a query embedded by any other model
        would be compared against vectors it can't be compared with. Saying it
        here keeps one place that decides — this worker and its `.env`.
        """
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES ('embedding', ?)"
            " ON CONFLICT(key) DO UPDATE SET value = excluded.value,"
            " updated_at = datetime('now')",
            (json.dumps({
                "model": embeddings.EMBED_MODEL,
                "host": embeddings.OLLAMA_HOST,
                "available": self._embed_ready,
            }),),
        )

    def embed_pending(self, budget: float = 3.0) -> int:
        """Give unread passages a vector, notes first, for a few seconds.

        Notes come first because they are what a person searches for and there
        are few of them; a translation is thirty thousand verses and fills in
        behind, the one being read before the others. Bounded by time rather
        than count so a slow machine still returns to the job queue promptly.
        """
        if not self.embedding_ready():
            return 0
        model = embeddings.EMBED_MODEL
        deadline = time.monotonic() + budget
        done = 0
        try:
            while time.monotonic() < deadline:
                rows = self.conn.execute(
                    """SELECT chunk_id, text FROM chunks
                        WHERE embedded_by IS NULL OR embedded_by <> ?
                        ORDER BY chunk_id LIMIT ?""",
                    (model, embeddings.BATCH),
                ).fetchall()
                if not rows:
                    break
                vectors = embeddings.embed([r["text"] for r in rows], model)
                with self.transaction():
                    # A chunk replaced meanwhile is simply gone; ids are never
                    # reused, so this can't land on the new text.
                    self.conn.executemany(
                        "UPDATE chunks SET embedding = ?, embedded_by = ?"
                        " WHERE chunk_id = ?",
                        [(embeddings.pack(v), model, r["chunk_id"])
                         for r, v in zip(rows, vectors)],
                    )
                done += len(rows)

            # Topic labels, for merge suggestions. Few, short, and only the ones
            # something still cites.
            while time.monotonic() < deadline:
                rows = self.conn.execute(
                    """SELECT c.concept_id, c.label FROM concepts c
                        WHERE (c.embedded_by IS NULL OR c.embedded_by <> ?)
                          AND EXISTS (SELECT 1 FROM concept_mentions m
                                       WHERE m.concept_id = c.concept_id)
                        ORDER BY c.concept_id LIMIT ?""",
                    (model, embeddings.BATCH),
                ).fetchall()
                if not rows:
                    break
                vectors = embeddings.embed([r["label"] for r in rows], model)
                with self.transaction():
                    self.conn.executemany(
                        "UPDATE concepts SET embedding = ?, embedded_by = ?"
                        " WHERE concept_id = ?",
                        [(embeddings.pack(v), model, r["concept_id"])
                         for r, v in zip(rows, vectors)],
                    )
                done += len(rows)

            # Once every verse has a vector, finding that out again is a scan
            # of the whole Bible; do it once a minute, not every tick.
            verses_due = time.monotonic() >= self._verses_idle_until
            while verses_due and time.monotonic() < deadline:
                rows = self.conn.execute(
                    """SELECT v.translation_id, v.verse_id, v.text
                         FROM verses v
                         JOIN translations t ON t.id = v.translation_id
                         LEFT JOIN verse_vectors vv
                           ON vv.translation_id = v.translation_id
                          AND vv.verse_id = v.verse_id
                          AND vv.model = ?
                        WHERE vv.verse_id IS NULL
                        ORDER BY t.abbrev = (SELECT value FROM settings
                                              WHERE key = 'active_translation') DESC,
                                 v.translation_id, v.canonical_order
                        LIMIT ?""",
                    (model, embeddings.BATCH),
                ).fetchall()
                if not rows:
                    self._verses_idle_until = time.monotonic() + self.EMBED_RECHECK
                    break
                vectors = embeddings.embed([r["text"] for r in rows], model)
                with self.transaction():
                    # REPLACE, so a vector from a previous model is overwritten
                    # rather than left to be compared against the wrong query.
                    self.conn.executemany(
                        """INSERT OR REPLACE INTO verse_vectors
                             (translation_id, verse_id, model, vector)
                           SELECT ?, ?, ?, ?
                            WHERE EXISTS (SELECT 1 FROM verses
                                           WHERE translation_id = ? AND verse_id = ?)""",
                        [(r["translation_id"], r["verse_id"], model,
                          embeddings.pack(v), r["translation_id"], r["verse_id"])
                         for r, v in zip(rows, vectors)],
                    )
                done += len(rows)
        except embeddings.EmbeddingUnavailable as exc:
            # The server went away mid-run. Search by words is unaffected;
            # look again on the next check rather than every tick.
            print(f"[worker] embedding paused: {exc}", file=sys.stderr, flush=True)
            self._embed_ready = False
            self._embed_checked = time.monotonic()
            self.publish_embedding()
        return done

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

        # Notes are read for topics after they exist, whether they came from a
        # job or were typed in the app. A few per tick: this is background work
        # and must never make the app wait.
        advanced += self.index_notes()
        # Vectors last: they're the only step that can take seconds, and
        # everything above is something a person is waiting to see.
        self.embed_pending()

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
    for path in load_local_env(db_path):
        # Names only — a token belongs in the file, not in the log.
        print(f"[worker] read settings from {path}", flush=True)
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
