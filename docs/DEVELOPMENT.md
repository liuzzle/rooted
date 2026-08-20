# Rooted — Development

Local-first Bible study app. Tauri v2 (Rust) shell + React/TypeScript UI + SQLite,
with a Python sidecar for AI ingestion (added in later phases).

See the full build plan in [`docs/PLAN.md`](./PLAN.md).

## Prerequisites

- **Node** 20.19+ (or 22.12+) and npm
- **Rust** stable (`rustup`) — Tauri backend
- **Python** 3.10+ — Bible import and the ingestion worker

```bash
python3 -m venv sidecar/.venv
sidecar/.venv/bin/python -m pip install -r sidecar/requirements.txt
```

The app finds `sidecar/.venv` automatically (override with `ROOTED_PYTHON`).

Optional local settings — currently a Hugging Face token for speaker labels and
the Whisper model size — live in a gitignored `.env`:

```bash
cp .env.example .env
```

The worker reads it at startup from `$ROOTED_ENV`, then the repo, then beside
`rooted.db` in the app data directory — that last one so an installed app,
which gets launchd's environment rather than your shell's, can still find a
token. Anything already exported wins; the file never overrides it. See
`.env.example` for the keys, including the three it deliberately refuses.

## First-time setup

```bash
npm install
npm run tauri dev
```

On first run the app has no Bible: open **Translations…** and download one.
Packs come from the registry in `src-tauri/packs/registry.json` (freely
distributable texts only) and are imported straight into the canonical model.

`scripts/import_bible.py` does the same thing from the command line — useful for
seeding a database without launching the app:

```bash
python3 scripts/import_bible.py                      # WEB (default)
python3 scripts/import_bible.py --translation kjv
```

Both importers read the same registry and tokenize identically; a test asserts
it, because a mismatch would shift token indices and move every word anchor.
Importing KJV both ways produces byte-identical verse and token rows.

Text is fetched from the getbible API over HTTPS but is **not** checksum- or
signature-verified — see the Phase 2 deviation note in [`PLAN.md`](./PLAN.md).

## Run

```bash
npm run tauri dev      # launches the desktop app (Rust build + Vite UI)
npm run build          # typecheck + build the frontend only
cargo test --manifest-path src-tauri/Cargo.toml              # data-layer tests
cargo test --manifest-path src-tauri/Cargo.toml -- --ignored # + network tests
npm test               # frontend logic tests (vitest)
python3 -m unittest discover -s sidecar   # ingestion worker tests
ROOTED_TEST_ASR=1 python3 -m unittest discover -s sidecar   # + real transcription
```

The end-to-end ingestion test (Rust queues a job → the real Python worker
processes it → a note appears) is in the `--ignored` set because it shells out
to `python3`. The transcription test is behind `ROOTED_TEST_ASR` for the same
reason in reverse: its first run downloads a Whisper model. OCR tests skip
themselves when the Vision bindings aren't installed.

The database path can be overridden for both the app and the import script via
the `ROOTED_DB` environment variable, or the import script's `--db` flag.

## Layout

| Path | Purpose |
|------|---------|
| `src/App.tsx` | Shell: Read · Notes · Dashboard views, active translation, pack modal. |
| `src/features/` | `reader/` (reading pane), `notes/` (note panel + chapter rail), `library/` (all notes), `dashboard/`, `translations/` (pack manager), `ingest/` (upload, pipeline status, review). |
| `src/lib/api.ts` | Typed Tauri commands. `src/lib/reference.ts` — scripture reference parsing. |
| `src-tauri/src/db.rs` | SQLite access + query commands (with unit tests). |
| `src-tauri/src/packs.rs` | Pack registry, download, tokenizer, import. |
| `src-tauri/src/lib.rs` | Tauri command registration + app setup. |
| `src-tauri/src/ingest.rs` | Jobs, documents, pages, spans, verification. |
| `src-tauri/src/sidecar.rs` | Worker process lifecycle + what it reports it can read. |
| `sidecar/worker.py` | Job state machine. `sidecar/engines.py` — the reading engines. |
| `src-tauri/migrations/` | Schema, applied in filename order on every start (each migration is idempotent). |
| `src-tauri/packs/registry.json` | Downloadable translations. |
| `scripts/import_bible.py` | Command-line equivalent of the in-app pack import. |

## Data model notes

- Verses use **OSIS BCV ids** (`Gen.1.1`) as a translation-independent key.
- Word anchoring uses `(translation_id, verse_id, token_idx)` with a stored
  `surface` snapshot and char offsets, so word-level notes/highlights survive
  re-imports and degrade gracefully across translations (Phase 2).
- **Verse** notes and highlights store no `translation_id` at all, so they show
  in every translation. **Word** notes and highlights are scoped to the
  translation they were made in.
- One highlight per anchor: setting a colour replaces the previous one, and
  clicking the active colour (or the slashed swatch) removes it.

## Switching translations (Tier-1 anchoring)

- Verse notes and highlights follow you into every translation.
- A word note is never re-pointed at a word it wasn't written on. Read another
  translation and it appears on the **verse**, labelled *“originally on the word
  ‘X’ in WEB”*, with a hollow indicator dot. Switch back and it returns to its
  word. Strong's-based alignment (Tier 2) comes in Phase 7.
- Word *highlights* are simply not painted outside their own translation.
- Removing a pack deletes its verses and tokens but keeps its `translations`
  row, so notes written against it keep resolving and a reinstall lands on the
  same `translation_id`.

## Ingestion (Phase 3)

The app starts `sidecar/worker.py` as a child process and they share the
database — no IPC. That's why the schema runs in **WAL** mode: two processes
write to the same file.

```
UPLOADED → EXTRACTING → NEEDS_REVIEW → VERIFIED → DONE
                 ↘ ERROR (retryable)
```

Rust owns what a person does (upload, review, verify, retry); the worker owns
the machine stages. Three properties hold the pipeline together:

- **Resumable.** Jobs are claimed with a lease (`claimed_by`/`claimed_at`). Kill
  the app mid-extraction and the next worker reclaims the job once the lease
  goes stale; after `MAX_ATTEMPTS` interrupted runs it stops instead of looping.
- **Idempotent.** One `extractions` row per job, upserted, and publishing twice
  updates the same note rather than making a second one.
- **Nothing becomes a note unverified.** `save_verification` is the only route
  to `VERIFIED`, and the worker refuses to publish a job whose extraction isn't
  marked verified. Tests assert both halves.

Formats: `.txt`/`.md` (decoded — a non-UTF-8 file drops confidence so a human
looks), `.docx` (paragraph text via stdlib `zipfile` + XML, no dependency),
`.pdf` (text layer via `pypdf`; without one it falls through to OCR), images
(`.jpg`/`.png`/`.heic`/`.tiff`), and audio (`.mp3`/`.m4a`/`.wav`/…).

Engines live in `sidecar/engines.py` and all return the same shape — an
`Extraction` of positioned `Span`s — so the worker, the review UI and the note
model don't care which one ran. A missing engine is an actionable error on the
job, never a silently empty result.

## Scans and recordings (Phase 4)

**A page is not a paragraph.** Handwritten notes carry meaning in their layout —
arrows, margin notes, bullets nested by indentation, fragments. Flattening that
into prose invents reading order and connections that were never written, and
once flattened you can't tell what was read from what was inferred. So a scan is
stored as **the page image plus spans with their positions on it** (`pages`,
`spans`), and review draws each span over the scan and corrects it in place.
Audio uses the same table with time instead of space: `start_s`/`end_s` and a
speaker label instead of a box.

**OCR is macOS Vision, on device.** Nothing leaves the machine, there's no model
download, and it returns per-line boxes and confidence. Its weaknesses are worth
knowing: it is fair on cursive, better on printing, and it **reports full
confidence for readings that are plainly wrong** ("cf." → "of."). Two
consequences are deliberate:

- A scan or recording is **never** auto-verified, whatever `--auto-verify` says.
- The review UI marks low-confidence spans but tells you a confident reading can
  still be wrong — the marking is a hint about where to look, not a filter on
  what needs reading.

Vision does not reconstruct arrows or hierarchy, and nothing here tries to: that
is interpretation, and it belongs to you during review, not to an engine.

**Audio is transcribed on device too.** `faster-whisper` turns a recording
into timestamped segments — each one a span with its own start, end and
confidence — so a doubtful stretch can be found in the audio and checked rather
than trusted. The model is chosen with `ROOTED_WHISPER_MODEL` (default `base`)
and is downloaded on first use.

**Speaker labels are opt-in.** They need `pyannote.audio` installed *and* a
`HUGGINGFACE_TOKEN` whose account has accepted the conditions for **three**
repos: `pyannote/speaker-diarization-3.1`, `pyannote/segmentation-3.0`, and
`pyannote/speaker-diarization-community-1`. The third isn't listed in 3.1's
config — pyannote 4 rebuilt the pipeline around PLDA clustering and loads those
weights from community-1 whichever checkpoint you name, so accepting only what
the config mentions gets you a 403 on a file you never asked for. Pick a
different pipeline with `ROOTED_DIARIZATION_MODEL`.

Three things about `pyannote.audio` 4.0 that the adapter absorbs, so neither
version needs pinning:

- `from_pretrained` renamed `use_auth_token` to `token`; the adapter uses
  whichever the installed signature takes.
- It decodes audio through **torchcodec**, which `dlopen`s FFmpeg's shared
  libraries and searches only the interpreter's own rpath — a working Homebrew
  ffmpeg is invisible to it, and speaker labels die on a missing `libavutil`.
  So the file is decoded here instead, with the PyAV that ships with
  faster-whisper (already how the transcript was read), and pyannote is handed
  a waveform at 16 kHz mono. The native dependency drops out entirely.
- It returns a `DiarizeOutput` holding two annotations rather than one
  `Annotation`. The adapter prefers its **exclusive** one, which drops
  overlapping speech: each transcript segment gets a single label, so an
  interjection can't relabel the sentence it lands in.

Without all of that, the transcript says it doesn't know who was talking rather
than guessing at speaker changes — and it says so *without failing the job*.
Speaker labels are an addition to a transcript that already stands on its own;
losing an hour of correct transcription to an unaccepted licence would be the
wrong trade, so the reason goes to the worker log and the transcript proceeds.

**A page this machine can't read can be re-read in the cloud.** It is the only
path by which anything here leaves the computer, and it is deliberately narrow:

- **You ask, per page.** An explicit action on one job, which the app confirms
  by naming what travels. There is no setting that turns it on for everything,
  and no "don't ask again".
- **Only cropped lines travel.** Vision has already found the lines and their
  boxes; escalation sends those crops, so what comes back can be put straight
  back on the box it came from. The note, its date and speaker, and every other
  document stay here. `crop_spans` is tested by re-reading each crop on-device
  and checking it says what its span says.
- **Every line is re-read, not just doubtful ones** — Vision reports full
  confidence for plain misreadings, so confidence can't be used as a filter.
- **The answer is checked before it is believed.** Indices must be exactly the
  ones sent — no extras, duplicates or omissions — or the job errors rather
  than risk a reading landing on the wrong line. A line the cloud can't read
  either keeps the on-device text and is marked doubtful.
- **The decision is used once.** The worker clears `jobs.escalate` the moment it
  acts on it, so a later retry re-reads on this machine only.

Needs `pip install anthropic` and `ANTHROPIC_API_KEY`; without both, the action
isn't offered. Nothing about this is auto-verified: a cloud reading is still
machine text and goes through review like any other.

**The app says what it can read before you upload.** Each tick the worker
writes `engines.describe()` to `settings.worker_engines`, and the Ingest view
lists them — a missing engine appears there, with the command that installs it,
instead of as a failed job afterwards. The worker probes once per process, so
installing something takes effect when the app restarts.

Auto-verify for perfect *typed* extractions exists behind `--auto-verify` and is
off by default.

Worker overrides: `ROOTED_WORKER` (script path), `ROOTED_PYTHON` (interpreter).
If neither resolves, the app still runs — uploads queue and the UI says the
worker is down.

## Notes without a reference

A note may have **no anchor at all** — a general study note. `notes` and
`note_anchors` are separate tables, so this needs no migration, but it means:

- `list_all_notes` uses a LEFT JOIN and returns `anchor: null` for them; a book
  filter necessarily excludes them.
- `set_note_anchor` attaches, moves or detaches a reference afterwards. Typed
  references always produce a **verse** anchor — a word anchor is only ever made
  by clicking an actual word, never inferred from text.
- `src/lib/reference.ts` resolves typed references ("1 Cor 13", "ps 23:1") only
  against installed books, and returns null rather than guessing on an ambiguous
  or unknown book. `npm test` covers it.
