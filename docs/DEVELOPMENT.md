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

# Import a public-domain translation (World English Bible) into the app DB.
# Writes to the same SQLite file the app reads:
#   macOS: ~/Library/Application Support/com.rooted.app/rooted.db
python3 scripts/import_bible.py                 # WEB (default)
# python3 scripts/import_bible.py --translation kjv   # (Phase 2)
```

## Run

```bash
npm run tauri dev      # launches the desktop app (Rust build + Vite UI)
npm run build          # typecheck + build the frontend only
```

The database path can be overridden for both the app and the import script via
the `ROOTED_DB` environment variable, or the import script's `--db` flag.

## Layout

| Path | Purpose |
|------|---------|
| `src/` | React UI. `src/features/reader/` (reading pane), `src/lib/api.ts` (typed Tauri commands). |
| `src-tauri/src/db.rs` | SQLite access + query commands. |
| `src-tauri/src/lib.rs` | Tauri command registration + app setup. |
| `src-tauri/migrations/0001_init.sql` | Canonical schema (books, translations, verses, tokens, notes, highlights). |
| `scripts/import_bible.py` | Parse a getbible.net translation → canonical BCV + token rows. |

## Data model notes

- Verses use **OSIS BCV ids** (`Gen.1.1`) as a translation-independent key.
- Word anchoring uses `(translation_id, verse_id, token_idx)` with a stored
  `surface` snapshot and char offsets, so word-level notes/highlights survive
  re-imports and degrade gracefully across translations (Phase 2).
