#!/usr/bin/env python3
"""
Import a public-domain Bible translation into Rooted's SQLite database.

Source: getbible.net v2 (per-book JSON), e.g. https://api.getbible.net/v2/web/1.json
Parses each verse, tokenizes into words with character offsets, and writes rows
into the canonical schema (books, translations, verses, tokens).

The database path matches what the Tauri app uses:
  - $ROOTED_DB if set, else the OS app-data dir + com.rooted.app/rooted.db
  - override with --db

Usage:
  python3 scripts/import_bible.py                 # imports WEB
  python3 scripts/import_bible.py --translation kjv
  python3 scripts/import_bible.py --db ./data/rooted.db
"""
from __future__ import annotations

import argparse
import json
import os
import re
import sqlite3
import sys
import urllib.request
from pathlib import Path

# Canonical 66-book order: (OSIS code, display name, testament)
BOOKS: list[tuple[str, str, str]] = [
    ("Gen", "Genesis", "OT"), ("Exod", "Exodus", "OT"), ("Lev", "Leviticus", "OT"),
    ("Num", "Numbers", "OT"), ("Deut", "Deuteronomy", "OT"), ("Josh", "Joshua", "OT"),
    ("Judg", "Judges", "OT"), ("Ruth", "Ruth", "OT"), ("1Sam", "1 Samuel", "OT"),
    ("2Sam", "2 Samuel", "OT"), ("1Kgs", "1 Kings", "OT"), ("2Kgs", "2 Kings", "OT"),
    ("1Chr", "1 Chronicles", "OT"), ("2Chr", "2 Chronicles", "OT"), ("Ezra", "Ezra", "OT"),
    ("Neh", "Nehemiah", "OT"), ("Esth", "Esther", "OT"), ("Job", "Job", "OT"),
    ("Ps", "Psalms", "OT"), ("Prov", "Proverbs", "OT"), ("Eccl", "Ecclesiastes", "OT"),
    ("Song", "Song of Solomon", "OT"), ("Isa", "Isaiah", "OT"), ("Jer", "Jeremiah", "OT"),
    ("Lam", "Lamentations", "OT"), ("Ezek", "Ezekiel", "OT"), ("Dan", "Daniel", "OT"),
    ("Hos", "Hosea", "OT"), ("Joel", "Joel", "OT"), ("Amos", "Amos", "OT"),
    ("Obad", "Obadiah", "OT"), ("Jonah", "Jonah", "OT"), ("Mic", "Micah", "OT"),
    ("Nah", "Nahum", "OT"), ("Hab", "Habakkuk", "OT"), ("Zeph", "Zephaniah", "OT"),
    ("Hag", "Haggai", "OT"), ("Zech", "Zechariah", "OT"), ("Mal", "Malachi", "OT"),
    ("Matt", "Matthew", "NT"), ("Mark", "Mark", "NT"), ("Luke", "Luke", "NT"),
    ("John", "John", "NT"), ("Acts", "Acts", "NT"), ("Rom", "Romans", "NT"),
    ("1Cor", "1 Corinthians", "NT"), ("2Cor", "2 Corinthians", "NT"), ("Gal", "Galatians", "NT"),
    ("Eph", "Ephesians", "NT"), ("Phil", "Philippians", "NT"), ("Col", "Colossians", "NT"),
    ("1Thess", "1 Thessalonians", "NT"), ("2Thess", "2 Thessalonians", "NT"),
    ("1Tim", "1 Timothy", "NT"), ("2Tim", "2 Timothy", "NT"), ("Titus", "Titus", "NT"),
    ("Phlm", "Philemon", "NT"), ("Heb", "Hebrews", "NT"), ("Jas", "James", "NT"),
    ("1Pet", "1 Peter", "NT"), ("2Pet", "2 Peter", "NT"), ("1John", "1 John", "NT"),
    ("2John", "2 John", "NT"), ("3John", "3 John", "NT"), ("Jude", "Jude", "NT"),
    ("Rev", "Revelation", "NT"),
]

# getbible slug -> (abbrev, display name, license)
TRANSLATIONS = {
    "web": ("WEB", "World English Bible", "Public Domain"),
    "kjv": ("KJV", "King James Version", "Public Domain"),
    "asv": ("ASV", "American Standard Version", "Public Domain"),
}

# Word tokenizer: unicode words with internal apostrophes/hyphens.
WORD_RE = re.compile(r"[^\W_]+(?:['’\-][^\W_]+)*", re.UNICODE)


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


def migration_sql() -> str:
    path = Path(__file__).resolve().parent.parent / "src-tauri" / "migrations" / "0001_init.sql"
    return path.read_text(encoding="utf-8")


def fetch_book(slug: str, nr: int) -> dict:
    url = f"https://api.getbible.net/v2/{slug}/{nr}.json"
    req = urllib.request.Request(url, headers={"User-Agent": "rooted-bible-import/0.1"})
    with urllib.request.urlopen(req, timeout=60) as resp:
        return json.loads(resp.read().decode("utf-8"))


def iter_verses(book_json: dict):
    """Yield (chapter:int, verse:int, text:str) from a getbible book payload."""
    for ch in book_json.get("chapters", []):
        ch_num = int(ch.get("chapter"))
        for v in ch.get("verses", []):
            yield ch_num, int(v.get("verse")), (v.get("text") or "").strip()


def tokenize(text: str) -> list[tuple[int, str, int, int]]:
    """Return [(idx, surface, char_start, char_end), ...] for the verse text."""
    out = []
    for i, m in enumerate(WORD_RE.finditer(text)):
        out.append((i, m.group(0), m.start(), m.end()))
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description="Import a Bible translation into Rooted.")
    ap.add_argument("--translation", default="web", choices=sorted(TRANSLATIONS),
                    help="getbible translation slug (default: web)")
    ap.add_argument("--db", default=None, help="path to rooted.db (default: app-data dir)")
    args = ap.parse_args()

    slug = args.translation
    abbrev, name, license_ = TRANSLATIONS[slug]
    db_path = Path(args.db) if args.db else default_db_path()
    db_path.parent.mkdir(parents=True, exist_ok=True)

    print(f"→ Importing {name} ({abbrev}) into {db_path}")
    conn = sqlite3.connect(db_path)
    conn.execute("PRAGMA foreign_keys = ON;")
    conn.executescript(migration_sql())

    # Seed canonical books (idempotent).
    conn.executemany(
        "INSERT OR IGNORE INTO books(osis, canonical_order, name, testament) VALUES (?,?,?,?)",
        [(osis, i + 1, disp, test) for i, (osis, disp, test) in enumerate(BOOKS)],
    )

    # Upsert the translation, then re-import cleanly.
    conn.execute(
        "INSERT OR IGNORE INTO translations(abbrev, name, language, license, source_type) "
        "VALUES (?,?,?,?, 'bundled')",
        (abbrev, name, "en", license_),
    )
    tid = conn.execute("SELECT id FROM translations WHERE abbrev = ?", (abbrev,)).fetchone()[0]
    conn.execute("DELETE FROM tokens WHERE translation_id = ?", (tid,))
    conn.execute("DELETE FROM verses WHERE translation_id = ?", (tid,))

    canonical = 0
    total_verses = 0
    total_tokens = 0
    for nr, (osis, disp, _test) in enumerate(BOOKS, start=1):
        book_json = fetch_book(slug, nr)
        verse_rows = []
        token_rows = []
        for ch_num, v_num, text in iter_verses(book_json):
            canonical += 1
            verse_id = f"{osis}.{ch_num}.{v_num}"
            verse_rows.append((verse_id, tid, osis, ch_num, v_num, text, canonical))
            for idx, surface, cs, ce in tokenize(text):
                token_rows.append((tid, verse_id, idx, surface, cs, ce))
        conn.executemany(
            "INSERT INTO verses(verse_id, translation_id, book_osis, chapter, verse, text, canonical_order) "
            "VALUES (?,?,?,?,?,?,?)",
            verse_rows,
        )
        conn.executemany(
            "INSERT INTO tokens(translation_id, verse_id, idx, surface, char_start, char_end) "
            "VALUES (?,?,?,?,?,?)",
            token_rows,
        )
        total_verses += len(verse_rows)
        total_tokens += len(token_rows)
        print(f"  [{nr:>2}/66] {disp:<20} {len(verse_rows):>4} verses", flush=True)

    conn.commit()
    conn.close()
    print(f"✓ Done: {total_verses} verses, {total_tokens} tokens for {abbrev}.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
