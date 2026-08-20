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

import base64
import json
import os
import sys
import zipfile
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional
from xml.etree import ElementTree

WORD_NS = "{http://schemas.openxmlformats.org/wordprocessingml/2006/main}"

# Below this, the review UI marks a span as doubtful and the job can be
# escalated to a stronger engine.
LOW_CONFIDENCE = 0.75

# Speaker labels. Overridable with ROOTED_DIARIZATION_MODEL — the licence is
# accepted per model, so which one a token can load is the user's choice.
DIARIZATION_MODEL = "pyannote/speaker-diarization-3.1"


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
# Cloud escalation — a second opinion on a page this machine can't read
# ---------------------------------------------------------------------------
#
# The only path in this app by which anything leaves the machine, and it is
# never taken on its own: a person asks for it, one job at a time.
#
# Two rules shape it. **The page keeps its layout** — escalation re-reads the
# line crops Vision already found, so every returned reading still has the box
# it came from and nothing gets reordered or joined. And **it re-reads every
# line, not just doubtful ones**: on-device OCR reports full confidence for
# readings that are plainly wrong, so confidence is a hint about where to look,
# never a filter on what to check.
#
# What is sent: the cropped line images. Not the note, not its metadata, not
# any other document. What comes back is still machine text — it goes to review
# like every other reading, and can never become a note unread.

CLOUD_OCR_MODEL = os.environ.get("ROOTED_CLOUD_OCR_MODEL", "claude-opus-5")

# How many line crops travel in one request.
CLOUD_OCR_BATCH = 8

CLOUD_OCR_PROMPT = """\
Transcribe each of these handwritten lines, cut from a page of study notes.

One image per line, labelled with its index. For each, return exactly what is
written — the same words, spelling, abbreviations, punctuation and casing.

Do not:
- correct spelling, grammar, or an abbreviation into its full form
- complete a fragment, or join it to another line
- reorder anything, or add words that are not written
- describe the image, or explain what you did

If a line is unreadable, set unreadable to true and leave text empty. An honest
blank is useful; a guess is not, because the person reviewing this cannot tell
the two apart.
"""

CLOUD_OCR_SCHEMA = {
    "type": "object",
    "properties": {
        "lines": {
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "index": {"type": "integer"},
                    "text": {"type": "string"},
                    "unreadable": {"type": "boolean"},
                },
                "required": ["index", "text", "unreadable"],
                "additionalProperties": False,
            },
        }
    },
    "required": ["lines"],
    "additionalProperties": False,
}


def cloud_ocr_available() -> bool:
    """Installed *and* keyed. Either half missing means the option isn't there,
    which is the honest thing to show before someone uploads a page."""
    try:
        import anthropic  # noqa: F401
    except ImportError:
        return False
    return bool(os.environ.get("ANTHROPIC_API_KEY"))


def crop_spans(page_path: Path, spans: list[Span], margin: float = 0.004) -> list[bytes]:
    """Cut each span's box out of the page as a PNG.

    Sending crops rather than the whole page is what keeps the boxes true: a
    reading comes back for a known line, so it can be put back exactly where
    that line is. A small margin allows for descenders and Vision's tight boxes.
    """
    if not vision_available():
        raise EngineUnavailable("cropping a page needs the Quartz bindings")
    import Quartz
    from Foundation import NSURL, NSMutableData

    url = NSURL.fileURLWithPath_(str(page_path))
    source = Quartz.CGImageSourceCreateWithURL(url, None)
    if source is None or Quartz.CGImageSourceGetCount(source) == 0:
        raise ExtractionError(f"could not read the page image: {page_path.name}")
    image = Quartz.CGImageSourceCreateImageAtIndex(source, 0, None)
    if image is None:
        raise ExtractionError(f"could not decode the page image: {page_path.name}")

    width = float(Quartz.CGImageGetWidth(image))
    height = float(Quartz.CGImageGetHeight(image))

    crops: list[bytes] = []
    for span in spans:
        if span.x is None or span.y is None or span.w is None or span.h is None:
            raise ExtractionError(
                "a span with no position can't be re-read — escalation only "
                "applies to scanned pages"
            )
        x = max(0.0, span.x - margin) * width
        y = max(0.0, span.y - margin) * height
        w = min(1.0, span.w + margin * 2) * width
        h = min(1.0, span.h + margin * 2) * height
        rect = Quartz.CGRectMake(x, y, max(1.0, w), max(1.0, h))
        crop = Quartz.CGImageCreateWithImageInRect(image, rect)
        if crop is None:
            raise ExtractionError(f"could not crop line {span.idx} from the page")

        data = NSMutableData.data()
        dest = Quartz.CGImageDestinationCreateWithData(data, "public.png", 1, None)
        Quartz.CGImageDestinationAddImage(dest, crop, None)
        if not Quartz.CGImageDestinationFinalize(dest):
            raise ExtractionError(f"could not encode line {span.idx} as an image")
        crops.append(bytes(data))
    return crops


def read_lines_in_cloud(crops: list[bytes], offset: int) -> dict[int, str]:
    """One request: line images in, a reading per line out.

    The returned indices must be exactly the ones asked for. Anything else —
    an invented line, a missing one, a renumbering — is rejected rather than
    guessed at, because a reading that lands on the wrong line is worse than no
    reading at all.
    """
    import anthropic

    client = anthropic.Anthropic()
    content: list[dict] = []
    for i, crop in enumerate(crops):
        content.append({"type": "text", "text": f"line {offset + i}:"})
        content.append(
            {
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": base64.standard_b64encode(crop).decode("ascii"),
                },
            }
        )
    content.append({"type": "text", "text": CLOUD_OCR_PROMPT})

    try:
        response = client.messages.create(
            model=CLOUD_OCR_MODEL,
            max_tokens=8000,
            messages=[{"role": "user", "content": content}],
            output_config={"format": {"type": "json_schema", "schema": CLOUD_OCR_SCHEMA}},
        )
    except anthropic.APIError as exc:
        raise ExtractionError(f"the cloud reader could not be reached: {exc}") from exc

    if response.stop_reason == "refusal":
        raise ExtractionError(
            "the cloud reader declined to transcribe this page; it stays as "
            "read on this machine"
        )

    text = next((b.text for b in response.content if b.type == "text"), None)
    if not text:
        raise ExtractionError("the cloud reader returned nothing for this page")
    return parse_cloud_readings(text, offset, len(crops))


def parse_cloud_readings(text: str, offset: int, count: int) -> dict[int, str]:
    """Check an answer line-by-line before any of it is believed.

    The indices must be exactly the ones sent: no extras, no duplicates, none
    missing. A reading that lands on the wrong line is worse than no reading,
    because the box underneath it says where those words were — and they
    weren't.
    """
    try:
        payload = json.loads(text)
    except json.JSONDecodeError as exc:
        raise ExtractionError(f"the cloud reader's answer was unreadable: {exc}") from exc

    expected = set(range(offset, offset + count))
    readings: dict[int, str] = {}
    for line in payload.get("lines", []):
        index = line.get("index")
        if index not in expected:
            raise ExtractionError(
                f"the cloud reader answered for line {index}, which wasn't sent"
            )
        if index in readings:
            raise ExtractionError(
                f"the cloud reader answered twice for line {index}"
            )
        readings[index] = "" if line.get("unreadable") else (line.get("text") or "")
    if set(readings) != expected:
        missing = sorted(expected - set(readings))
        raise ExtractionError(f"the cloud reader skipped line(s) {missing}")
    return readings


def escalate_extraction(extraction: Extraction) -> Extraction:
    """Re-read an on-device scan in the cloud, keeping every box.

    Spans whose reading comes back empty keep the on-device text and are marked
    doubtful: a line neither reader could manage is exactly what review is for.
    """
    if not cloud_ocr_available():
        raise EngineUnavailable(
            "reading a page in the cloud needs the Anthropic SDK and a key — "
            "`sidecar/.venv/bin/pip install anthropic`, then set "
            "ANTHROPIC_API_KEY in .env"
        )
    if not extraction.pages:
        raise ExtractionError("only a scanned page can be re-read in the cloud")

    by_page = {page.page_no: page for page in extraction.pages}
    read_any = False
    for page_no, page in sorted(by_page.items()):
        spans = [s for s in extraction.spans if (s.page_no or 1) == page_no]
        if not spans:
            continue
        crops = crop_spans(Path(page.image_path), spans)
        for start in range(0, len(spans), CLOUD_OCR_BATCH):
            batch = spans[start:start + CLOUD_OCR_BATCH]
            readings = read_lines_in_cloud(crops[start:start + CLOUD_OCR_BATCH],
                                           batch[0].idx)
            for span in batch:
                reading = readings.get(span.idx, "")
                if reading.strip():
                    span.text = reading
                    # The cloud reader reports no per-line confidence, and a
                    # made-up number would be read as one. Doubt belongs to the
                    # reviewer here.
                    span.confidence = None
                    read_any = True
                else:
                    # Unreadable there too: keep what this machine read, and
                    # make sure review stops on it.
                    span.confidence = 0.0

    if not read_any:
        raise ExtractionError(
            "the cloud reader could not read any line on this page either"
        )
    extraction.text = "\n".join(s.text for s in extraction.spans)
    extraction.engine = f"{extraction.engine}+{CLOUD_OCR_MODEL}"
    # No single number describes "some lines re-read, some not"; the spans carry
    # what is known, and a scan is never auto-verified anyway.
    extraction.confidence = min(
        [s.confidence for s in extraction.spans if s.confidence is not None] or [1.0]
    )
    return extraction


# ---------------------------------------------------------------------------
# Audio — transcription and (optionally) who spoke
# ---------------------------------------------------------------------------

def asr_available() -> bool:
    try:
        import faster_whisper  # noqa: F401
    except ImportError:
        return False
    return True


def pyannote_installed() -> bool:
    try:
        import pyannote.audio  # noqa: F401
    except ImportError:
        return False
    return True


def diarization_available() -> bool:
    """Speaker labels need both the model and a token that accepted its licence."""
    return pyannote_installed() and bool(os.environ.get("HUGGINGFACE_TOKEN"))


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
    try:
        model = WhisperModel(size, device="auto", compute_type="int8")
    except Exception as exc:
        # First use of a model downloads it; an unknown name or no network
        # lands here, and neither is the recording's fault.
        raise ExtractionError(
            f"could not load the '{size}' Whisper model: {exc}"
        ) from exc

    try:
        # `transcribe` returns a generator; decoding errors surface as it runs,
        # so drain it here where they can still be named.
        segments, _info = model.transcribe(str(path), word_timestamps=False)
        segments = list(segments)
    except Exception as exc:
        raise ExtractionError(
            f"could not read {path.name} as audio — the file may be truncated, "
            f"or in a codec this build can't decode ({type(exc).__name__})"
        ) from exc

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

    Never fails the job. Speaker labels are an addition to a transcript that
    already stands on its own; losing an hour of correct transcription because
    a licence wasn't accepted would be the wrong trade. The reason goes to the
    worker log instead.
    """
    if not diarization_available():
        return False
    try:
        return _label_speakers(path, spans)
    except ExtractionError as exc:
        print(f"[engines] no speaker labels: {exc}", file=sys.stderr, flush=True)
        return False
    except Exception as exc:
        print(
            f"[engines] no speaker labels: {type(exc).__name__}: {exc}",
            file=sys.stderr,
            flush=True,
        )
        return False


def decode_for_diarization(path: Path) -> dict:
    """Hand pyannote audio it doesn't have to open itself.

    Given a path, pyannote 4 decodes through torchcodec, which dynamically
    loads FFmpeg's shared libraries and only searches the interpreter's own
    rpath — so a perfectly good Homebrew ffmpeg is invisible to it and speaker
    labels die on a `dlopen`. Decoding here instead uses the PyAV that ships
    with faster-whisper, which is already how the transcript itself was read,
    and drops the native dependency entirely.

    16 kHz mono, which is what the pipeline resamples to anyway.
    """
    import numpy
    import torch
    from faster_whisper.audio import decode_audio  # type: ignore

    samples = decode_audio(str(path), sampling_rate=16000)
    if not isinstance(samples, numpy.ndarray):
        samples = numpy.asarray(samples, dtype="float32")
    # pyannote wants (channel, sample).
    waveform = torch.from_numpy(numpy.ascontiguousarray(samples)).float().unsqueeze(0)
    return {"waveform": waveform, "sample_rate": 16000}


def diarization_turns(result) -> list[tuple[float, float, str]]:
    """Who spoke when, as plain numbers, whichever pyannote produced them.

    pyannote 3 returns an `Annotation`; 4 returns a `DiarizeOutput` carrying
    two of them. Prefer its *exclusive* one: it drops overlapping speech, which
    is what you want when each transcript segment gets a single label — an
    interjection shouldn't relabel the sentence it lands in.
    """
    annotation = getattr(result, "exclusive_speaker_diarization", None)
    if annotation is None:
        annotation = getattr(result, "speaker_diarization", result)
    return [
        (float(turn.start), float(turn.end), str(speaker))
        for turn, _, speaker in annotation.itertracks(yield_label=True)
    ]


def _label_speakers(path: Path, spans: list[Span]) -> bool:
    import inspect

    from pyannote.audio import Pipeline  # type: ignore

    model = os.environ.get("ROOTED_DIARIZATION_MODEL", DIARIZATION_MODEL)
    token = os.environ["HUGGINGFACE_TOKEN"]
    # pyannote renamed the argument in 4.0; support both rather than pinning a
    # version, since which one is installed is the user's business.
    keyword = (
        "token"
        if "token" in inspect.signature(Pipeline.from_pretrained).parameters
        else "use_auth_token"
    )
    try:
        pipeline = Pipeline.from_pretrained(model, **{keyword: token})
    except Exception as exc:
        raise ExtractionError(
            f"could not load the '{model}' diarization pipeline: {exc}. "
            "The token's account has to accept the conditions for this model "
            "and for every model it pulls in — which is more than the config "
            "lists: pyannote 4 clusters with PLDA weights from "
            "speaker-diarization-community-1 whichever checkpoint you name. "
            "The message above names the repo that actually stopped it."
        ) from exc
    if pipeline is None:
        # pyannote returns None rather than raising when the token can't see
        # the model — usually an unaccepted licence.
        raise ExtractionError(
            f"'{model}' could not be loaded with this HUGGINGFACE_TOKEN — "
            "accept its licence on huggingface.co, and the segmentation "
            "model's, then try again."
        )
    turns = diarization_turns(pipeline(decode_for_diarization(path)))
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

def describe() -> list[dict]:
    """Which engines this machine can actually run — surfaced in the UI so a
    missing model is visible before a file is uploaded, not after.

    A list, in the order a person meets these formats, so the UI can render it
    without deciding anything. `note` says what to do about an engine that
    isn't there; nothing here installs or downloads.
    """
    try:
        import pypdf  # noqa: F401
        pdf_ok = True
    except ImportError:
        pdf_ok = False

    if diarization_available():
        speakers = "speaker labels on each segment"
    elif pyannote_installed():
        speakers = (
            "installed, but needs HUGGINGFACE_TOKEN set, with the licence "
            "accepted at hf.co/pyannote/speaker-diarization-3.1"
        )
    else:
        speakers = "pip install \"pyannote.audio>=3.1\" (also needs HUGGINGFACE_TOKEN)"

    return [
        {
            "key": "typed",
            "label": "Typed documents",
            "available": True,
            "engine": "stdlib",
            "note": "txt, md, docx",
        },
        {
            "key": "pdf",
            "label": "PDF text layer",
            "available": pdf_ok,
            "engine": "pypdf",
            "note": "text layer; a scanned PDF falls through to OCR"
            if pdf_ok
            else "pip install pypdf",
        },
        {
            "key": "ocr",
            "label": "Scans and handwriting",
            "available": vision_available(),
            "engine": "apple-vision",
            "note": "on-device; per-line boxes and confidence"
            if vision_available()
            else "needs the macOS Vision bindings (pyobjc-framework-Vision)",
        },
        {
            "key": "asr",
            "label": "Recordings",
            "available": asr_available(),
            "engine": "faster-whisper",
            "note": "timestamped segments"
            if asr_available()
            else "pip install faster-whisper",
        },
        {
            "key": "cloud",
            "label": "Cloud second opinion",
            "available": cloud_ocr_available(),
            "engine": CLOUD_OCR_MODEL,
            "note": "re-reads a scan's lines off the machine, when you ask"
            if cloud_ocr_available()
            else "pip install anthropic, then set ANTHROPIC_API_KEY in .env",
        },
        {
            "key": "diarization",
            "label": "Who spoke",
            "available": diarization_available(),
            "engine": "pyannote-3.1",
            "note": speakers,
        },
    ]
