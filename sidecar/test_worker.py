#!/usr/bin/env python3
"""
Worker tests — stdlib only:  python3 -m unittest discover -s sidecar

Covers the three properties the pipeline is built on: it resumes after a crash,
its stages are idempotent, and no note is ever written from text a human hasn't
verified.
"""
from __future__ import annotations

import sqlite3
import tempfile
import unittest
import zipfile
from pathlib import Path

import worker as w


def make_docx(path: Path, paragraphs: list[str]) -> None:
    """A minimal but real .docx — a zip holding WordprocessingML."""
    body = "".join(
        f"<w:p><w:r><w:t>{p}</w:t></w:r></w:p>" for p in paragraphs
    )
    xml = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">'
        f"<w:body>{body}</w:body></w:document>"
    )
    with zipfile.ZipFile(path, "w") as zf:
        zf.writestr("word/document.xml", xml)


class PipelineTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        self.conn = w.connect(self.dir / "test.db")
        w.apply_migrations(self.conn)
        self.worker = w.Worker(self.conn, lease=60)

    def tearDown(self) -> None:
        self.conn.close()
        self.tmp.cleanup()

    # -- helpers ------------------------------------------------------------

    def upload(self, filename: str, content: bytes | None = None, **meta) -> int:
        path = self.dir / filename
        if content is not None:
            path.write_bytes(content)
        fmt = filename.rsplit(".", 1)[-1].lower()
        cur = self.conn.execute(
            """INSERT INTO documents
                 (filename, stored_path, format, byte_size, sha256, title, doc_date,
                  speaker, context)
               VALUES (?,?,?,?,'x',?,?,?,?)""",
            (filename, str(path), fmt, len(content or b""), meta.get("title"),
             meta.get("doc_date"), meta.get("speaker"), meta.get("context")),
        )
        doc_id = cur.lastrowid
        cur = self.conn.execute(
            "INSERT INTO jobs (doc_id, state) VALUES (?, ?)", (doc_id, w.UPLOADED)
        )
        return cur.lastrowid

    def state(self, job_id: int) -> str:
        return self.conn.execute(
            "SELECT state FROM jobs WHERE job_id = ?", (job_id,)
        ).fetchone()["state"]

    def extraction(self, job_id: int) -> sqlite3.Row:
        return self.conn.execute(
            "SELECT * FROM extractions WHERE job_id = ?", (job_id,)
        ).fetchone()

    def verify(self, job_id: int, text: str | None = None) -> None:
        """Stand in for the human doing it in the app."""
        row = self.extraction(job_id)
        self.conn.execute(
            "UPDATE extractions SET text = ?, verified = 1, edited = ? WHERE job_id = ?",
            (text if text is not None else row["text"],
             1 if text is not None and text != row["text"] else 0, job_id),
        )
        self.conn.execute("UPDATE jobs SET state = ? WHERE job_id = ?", (w.VERIFIED, job_id))

    # -- extraction ---------------------------------------------------------

    def test_plain_text_extracts_verbatim(self):
        job_id = self.upload("notes.txt", "Line one\nLine two\n".encode())
        self.worker.tick()
        self.assertEqual(self.state(job_id), w.NEEDS_REVIEW)
        row = self.extraction(job_id)
        self.assertEqual(row["text"], "Line one\nLine two\n")
        self.assertEqual(row["confidence"], 1.0)
        self.assertEqual(row["verified"], 0)

    def test_docx_paragraphs_extract(self):
        path = self.dir / "talk.docx"
        make_docx(path, ["The first paragraph.", "The second paragraph."])
        job_id = self.upload("talk.docx")
        self.worker.tick()
        self.assertEqual(self.state(job_id), w.NEEDS_REVIEW)
        self.assertEqual(
            self.extraction(job_id)["text"],
            "The first paragraph.\nThe second paragraph.",
        )

    def test_non_utf8_text_is_flagged_for_review(self):
        # cp1252 curly quote: decodable as latin-1 but possibly wrong.
        job_id = self.upload("old.txt", b"The Lord\x92s word")
        self.worker.tick()
        row = self.extraction(job_id)
        self.assertLess(row["confidence"], 1.0)
        self.assertEqual(self.state(job_id), w.NEEDS_REVIEW)

    def test_unreadable_file_fails_with_a_useful_message(self):
        job_id = self.upload("broken.docx", b"this is not a zip")
        self.worker.tick()
        self.assertEqual(self.state(job_id), w.ERROR)
        error = self.conn.execute(
            "SELECT last_error FROM jobs WHERE job_id = ?", (job_id,)
        ).fetchone()["last_error"]
        self.assertIn("docx", error)

    def test_missing_file_errors_rather_than_inventing(self):
        job_id = self.upload("gone.txt")  # no bytes written
        self.worker.tick()
        self.assertEqual(self.state(job_id), w.ERROR)
        self.assertIsNone(self.extraction(job_id))

    # -- the human gate -----------------------------------------------------

    def test_no_note_without_human_verification(self):
        job_id = self.upload("notes.txt", b"machine text")
        self.worker.tick()

        # Force the job forward without anyone verifying the extraction.
        self.conn.execute("UPDATE jobs SET state = ? WHERE job_id = ?", (w.VERIFIED, job_id))
        self.worker.tick()

        self.assertEqual(self.state(job_id), w.ERROR)
        self.assertEqual(
            self.conn.execute("SELECT COUNT(*) c FROM notes").fetchone()["c"], 0
        )

    def test_verified_text_becomes_a_note_carrying_its_metadata(self):
        job_id = self.upload(
            "talk.txt", b"scribbled text",
            title="Sunday evening", doc_date="2026-08-16",
            speaker="A. Speaker", context="Evening service",
        )
        self.worker.tick()
        self.verify(job_id, "corrected text")
        self.worker.tick()

        self.assertEqual(self.state(job_id), w.DONE)
        note = self.conn.execute("SELECT * FROM notes").fetchone()
        self.assertEqual(note["body"], "corrected text")
        self.assertEqual(note["title"], "Sunday evening")
        self.assertEqual(note["speaker"], "A. Speaker")
        self.assertEqual(note["date"], "2026-08-16")
        self.assertEqual(note["context"], "Evening service")
        note_id = self.conn.execute(
            "SELECT note_id FROM jobs WHERE job_id = ?", (job_id,)
        ).fetchone()["note_id"]
        self.assertEqual(note_id, note["note_id"])

    def test_untitled_documents_fall_back_to_the_filename(self):
        job_id = self.upload("2026-08-16 evening.txt", b"text")
        self.worker.tick()
        self.verify(job_id)
        self.worker.tick()
        self.assertEqual(
            self.conn.execute("SELECT title FROM notes").fetchone()["title"],
            "2026-08-16 evening",
        )

    # -- idempotence & resume ----------------------------------------------

    def test_publishing_twice_updates_one_note(self):
        job_id = self.upload("notes.txt", b"text")
        self.worker.tick()
        self.verify(job_id)
        self.worker.tick()

        # Someone corrects the text and re-verifies before deleting the job.
        self.conn.execute(
            "UPDATE extractions SET text = 'edited text' WHERE job_id = ?", (job_id,)
        )
        self.conn.execute("UPDATE jobs SET state = ? WHERE job_id = ?", (w.VERIFIED, job_id))
        self.worker.tick()

        notes = self.conn.execute("SELECT * FROM notes").fetchall()
        self.assertEqual(len(notes), 1, "a re-run must not duplicate the note")
        self.assertEqual(notes[0]["body"], "edited text")

    def test_re_extraction_replaces_rather_than_accumulates(self):
        job_id = self.upload("notes.txt", b"first")
        self.worker.tick()
        (self.dir / "notes.txt").write_bytes(b"second")
        self.conn.execute(
            "UPDATE jobs SET state = ?, claimed_at = NULL, claimed_by = NULL"
            " WHERE job_id = ?", (w.UPLOADED, job_id),
        )
        self.worker.tick()

        rows = self.conn.execute(
            "SELECT * FROM extractions WHERE job_id = ?", (job_id,)
        ).fetchall()
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["text"], "second")
        self.assertEqual(rows[0]["verified"], 0, "new text needs reviewing again")

    def test_a_job_interrupted_mid_stage_is_reclaimed(self):
        job_id = self.upload("notes.txt", b"text")
        # Simulate a worker that claimed the job and then died.
        self.conn.execute(
            """UPDATE jobs SET state = ?, claimed_by = 'dead-worker',
                      claimed_at = datetime('now', '-1 hour') WHERE job_id = ?""",
            (w.EXTRACTING, job_id),
        )

        fresh = w.Worker(self.conn, lease=60)
        fresh.tick()

        self.assertEqual(self.state(job_id), w.NEEDS_REVIEW)
        self.assertEqual(self.extraction(job_id)["text"], "text")

    def test_a_live_claim_is_left_alone(self):
        job_id = self.upload("notes.txt", b"text")
        self.conn.execute(
            """UPDATE jobs SET state = ?, claimed_by = 'busy-worker',
                      claimed_at = datetime('now') WHERE job_id = ?""",
            (w.EXTRACTING, job_id),
        )
        w.Worker(self.conn, lease=60).tick()
        self.assertEqual(self.state(job_id), w.EXTRACTING, "another worker is on it")

    def test_repeated_interruptions_eventually_stop(self):
        job_id = self.upload("notes.txt", b"text")
        self.conn.execute(
            """UPDATE jobs SET state = ?, attempts = ?, claimed_by = 'dead',
                      claimed_at = datetime('now', '-1 hour') WHERE job_id = ?""",
            (w.EXTRACTING, w.MAX_ATTEMPTS, job_id),
        )
        self.worker.tick()
        self.assertEqual(self.state(job_id), w.ERROR)
        self.assertIn(
            "interrupted",
            self.conn.execute(
                "SELECT last_error FROM jobs WHERE job_id = ?", (job_id,)
            ).fetchone()["last_error"],
        )

    def test_two_workers_never_claim_the_same_job(self):
        job_id = self.upload("notes.txt", b"text")
        other = w.Worker(w.connect(self.dir / "test.db"), lease=60)

        first = self.worker.claim()
        second = other.claim()

        self.assertIsNotNone(first)
        self.assertIsNone(second, "the job was already claimed")
        self.assertEqual(first["job_id"], job_id)

    # -- auto-verify --------------------------------------------------------

    def test_auto_verify_is_off_by_default(self):
        job_id = self.upload("notes.txt", b"clean text")
        self.worker.tick()
        self.assertEqual(self.state(job_id), w.NEEDS_REVIEW)

    def test_auto_verify_only_applies_to_certain_extractions(self):
        clean = self.upload("clean.txt", "clean text".encode())
        murky = self.upload("murky.txt", b"The Lord\x92s word")
        auto = w.Worker(self.conn, lease=60, auto_verify=True)
        auto.tick()

        self.assertEqual(self.state(clean), w.DONE)
        self.assertEqual(self.state(murky), w.NEEDS_REVIEW,
                         "anything less than certain still needs a person")


if __name__ == "__main__":
    unittest.main()
