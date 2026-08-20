#!/usr/bin/env python3
"""
Worker tests — stdlib only:  python3 -m unittest discover -s sidecar

Covers the three properties the pipeline is built on: it resumes after a crash,
its stages are idempotent, and no note is ever written from text a human hasn't
verified.
"""
from __future__ import annotations

import json
import os
import shutil
import sqlite3
import subprocess
import tempfile
import unittest
import zipfile
from pathlib import Path

import engines
import worker as w


def _vision_available() -> bool:
    return engines.vision_available()


def _say_available() -> bool:
    """macOS speech synthesis, used to make a recording to transcribe."""
    return shutil.which("say") is not None


def _record(path: Path, text: str, voice: str = "Daniel") -> Path:
    """Speak `text` into an AIFF, so a transcription test has real speech in it
    rather than a checked-in binary fixture."""
    subprocess.run(["say", "-v", voice, "-o", str(path), text], check=True)
    return path


class _stub:
    """Swap a module attribute for the duration of a test."""

    def __init__(self, module, name: str, replacement) -> None:
        self.module, self.name, self.replacement = module, name, replacement

    def __enter__(self):
        self.original = getattr(self.module, self.name)
        setattr(self.module, self.name, self.replacement)

    def __exit__(self, *exc):
        setattr(self.module, self.name, self.original)
        return False


class _without:
    """Pretend an engine isn't installed, so its absence can be tested on a
    machine where it is."""

    def __init__(self, module, name: str) -> None:
        self.module, self.name = module, name

    def __enter__(self):
        self.original = getattr(self.module, self.name)
        setattr(self.module, self.name, lambda *a, **k: False)

    def __exit__(self, *exc):
        setattr(self.module, self.name, self.original)
        return False


def _render_page(path: Path) -> Path:
    """Draw a page of note-like text so OCR has something real to read.

    Uses Quartz directly — no image library, and no checked-in binary fixture
    that could drift from what the engine actually sees.
    """
    import Quartz
    from Foundation import NSURL

    width, height = 1000, 700
    space = Quartz.CGColorSpaceCreateDeviceRGB()
    ctx = Quartz.CGBitmapContextCreate(
        None, width, height, 8, 0, space,
        Quartz.kCGImageAlphaPremultipliedFirst | Quartz.kCGBitmapByteOrder32Host,
    )
    Quartz.CGContextSetRGBFillColor(ctx, 1, 1, 1, 1)
    Quartz.CGContextFillRect(ctx, Quartz.CGRectMake(0, 0, width, height))
    Quartz.CGContextSetRGBFillColor(ctx, 0, 0, 0, 1)
    Quartz.CGContextSetTextMatrix(ctx, Quartz.CGAffineTransformIdentity)

    # (text, x, y-from-bottom, size)
    lines = [
        ("Covenant - Abraham", 60, 620, 36),
        ("promise repeated to Isaac", 100, 540, 28),
        ("and again to Jacob", 100, 490, 28),
        ("see Galatians 3", 140, 420, 26),
        ("cf. Romans 4", 640, 380, 24),
        ("fulfilled in Christ", 100, 260, 30),
    ]
    for text, x, y, size in lines:
        Quartz.CGContextSelectFont(ctx, b"Helvetica", size, Quartz.kCGEncodingMacRoman)
        Quartz.CGContextSetTextDrawingMode(ctx, Quartz.kCGTextFill)
        Quartz.CGContextShowTextAtPoint(ctx, x, y, text.encode("mac-roman"), len(text))

    image = Quartz.CGBitmapContextCreateImage(ctx)
    dest = Quartz.CGImageDestinationCreateWithURL(
        NSURL.fileURLWithPath_(str(path)), "public.png", 1, None
    )
    Quartz.CGImageDestinationAddImage(dest, image, None)
    Quartz.CGImageDestinationFinalize(dest)
    return path


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

    # -- scans --------------------------------------------------------------

    def test_a_scan_produces_positioned_spans_and_a_page(self):
        if not _vision_available():
            self.skipTest("Vision bindings not installed")
        image = _render_page(self.dir / "page.png")
        job_id = self.upload("page.png", image.read_bytes())
        self.worker.tick()

        self.assertEqual(self.state(job_id), w.NEEDS_REVIEW)
        doc_id = self.conn.execute(
            "SELECT doc_id FROM jobs WHERE job_id = ?", (job_id,)
        ).fetchone()["doc_id"]

        pages = self.conn.execute(
            "SELECT * FROM pages WHERE doc_id = ?", (doc_id,)
        ).fetchall()
        self.assertEqual(len(pages), 1)
        self.assertEqual(pages[0]["width"], 1000)

        spans = self.conn.execute(
            "SELECT * FROM spans WHERE doc_id = ? ORDER BY idx", (doc_id,)
        ).fetchall()
        self.assertGreaterEqual(len(spans), 3)
        # Every span knows where it was on the page.
        for span in spans:
            for axis in ("x", "y", "w", "h"):
                self.assertIsNotNone(span[axis])
                self.assertGreaterEqual(span[axis], 0.0)
                self.assertLessEqual(span[axis], 1.0)
        # The margin note is off to the right, not folded into the main column.
        margin = [s for s in spans if "Romans" in s["text"]]
        self.assertTrue(margin, f"expected a margin span, got {[s['text'] for s in spans]}")
        self.assertGreater(margin[0]["x"], 0.5)

    def test_a_scan_is_never_auto_verified(self):
        if not _vision_available():
            self.skipTest("Vision bindings not installed")
        image = _render_page(self.dir / "page.png")
        job_id = self.upload("page.png", image.read_bytes())
        # Even told to auto-verify, OCR output must be read by a person: the
        # engine reports full confidence for readings that are plainly wrong.
        w.Worker(self.conn, lease=60, auto_verify=True).tick()
        self.assertEqual(self.state(job_id), w.NEEDS_REVIEW)

    # -- recordings ---------------------------------------------------------

    def test_audio_without_an_engine_says_so(self):
        """The message has to name what to install — the job can't be retried
        into working otherwise."""
        job_id = self.upload("talk.mp3", b"not really audio")
        self.assertEqual(w.kind_of("mp3"), "audio")
        with _without(engines, "asr_available"):
            self.worker.tick()

        self.assertEqual(self.state(job_id), w.ERROR)
        error = self.conn.execute(
            "SELECT last_error FROM jobs WHERE job_id = ?", (job_id,)
        ).fetchone()["last_error"]
        self.assertIn("faster-whisper", error)

    def test_unreadable_audio_says_so_in_words(self):
        """A file the decoder chokes on is reported as a bad recording, not as
        a raw exception from somewhere inside the stack."""
        if not engines.asr_available():
            self.skipTest("faster-whisper not installed")
        job_id = self.upload("talk.mp3", b"not really audio")
        self.worker.tick()

        self.assertEqual(self.state(job_id), w.ERROR)
        error = self.conn.execute(
            "SELECT last_error FROM jobs WHERE job_id = ?", (job_id,)
        ).fetchone()["last_error"]
        self.assertIn("talk.mp3", error)
        self.assertIn("audio", error)
        # Not "InvalidDataError: [Errno 1094995529] ..." — that tells nobody
        # anything. Worker-level formatting of an unexpected exception is
        # `Type: message`; an engine that knows better says it in words.
        self.assertFalse(error.startswith("InvalidDataError"), error)

    def test_a_recording_becomes_timed_segments(self):
        """End-to-end transcription. Opt-in: the first run downloads a model."""
        if not os.environ.get("ROOTED_TEST_ASR"):
            self.skipTest("set ROOTED_TEST_ASR=1 to run transcription end-to-end")
        if not engines.asr_available() or not _say_available():
            self.skipTest("needs faster-whisper and macOS `say`")
        audio = _record(
            self.dir / "talk.aiff",
            "He shall be like a tree planted by the rivers of water. "
            "The fruit comes in its season.",
        )
        job_id = self.upload("talk.aiff", audio.read_bytes())
        # Even told to auto-verify: a transcript is a reading of sound, not a
        # copy of text, so a person still has to accept it.
        w.Worker(self.conn, lease=60, auto_verify=True).tick()

        self.assertEqual(self.state(job_id), w.NEEDS_REVIEW)
        doc_id = self.conn.execute(
            "SELECT doc_id FROM jobs WHERE job_id = ?", (job_id,)
        ).fetchone()["doc_id"]
        spans = self.conn.execute(
            "SELECT * FROM spans WHERE doc_id = ? ORDER BY idx", (doc_id,)
        ).fetchall()
        self.assertTrue(spans, "expected transcript segments")

        previous_end = 0.0
        for span in spans:
            # Every segment is findable in the recording, and they run forward.
            self.assertIsNotNone(span["start_s"])
            self.assertIsNotNone(span["end_s"])
            self.assertGreaterEqual(span["start_s"], previous_end - 0.001)
            self.assertGreater(span["end_s"], span["start_s"])
            previous_end = span["end_s"]
            # Space is for pages; a recording has no box on a page.
            self.assertIsNone(span["x"])
            # Speaker labels only exist when diarization ran.
            if not engines.diarization_available():
                self.assertIsNone(span["speaker"])

        if engines.diarization_available():
            # One voice, so one label — but every segment must carry it.
            labels = {s["speaker"] for s in spans}
            self.assertNotIn(None, labels, "diarization ran but labelled nothing")

        transcript = " ".join(s["text"] for s in spans).lower()
        self.assertIn("tree", transcript)

    # -- cloud escalation ---------------------------------------------------

    def escalated_scan(self, readings, **kw):
        """A scan job asked to be re-read in the cloud, with `readings`
        standing in for what comes back."""
        image = _render_page(self.dir / "page.png")
        job_id = self.upload("page.png", image.read_bytes())
        self.worker.tick()
        self.conn.execute("UPDATE jobs SET escalate = 1, state = ? WHERE job_id = ?",
                          (w.UPLOADED, job_id))
        with _stub(engines, "cloud_ocr_available", lambda: True), \
             _stub(engines, "read_lines_in_cloud", readings):
            self.worker.tick()
        return job_id

    def spans_of(self, job_id):
        doc_id = self.conn.execute(
            "SELECT doc_id FROM jobs WHERE job_id = ?", (job_id,)
        ).fetchone()["doc_id"]
        return self.conn.execute(
            "SELECT * FROM spans WHERE doc_id = ? ORDER BY idx", (doc_id,)
        ).fetchall()

    def test_a_crop_contains_the_line_it_claims_to(self):
        """The whole design rests on this: a crop sent for line N really is
        line N, so the reading that comes back can be put back on its box.
        Read each crop again on this machine and check it says the same thing.
        """
        if not _vision_available():
            self.skipTest("Vision bindings not installed")
        page = _render_page(self.dir / "page.png")
        extraction = engines.ocr_image(page)
        crops = engines.crop_spans(page, extraction.spans)

        self.assertEqual(len(crops), len(extraction.spans))
        for span, crop in zip(extraction.spans, crops):
            path = self.dir / f"crop-{span.idx}.png"
            path.write_bytes(crop)
            reread = " ".join(s.text for s in engines.ocr_image(path).spans)
            self.assertEqual(reread, span.text)

    def test_a_cloud_reading_replaces_the_text_and_keeps_the_box(self):
        """The whole point: a better reading, still anchored where it was."""
        if not _vision_available():
            self.skipTest("Vision bindings not installed")
        job_id = self.escalated_scan(
            lambda crops, offset: {offset + i: f"cloud line {offset + i}"
                                   for i in range(len(crops))}
        )

        self.assertEqual(self.state(job_id), w.NEEDS_REVIEW)
        spans = self.spans_of(job_id)
        self.assertTrue(spans)
        for span in spans:
            self.assertEqual(span["text"], f"cloud line {span['idx']}")
            # Position is Vision's, and survives being re-read.
            for axis in ("x", "y", "w", "h"):
                self.assertIsNotNone(span[axis])

    def test_a_line_the_cloud_cannot_read_keeps_what_this_machine_read(self):
        """An empty reading is not an erasure — and it has to stop the eye."""
        if not _vision_available():
            self.skipTest("Vision bindings not installed")
        job_id = self.escalated_scan(
            lambda crops, offset: {offset: "cloud line", **{
                offset + i: "" for i in range(1, len(crops))
            }}
        )
        spans = self.spans_of(job_id)
        self.assertEqual(spans[0]["text"], "cloud line")
        self.assertNotEqual(spans[1]["text"], "")
        self.assertEqual(spans[1]["confidence"], 0.0)

    def test_a_failed_cloud_reading_leaves_an_actionable_job(self):
        if not _vision_available():
            self.skipTest("Vision bindings not installed")

        def refuse(crops, offset):
            raise engines.ExtractionError("the cloud reader skipped line(s) [2]")

        job_id = self.escalated_scan(refuse)
        self.assertEqual(self.state(job_id), w.ERROR)
        error = self.conn.execute(
            "SELECT last_error FROM jobs WHERE job_id = ?", (job_id,)
        ).fetchone()["last_error"]
        self.assertIn("skipped line", error)

    def test_escalation_happens_once_and_is_not_inherited_by_a_retry(self):
        """Sending a page off the machine is a decision made once."""
        if not _vision_available():
            self.skipTest("Vision bindings not installed")
        job_id = self.escalated_scan(
            lambda crops, offset: {offset + i: "cloud" for i in range(len(crops))}
        )
        self.assertEqual(
            self.conn.execute(
                "SELECT escalate FROM jobs WHERE job_id = ?", (job_id,)
            ).fetchone()["escalate"],
            0,
        )

    def test_a_typed_document_has_nothing_to_send(self):
        job_id = self.upload("notes.txt", b"already text")
        self.conn.execute("UPDATE jobs SET escalate = 1 WHERE job_id = ?", (job_id,))
        with _stub(engines, "cloud_ocr_available", lambda: True):
            self.worker.tick()

        self.assertEqual(self.state(job_id), w.ERROR)
        error = self.conn.execute(
            "SELECT last_error FROM jobs WHERE job_id = ?", (job_id,)
        ).fetchone()["last_error"]
        self.assertIn("nothing to send", error)

    def test_escalating_without_a_key_says_what_is_missing(self):
        if not _vision_available():
            self.skipTest("Vision bindings not installed")
        image = _render_page(self.dir / "page.png")
        job_id = self.upload("page.png", image.read_bytes())
        self.worker.tick()
        self.conn.execute("UPDATE jobs SET escalate = 1, state = ? WHERE job_id = ?",
                          (w.UPLOADED, job_id))
        with _stub(engines, "cloud_ocr_available", lambda: False):
            self.worker.tick()

        error = self.conn.execute(
            "SELECT last_error FROM jobs WHERE job_id = ?", (job_id,)
        ).fetchone()["last_error"]
        self.assertIn("ANTHROPIC_API_KEY", error)

    # -- what this machine can read -----------------------------------------

    def test_the_worker_reports_which_engines_it_has(self):
        """The app can only warn about a missing engine before an upload if the
        worker says what it found."""
        self.worker.tick()
        reported = self.conn.execute(
            "SELECT value FROM settings WHERE key = 'worker_engines'"
        ).fetchone()["value"]
        found = {e["key"]: e for e in json.loads(reported)}

        self.assertIn("asr", found)
        self.assertEqual(found["asr"]["available"], engines.asr_available())
        self.assertEqual(found["ocr"]["available"], engines.vision_available())
        for engine in found.values():
            # An unavailable engine has to say what to do about it.
            self.assertTrue(engine["note"].strip(), engine)
            self.assertTrue(engine["label"].strip(), engine)

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


class CloudReadingTest(unittest.TestCase):
    """Checking an answer that came from off the machine, before believing it."""

    def answer(self, lines) -> str:
        return json.dumps({"lines": lines})

    def test_a_reading_per_line_comes_back_keyed_by_line(self):
        readings = engines.parse_cloud_readings(
            self.answer([
                {"index": 3, "text": "in the beginning", "unreadable": False},
                {"index": 4, "text": "God created", "unreadable": False},
            ]),
            offset=3,
            count=2,
        )
        self.assertEqual(readings, {3: "in the beginning", 4: "God created"})

    def test_an_unreadable_line_comes_back_empty_not_guessed(self):
        readings = engines.parse_cloud_readings(
            self.answer([{"index": 0, "text": "a guess", "unreadable": True}]),
            offset=0,
            count=1,
        )
        self.assertEqual(readings[0], "")

    def test_a_line_that_was_not_sent_is_refused(self):
        with self.assertRaises(engines.ExtractionError) as caught:
            engines.parse_cloud_readings(
                self.answer([{"index": 9999, "text": "elsewhere", "unreadable": False}]),
                offset=0,
                count=1,
            )
        self.assertIn("9999", str(caught.exception))

    def test_a_missing_line_is_refused_rather_than_left_stale(self):
        with self.assertRaises(engines.ExtractionError) as caught:
            engines.parse_cloud_readings(
                self.answer([{"index": 0, "text": "one", "unreadable": False}]),
                offset=0,
                count=2,
            )
        self.assertIn("skipped", str(caught.exception))

    def test_two_readings_for_one_line_are_refused(self):
        with self.assertRaises(engines.ExtractionError) as caught:
            engines.parse_cloud_readings(
                self.answer([
                    {"index": 0, "text": "one", "unreadable": False},
                    {"index": 0, "text": "or the other", "unreadable": False},
                ]),
                offset=0,
                count=1,
            )
        self.assertIn("twice", str(caught.exception))

    def test_an_unparseable_answer_is_refused(self):
        with self.assertRaises(engines.ExtractionError):
            engines.parse_cloud_readings("not json at all", offset=0, count=1)


class DiarizationShapeTest(unittest.TestCase):
    """Reading pyannote's answer, whichever version produced it.

    Fakes rather than the real pipeline: this is about the shape of the result,
    and the models are gated, large, and not everyone's to download.
    """

    class Turn:
        def __init__(self, start, end):
            self.start, self.end = start, end

    class Annotation:
        def __init__(self, turns):
            self.turns = turns

        def itertracks(self, yield_label=False):
            for start, end, speaker in self.turns:
                yield DiarizationShapeTest.Turn(start, end), None, speaker

    def test_pyannote_3_returns_the_annotation_itself(self):
        annotation = self.Annotation([(0.0, 1.5, "SPEAKER_00")])
        self.assertEqual(
            engines.diarization_turns(annotation), [(0.0, 1.5, "SPEAKER_00")]
        )

    def test_pyannote_4_wraps_two_annotations(self):
        """The exclusive one wins: an interjection shouldn't relabel the
        sentence it lands in."""

        class DiarizeOutput:
            speaker_diarization = DiarizationShapeTest.Annotation(
                [(0.0, 9.0, "SPEAKER_00"), (4.0, 5.0, "SPEAKER_01")]
            )
            exclusive_speaker_diarization = DiarizationShapeTest.Annotation(
                [(0.0, 4.0, "SPEAKER_00"), (5.0, 9.0, "SPEAKER_00")]
            )

        self.assertEqual(
            engines.diarization_turns(DiarizeOutput()),
            [(0.0, 4.0, "SPEAKER_00"), (5.0, 9.0, "SPEAKER_00")],
        )

    def test_a_wrapper_without_the_exclusive_one_still_reads(self):
        class DiarizeOutput:
            speaker_diarization = DiarizationShapeTest.Annotation(
                [(1.0, 2.0, "SPEAKER_07")]
            )
            exclusive_speaker_diarization = None

        self.assertEqual(
            engines.diarization_turns(DiarizeOutput()), [(1.0, 2.0, "SPEAKER_07")]
        )


class LocalSettingsTest(unittest.TestCase):
    """`.env` — the only place a GUI-launched app can find a token."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        self.original = dict(os.environ)

    def tearDown(self) -> None:
        os.environ.clear()
        os.environ.update(self.original)
        self.tmp.cleanup()

    def write(self, text: str, name: str = ".env") -> Path:
        path = self.dir / name
        path.write_text(text, encoding="utf-8")
        return path

    def test_reads_keys_a_person_would_write(self):
        pairs = dict(
            w.parse_env_file(
                "\n".join(
                    [
                        "# speaker labels",
                        "",
                        'export HUGGINGFACE_TOKEN="hf_secret"',
                        "ROOTED_WHISPER_MODEL = small ",
                        "NOT_A_PAIR",
                        "PADDED='quoted value'",
                    ]
                )
            )
        )
        self.assertEqual(pairs["HUGGINGFACE_TOKEN"], "hf_secret")
        self.assertEqual(pairs["ROOTED_WHISPER_MODEL"], "small")
        self.assertEqual(pairs["PADDED"], "quoted value")
        self.assertNotIn("NOT_A_PAIR", pairs)

    def test_a_value_may_contain_an_equals_sign(self):
        pairs = dict(w.parse_env_file("HUGGINGFACE_TOKEN=hf_a=b=c"))
        self.assertEqual(pairs["HUGGINGFACE_TOKEN"], "hf_a=b=c")

    def test_the_file_never_overrides_the_environment(self):
        """What you exported deliberately beats what a file happens to say."""
        os.environ["HUGGINGFACE_TOKEN"] = "from-the-shell"
        applied = w.load_env_file(self.write("HUGGINGFACE_TOKEN=from-the-file"))

        self.assertEqual(os.environ["HUGGINGFACE_TOKEN"], "from-the-shell")
        self.assertEqual(applied, [])

    def test_settings_the_app_shares_are_refused(self):
        """A worker reading a different database than the app would look like
        uploads vanishing, so the file isn't allowed to say."""
        os.environ.pop("ROOTED_DB", None)
        w.load_env_file(self.write("ROOTED_DB=/tmp/somewhere-else.db"))
        self.assertNotIn("ROOTED_DB", os.environ)

    def test_a_file_beside_the_database_is_found(self):
        """The packaged case: no shell, but the app data directory exists."""
        os.environ.pop("ROOTED_ENV", None)
        db = self.dir / "rooted.db"
        self.write("ROOTED_TEST_ENV_MARKER=found", name=".env")

        loaded = w.load_local_env(db)

        self.assertEqual(os.environ.get("ROOTED_TEST_ENV_MARKER"), "found")
        self.assertIn(self.dir / ".env", loaded)

    def test_a_missing_file_is_not_an_error(self):
        missing = self.dir / "nothing-here"
        os.environ["ROOTED_ENV"] = str(missing)
        # Says nothing about the repo's own .env, which may exist: only that a
        # path that isn't there is skipped rather than raising.
        self.assertNotIn(missing, w.load_local_env(self.dir / "rooted.db"))


if __name__ == "__main__":
    unittest.main()
