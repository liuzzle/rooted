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

Both importers tokenize identically; a test asserts it, because a mismatch
would shift token indices and move every word anchor.

## Run

```bash
npm run tauri dev      # launches the desktop app (Rust build + Vite UI)
npm run build          # typecheck + build the frontend only
cargo test --manifest-path src-tauri/Cargo.toml              # data-layer tests
cargo test --manifest-path src-tauri/Cargo.toml -- --ignored # + network tests
```

The database path can be overridden for both the app and the import script via
the `ROOTED_DB` environment variable, or the import script's `--db` flag.

## Layout

| Path | Purpose |
|------|---------|
| `src/` | React UI. `src/features/reader/` (reading pane), `src/features/notes/` (notes & highlights), `src/features/translations/` (pack manager), `src/lib/api.ts` (typed Tauri commands). |
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
