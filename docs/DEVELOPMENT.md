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
| `src/App.tsx` | Shell: Read · Search · Notes · Topics · Dashboard · Ingest views, active translation, pack modal. |
| `src/features/` | `reader/` (reading pane), `notes/` (note panel + chapter rail), `library/` (all notes), `dashboard/`, `translations/` (pack manager), `ingest/` (upload, pipeline status, review), `topics/` (list, pages, graph), `search/`. |
| `src/lib/api.ts` | Typed Tauri commands. `src/lib/reference.ts` — scripture reference parsing. |
| `src-tauri/src/db.rs` | SQLite access + query commands (with unit tests). |
| `src-tauri/src/packs.rs` | Pack registry, download, tokenizer, import. |
| `src-tauri/src/lib.rs` | Tauri command registration + app setup. |
| `src-tauri/src/ingest.rs` | Jobs, documents, pages, spans, verification. |
| `src-tauri/src/sidecar.rs` | Worker process lifecycle + what it reports it can read. |
| `src-tauri/src/graph.rs` | Reading the topic graph: topics, citations, references, edges. |
| `src-tauri/src/search.rs` | Search: two lanes of cited passages, fused from words, topics, citations and meaning. |
| `sidecar/worker.py` | Job state machine. `sidecar/engines.py` — the reading engines. `concepts.py`, `references.py` — topics and scripture references. `embeddings.py` — vectors for search by meaning. |
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

## Topics (Phase 5)

The worker reads every note for topics — one typed in the app as much as one
that arrived as a scan, since a graph covering only ingested notes would be a
lie about the corpus. It works from `notes`, so nothing needs to be re-run when
a note is written by hand.

**The notes are German, and that changes the rules.** German capitalises every
noun, so "capitalised mid-sentence" identifies a noun, not a name — and nearly
every sentence has several. German also declines: "Herr" and "Herrn" are one
word. So:

- **Keys are lemma-then-stem.** Neither alone works. Snowball misses
  "Herr"/"Herrn" (it protects short stems, and hand-stripping the -n wrongly
  merges "Stein" into "Stei"); lemmatising misses "Glaube"/"Glauben", because
  "Glauben" is itself a dictionary word. Composed, both pairs merge while
  "Herr" and "Herz" stay apart. `simplemma` + `snowballstemmer`, both pure
  Python; without them keys fall back to casefolding, which merges nothing.
- **Mentions are found the same way.** A topic recorded as "Herr" has to find
  "Herrn" in the text, or the graph understates what the note says. Matching is
  by word sequence on the normalised form — so "grace" is still not found
  inside "disgrace", and "God's grace" still does not match "the grace of God".
- **Stopwords are checked by lemma**, so one entry ("sein") covers the whole
  paradigm ("seine", "seinen", "seinem").
- **Recurrence carries the weight in German.** A capitalised word needs to
  appear at least twice; in English a name mid-sentence counts once. Either way
  a word only ever capitalised at the start of a sentence proves nothing, which
  is what stops "Later the law came. Later still…" from making a topic.
- **Language is detected per note** by function-word overlap, and stored on the
  source, because which rules were applied is part of knowing what the result
  means.

`sources.scheme` records the rules a note was read under, so improving the
extractor re-reads notes that were already indexed instead of leaving them on
the old rules forever.

**Scripture references are found, stored, and clickable.** `references.py`
resolves German and English forms — "Joh 3,16", "1. Mose 12,1-3", "Röm 4,3",
"John 3:16" — with spacing and umlaut variants generated rather than listed, so
"Roemer" and "1 Kor" resolve too. They are stored in `verse_links` with offsets,
like everything else here, and the note body is *split* at those offsets rather
than rewritten: hover previews the verse in the translation you're reading,
click opens it. A citation is also excluded from topic extraction — "Vergleiche
Joh 3,16" should not make a topic out of a gospel abbreviation.

**Extraction is span-selection, not generation.** An extractor proposes a
string; `concepts.gate` finds it verbatim in the note or rejects it. That's why
extractors return *strings and never offsets*: a model asked for character
positions produces plausible ones, so the positions are computed here instead
and cannot be wrong about where a word is. The stored surface is sliced out of
the note, so a concept's citation always shows what was actually written — if a
model reports "God's grace" for a note reading "the grace of God", nothing is
stored. `test_a_paraphrase_is_rejected` is that promise in executable form.

Two extractors, both gated the same way — "this one is trustworthy" is the kind
of exception that stops being true quietly:

- **`phrases`** always runs, needs nothing installed. Capitalised runs that
  aren't merely sentence openings (a name earns its capital by appearing
  mid-sentence somewhere), plus words the note keeps returning to.
- **`ollama/<model>`** runs when a local model server answers on
  `ROOTED_OLLAMA_HOST`. It only ever *adds* topics: the deterministic pass
  proposes first, so the model can't rename an existing one, and its labels
  come back spelled as the note spells them.

Topics are proposed from the **whole note** and then located **per paragraph**.
Counting repetition inside a paragraph would miss exactly the notes that are
most clearly about something — a note returning to "covenant" in three separate
paragraphs is more about covenant than one that says it three times in a row.

**A mention cites a range of a chunk of a source.** Editing a note moves those
offsets, so a changed note is re-read and its chunks replaced outright; the
cascade takes the old mentions with them. `sources.text_hash` is what decides
"changed" — a mention pointing into text that no longer exists is the one
failure this ledger must not have.

`concepts.key` collapses case and whitespace so one word isn't two topics. It
is **not** a synonym test: merging "grace" with "unmerited favour" needs
embeddings and a person to confirm, and that pass isn't built yet. Likewise
`concept_mentions.verified` and the `embedding` columns exist unused, because
the merge and verse-suggestion passes will need somewhere to say so.

## Search (Phase 6)

The **Search** tab returns passages, never answers. There are two lanes, your
notes and the Bible in the translation you're reading, and every result in
either one is a chunk or a verse quoted whole, with where it came from. A query
nothing matches shows **"No sources found"**: that is a result, not an error,
and it's what `an_unknown_topic_finds_nothing_rather_than_something` tests.

Each lane combines several independent ways of finding a passage, and every hit
says which ones found it:

| Found by | What it means | Needs |
|---|---|---|
| `words` | The words typed are in the passage (FTS5, as prefixes) | nothing |
| `topic` | The query is a topic, and these are its recorded mentions | Phase 5 |
| `cited` | A matching note cites this verse (Bible lane only) | Phase 5 |
| `meaning` | Close in meaning, even if the words differ | a local embedding model |

They are merged by **reciprocal rank fusion**, not by adding scores. bm25 and
cosine are on different scales, and a passage found two ways ought to outrank
one found strongly in only one.

**Words** work out of the box. `chunks_fts` and `verses_fts` are
external-content FTS5 tables kept in step by triggers, so they stay correct
whichever process writes: the worker writes chunks, the app writes verses. The
tokenizer folds diacritics ("romer" finds "Römer") but doesn't stem, so each
query term is matched as a **prefix** instead ("Gnade" finds "Gnaden"). That
copes with German declension without a second normaliser in Rust. Query text is
always quoted before it reaches FTS, so `OR`, `NEAR(` or a stray `"` are just
text.

**Meaning** is optional, local, and checked rather than assumed:

```bash
ollama pull bge-m3      # multilingual; the notes are German
```

The worker gives each chunk and verse a vector, note passages first and then
the translation you have open, and records which model made each one
(`chunks.embedded_by`, `verse_vectors.model`). It reports that model and host
in `settings.embedding`, and the app embeds the query with exactly that model:
vectors from two different models can't be compared. Expect roughly 50 verses a
second on Apple Silicon, so about ten minutes per translation, in the
background. The search page says how far it has got, and says so when meaning
is off and why.

A vector only **ranks** passages that already exist. It is never turned back
into text, so a weak model makes search worse, never untruthful. Below a
similarity floor (`ROOTED_SEMANTIC_FLOOR`, default 0.55) a passage isn't
reported at all. That floor is what keeps an unrelated query empty, and it
has to be calibrated against **a whole translation, not a sample**. On 8
verses, nonsense never scored above 0.31. On all 31,070 verses of ELB71,
nonsense found something at 0.40–0.49 ("asdfgh" landed on a genealogy), while
real multi-word queries topped out at 0.63–0.67. A relative test (z-score
against the query's own distribution) doesn't separate the two; this floor
does. Single words score like noise, so they get nothing from meaning, which is
fine because the words lane covers them. Change the model and the floor has to
be measured again.

Verse vectors are kept in memory between queries (`search::VectorCache`, about
120 MB for a 1024-dimension model over one translation) and reloaded when the
count in the table changes, so the cache keeps up while the worker is still
filling it. Search is brute force. That's fine at this scale and avoids
loading a SQLite extension into both processes. sqlite-vec is the upgrade if
it's ever needed.

Env: `ROOTED_EMBED_MODEL` (default `bge-m3`), `ROOTED_OLLAMA_HOST` (shared with
the topic extractor), `ROOTED_SEMANTIC_FLOOR`. The worker tests point
`ROOTED_OLLAMA_HOST` at a closed port, so no test ever reaches a model server
that happens to be running.

### The topic graph

**Topics → Graph** draws topics as nodes. An edge joins two topics **only where
one passage mentions both**, and clicking it lists those passages with both
mentions marked. Similarity is deliberately not drawn: it's a guess, and a line
on a graph looks like a fact.

It's interactive:
- **Drag a topic** and the physics runs live, so the topics written alongside
  it follow.
- **Drag the background** to pan, **scroll** to zoom around the cursor.
- **Click** a topic to list what it's written alongside, or a line to read the
  passages behind it. **Double-click** opens the topic page.
- **Find** centres a topic by name.

Circles and labels are sized in screen pixels, so zooming spreads the graph out
instead of inflating it.

The layout (`forceLayout.ts`) is Fruchterman–Reingold, run to rest before the
first frame and started from a spiral rather than random positions, so the same
graph opens the same way on every visit. Gravity has to be strong (1 per unit of
distance): every pair of topics repels, so with weak gravity the graph only
stops growing at about k·√(n/G). At 0.02 it spread to forty times its intended
area and was drawn as a thin streak. `forceLayout.test.ts` guards against that.

Double-click is detected in the pointer-up handler, not with `dblclick`: the
canvas captures the pointer so a drag can't escape it, and capture retargets
`dblclick` to the canvas.

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
