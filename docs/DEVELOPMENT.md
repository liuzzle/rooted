# Rooted — Development

Local-first Bible study app. Tauri v2 (Rust) shell + React/TypeScript UI + SQLite,
with a Python sidecar for AI ingestion (added in later phases).

See the full build plan in [`docs/PLAN.md`](./PLAN.md).

## Prerequisites

- **Node** 20.19+ (or 22.12+) and npm
- **Rust** stable (`rustup`) — Tauri backend
- **Python** 3.10+ — Bible import (later: AI sidecar)

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
```

The end-to-end ingestion test (Rust queues a job → the real Python worker
processes it → a note appears) is in the `--ignored` set because it shells out
to `python3`.

The database path can be overridden for both the app and the import script via
the `ROOTED_DB` environment variable, or the import script's `--db` flag.

## Layout

| Path | Purpose |
|------|---------|
| `src/App.tsx` | Shell: Read · Notes · Dashboard views, active translation, pack modal. |
| `src/features/` | `reader/` (reading pane), `notes/` (note panel + chapter rail), `library/` (all notes), `dashboard/`, `translations/` (pack manager). |
| `src/lib/api.ts` | Typed Tauri commands. `src/lib/reference.ts` — scripture reference parsing. |
| `src-tauri/src/db.rs` | SQLite access + query commands (with unit tests). |
| `src-tauri/src/packs.rs` | Pack registry, download, tokenizer, import. |
| `src-tauri/src/lib.rs` | Tauri command registration + app setup. |
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
looks), `.docx` (paragraph text via stdlib `zipfile` + XML, no dependency), and
`.pdf` **text layer only** — that needs `pip install pypdf`, and a scanned PDF is
reported as needing OCR rather than guessed at. Auto-verify for perfect
extractions exists behind `--auto-verify` and is off by default.

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
