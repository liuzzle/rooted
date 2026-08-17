#!/usr/bin/env python3
"""
Reading engines: how bytes on disk become text.

Every engine returns the same shape — an :class:`Extraction` of positioned
:class:`Span`s — so the worker, the review UI and the note model don't care
which engine produced them.

The rule all of these obey: **an engine transcribes, it never composes.** It
reports what it read and how sure it was, with a position. It does not reorder,
join, summarise, complete a fragment, or resolve an arrow. A page of handwritten
notes has meaning in its layout, and inventing that layout is indistinguishable
from inventing content — so spans keep their coordinates and a human decides
what they mean.

Engines declare themselves unavailable rather than degrading silently: a missing
dependency is an actionable error on the job, never a quietly empty result.
"""
from __future__ import annotations

import os
import zipfile
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional
from xml.etree import ElementTree

WORD_NS = "{http://schemas.openxmlformats.org/wordprocessingml/2006/main}"

# Below this, the review UI marks a span as doubtful and the job can be
# escalated to a stronger engine.
LOW_CONFIDENCE = 0.75


class ExtractionError(Exception):
    """The document can't be read. The job goes to ERROR with this message."""


class EngineUnavailable(ExtractionError):
    """The engine isn't installed. Same handling, clearer cause."""


@dataclass
class Span:
    """A piece of recognised text and where it came from.

    Images use the box (`x`, `y`, `w`, `h`), normalised 0..1 from the top-left.
    Audio uses `start_s`/`end_s` and `speaker`. Typed documents use neither.
    """

    idx: int
    text: str
    confidence: Optional[float] = None
    page_no: Optional[int] = None
    x: Optional[float] = None
    y: Optional[float] = None
    w: Optional[float] = None
    h: Optional[float] = None
    start_s: Optional[float] = None
    end_s: Optional[float] = None
    speaker: Optional[str] = None


@dataclass
class PageImage:
    """A page as the reviewer will see it, with the size its boxes refer to."""

    page_no: int
    image_path: str
    width: int
    height: int


@dataclass
class Extraction:
    text: str
    engine: str
    confidence: float
    spans: list[Span] = field(default_factory=list)
    pages: list[PageImage] = field(default_factory=list)


# ---------------------------------------------------------------------------
# Typed documents
# ---------------------------------------------------------------------------

def extract_plain_text(path: Path) -> Extraction:
    raw = path.read_bytes()
    try:
        return Extraction(raw.decode("utf-8"), "text/utf-8", 1.0)
    except UnicodeDecodeError:
        # latin-1 always decodes, so accented characters may be wrong: flag it.
        return Extraction(raw.decode("latin-1"), "text/latin-1", 0.6)


def extract_docx(path: Path) -> Extraction:
    """Paragraph text from a .docx using only the standard library."""
    try:
        with zipfile.ZipFile(path) as zf:
            xml = zf.read("word/document.xml")
    except (zipfile.BadZipFile, KeyError) as exc:
        raise ExtractionError(f"not a readable .docx: {exc}") from exc

    root = ElementTree.fromstring(xml)
    paragraphs = []
    for para in root.iter(f"{WORD_NS}p"):
        runs = [node.text or "" for node in para.iter(f"{WORD_NS}t")]
        if para.find(f".//{WORD_NS}tab") is not None and runs:
            runs = ["\t".join(runs)]
        paragraphs.append("".join(runs))

    text = "\n".join(paragraphs).strip()
    if not text:
        raise ExtractionError("the document has no text (an image-only file?)")
    return Extraction(text, "docx/xml", 1.0)


def extract_pdf_text_layer(path: Path) -> Extraction:
    """Text-layer extraction. Raises if there is no text layer to read."""
    try:
        from pypdf import PdfReader  # type: ignore
    except ImportError as exc:
        raise EngineUnavailable(
            "reading PDFs needs pypdf — `pip install -r sidecar/requirements.txt`"
        ) from exc

    reader = PdfReader(str(path))
    pages = [(page.extract_text() or "") for page in reader.pages]
    text = "\n\n".join(p.strip() for p in pages if p.strip()).strip()
    if not text:
        raise ExtractionError("this PDF has no text layer — it looks scanned")
    per_page = len(text) / max(1, len(pages))
    confidence = 1.0 if per_page >= 200 else max(0.3, per_page / 200)
    return Extraction(text, "pdf/text-layer", confidence)


def confidence_of_decode(text: str) -> float:
    """Replacement characters mean bytes were lost in decoding."""
    if not text:
        return 0.0
    return max(0.0, 1.0 - (text.count("�") / max(1, len(text))) * 10)


# ---------------------------------------------------------------------------
# Handwriting / scans — Apple Vision, on device
# ---------------------------------------------------------------------------

def vision_available() -> bool:
    try:
        import Vision  # noqa: F401
        import Quartz  # noqa: F401
    except ImportError:
        return False
    return True


def ocr_image(path: Path, page_no: int = 1) -> Extraction:
    """Recognise text in an image with macOS Vision.

    Returns one span per recognised line, each with the box Vision found it in
    and its own confidence. Lines are ordered top-to-bottom, then left-to-right
    — a *guess* at reading order, which is why the boxes are kept: for notes
    with margins and arrows, the page itself is the real record, and the
    reviewer can see where every line came from.
    """
    if not vision_available():
        raise EngineUnavailable(
            "handwriting OCR needs the Vision bindings — "
            "`pip install -r sidecar/requirements.txt`"
        )
    import Quartz
    import Vision
    from Foundation import NSURL

    url = NSURL.fileURLWithPath_(str(path))
    source = Quartz.CGImageSourceCreateWithURL(url, None)
    if source is None or Quartz.CGImageSourceGetCount(source) == 0:
        raise ExtractionError(f"could not read the image: {path.name}")
    image = Quartz.CGImageSourceCreateImageAtIndex(source, 0, None)
    if image is None:
        raise ExtractionError(f"could not decode the image: {path.name}")

    width = Quartz.CGImageGetWidth(image)
    height = Quartz.CGImageGetHeight(image)

    request = Vision.VNRecognizeTextRequest.alloc().init()
    request.setRecognitionLevel_(Vision.VNRequestTextRecognitionLevelAccurate)
    # Handwriting benefits from the language model; it corrects nothing that
    # wasn't recognised, it only picks between candidate readings.
    request.setUsesLanguageCorrection_(True)
    handler = Vision.VNImageRequestHandler.alloc().initWithCGImage_options_(image, None)
    ok, error = handler.performRequests_error_([request], None)
    if not ok:
        raise ExtractionError(f"Vision failed on {path.name}: {error}")

    spans: list[Span] = []
    for observation in request.results() or []:
        candidates = observation.topCandidates_(1)
        if not candidates:
            continue
        best = candidates[0]
        text = best.string()
        if not text or not text.strip():
            continue
        box = observation.boundingBox()
        # Vision's origin is bottom-left; the UI draws from the top-left.
        x = float(box.origin.x)
        w = float(box.size.width)
        h = float(box.size.height)
        y = 1.0 - float(box.origin.y) - h
        spans.append(
            Span(
                idx=0,  # assigned below, once sorted
                text=text,
                confidence=float(best.confidence()),
                page_no=page_no,
                x=round(x, 5),
                y=round(y, 5),
                w=round(w, 5),
                h=round(h, 5),
            )
        )

    if not spans:
        raise ExtractionError(
            "no text found on this page — if it is handwriting, it may be too "
            "faint or too slanted for on-device OCR"
        )

    # Top-to-bottom, then left-to-right. Rows within ~2% of each other count as
    # the same line, so side-by-side notes don't interleave.
    spans.sort(key=lambda s: (round((s.y or 0) / 0.02), s.x or 0))
    for i, span in enumerate(spans):
        span.idx = i

    text = "\n".join(s.text for s in spans)
    confidence = min((s.confidence or 0.0) for s in spans)
    pages = [PageImage(page_no, str(path), int(width), int(height))]
    return Extraction(text, "vision/ocr", confidence, spans, pages)


def render_pdf_pages(path: Path, out_dir: Path, dpi: int = 200) -> list[Path]:
    """Rasterise a scanned PDF so its pages can be OCR'd and shown in review."""
    if not vision_available():
        raise EngineUnavailable("rendering PDF pages needs the Quartz bindings")
    import Quartz
    from Foundation import NSURL

    url = NSURL.fileURLWithPath_(str(path))
    document = Quartz.CGPDFDocumentCreateWithURL(url)
    if document is None:
        raise ExtractionError(f"could not open the PDF: {path.name}")
    count = Quartz.CGPDFDocumentGetNumberOfPages(document)
    scale = dpi / 72.0

    out_dir.mkdir(parents=True, exist_ok=True)
    written: list[Path] = []
    for page_no in range(1, count + 1):
        page = Quartz.CGPDFDocumentGetPage(document, page_no)
        rect = Quartz.CGPDFPageGetBoxRect(page, Quartz.kCGPDFMediaBox)
        width = int(rect.size.width * scale)
        height = int(rect.size.height * scale)
        color_space = Quartz.CGColorSpaceCreateDeviceRGB()
        context = Quartz.CGBitmapContextCreate(
            None, width, height, 8, 0, color_space,
            Quartz.kCGImageAlphaPremultipliedFirst | Quartz.kCGBitmapByteOrder32Host,
        )
        # White paper behind the page, or transparent areas OCR as noise.
        Quartz.CGContextSetRGBFillColor(context, 1.0, 1.0, 1.0, 1.0)
        Quartz.CGContextFillRect(context, Quartz.CGRectMake(0, 0, width, height))
        Quartz.CGContextScaleCTM(context, scale, scale)
        Quartz.CGContextDrawPDFPage(context, page)

        image = Quartz.CGBitmapContextCreateImage(context)
        out_path = out_dir / f"{path.stem}-p{page_no}.png"
        dest_url = NSURL.fileURLWithPath_(str(out_path))
        dest = Quartz.CGImageDestinationCreateWithURL(dest_url, "public.png", 1, None)
        Quartz.CGImageDestinationAddImage(dest, image, None)
        Quartz.CGImageDestinationFinalize(dest)
        written.append(out_path)
    return written


def ocr_pdf(path: Path, out_dir: Path) -> Extraction:
    """OCR a scanned PDF, page by page, keeping each page's spans on its page."""
    pages = render_pdf_pages(path, out_dir)
    spans: list[Span] = []
    images: list[PageImage] = []
    confidences: list[float] = []

    for page_no, image_path in enumerate(pages, start=1):
        try:
            page_extraction = ocr_image(image_path, page_no=page_no)
        except ExtractionError:
            continue  # a blank page in a scan is normal
        images.extend(page_extraction.pages)
        confidences.append(page_extraction.confidence)
        for span in page_extraction.spans:
            span.idx = len(spans)
            spans.append(span)

    if not spans:
        raise ExtractionError("no text found anywhere in this scanned PDF")
    text = "\n".join(s.text for s in spans)
    return Extraction("\n".join(t for t in [text] if t), "vision/ocr-pdf",
                      min(confidences), spans, images)


# ---------------------------------------------------------------------------
# Audio — transcription and (optionally) who spoke
# ---------------------------------------------------------------------------

def asr_available() -> bool:
    try:
        import faster_whisper  # noqa: F401
    except ImportError:
        return False
    return True


def diarization_available() -> bool:
    try:
        import pyannote.audio  # noqa: F401
    except ImportError:
        return False
    return bool(os.environ.get("HUGGINGFACE_TOKEN"))


def transcribe_audio(path: Path, model_size: Optional[str] = None) -> Extraction:
    """Transcribe a recording into timestamped segments.

    Each segment becomes a span carrying its own start/end and the model's
    confidence, so a doubtful stretch can be found and corrected against the
    audio rather than trusted wholesale.
    """
    if not asr_available():
        raise EngineUnavailable(
            "audio transcription needs faster-whisper — "
            "`sidecar/.venv/bin/pip install faster-whisper`"
        )
    from faster_whisper import WhisperModel  # type: ignore

    size = model_size or os.environ.get("ROOTED_WHISPER_MODEL", "base")
    model = WhisperModel(size, device="auto", compute_type="int8")
    segments, _info = model.transcribe(str(path), word_timestamps=False)

    spans: list[Span] = []
    for idx, segment in enumerate(segments):
        text = (segment.text or "").strip()
        if not text:
            continue
        # avg_logprob is a log probability; map it to a rough 0..1 for display.
        confidence = None
        if getattr(segment, "avg_logprob", None) is not None:
            confidence = max(0.0, min(1.0, 1.0 + segment.avg_logprob / 5.0))
        spans.append(
            Span(
                idx=idx,
                text=text,
                confidence=confidence,
                start_s=float(segment.start),
                end_s=float(segment.end),
            )
        )

    if not spans:
        raise ExtractionError("no speech found in this recording")

    speakers = label_speakers(path, spans)
    text = "\n".join(
        f"[{s.speaker}] {s.text}" if s.speaker else s.text for s in spans
    )
    confidence = min((s.confidence or 1.0) for s in spans)
    engine = f"faster-whisper/{size}" + ("+pyannote" if speakers else "")
    return Extraction(text, engine, confidence, spans)


def label_speakers(path: Path, spans: list[Span]) -> bool:
    """Attach speaker labels to segments, if diarization is available.

    Returns whether anything was labelled. Without it the transcript is still
    correct — it just doesn't say who was talking, which is better than
    guessing at speaker changes.
    """
    if not diarization_available():
        return False
    from pyannote.audio import Pipeline  # type: ignore

    pipeline = Pipeline.from_pretrained(
        "pyannote/speaker-diarization-3.1",
        use_auth_token=os.environ["HUGGINGFACE_TOKEN"],
    )
    diarization = pipeline(str(path))
    turns = [
        (turn.start, turn.end, speaker)
        for turn, _, speaker in diarization.itertracks(yield_label=True)
    ]
    if not turns:
        return False

    for span in spans:
        if span.start_s is None:
            continue
        middle = (span.start_s + (span.end_s or span.start_s)) / 2
        # The turn covering the middle of the segment wins; ties go to the
        # longest overlap.
        best, best_overlap = None, 0.0
        for start, end, speaker in turns:
            overlap = min(end, span.end_s or middle) - max(start, span.start_s)
            if start <= middle <= end and overlap > best_overlap:
                best, best_overlap = speaker, overlap
        if best:
            span.speaker = best
    return any(s.speaker for s in spans)


# ---------------------------------------------------------------------------
# What's installed
# ---------------------------------------------------------------------------

def describe() -> dict[str, dict]:
    """Which engines this machine can actually run — surfaced in the UI so a
    missing model is visible before a file is uploaded, not after."""
    try:
        import pypdf  # noqa: F401
        pdf_ok = True
    except ImportError:
        pdf_ok = False

    return {
        "typed": {"available": True, "engine": "stdlib", "note": "txt, md, docx"},
        "pdf": {
            "available": pdf_ok,
            "engine": "pypdf",
            "note": "text layer; scanned PDFs fall through to OCR",
        },
        "ocr": {
            "available": vision_available(),
            "engine": "apple-vision",
            "note": "on-device; per-line boxes and confidence",
        },
        "asr": {
            "available": asr_available(),
            "engine": "faster-whisper",
            "note": "timestamped transcription",
        },
        "diarization": {
            "available": diarization_available(),
            "engine": "pyannote-3.1",
            "note": "speaker labels; needs HUGGINGFACE_TOKEN",
        },
    }
