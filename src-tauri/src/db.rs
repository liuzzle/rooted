use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;

/// Shared DB handle stored in Tauri state.
pub struct Db(pub Mutex<Connection>);

/// Resolve the database path.
/// Priority: `ROOTED_DB` env override, else `<app_data_dir>/rooted.db`.
/// On macOS the app data dir is `~/Library/Application Support/com.rooted.app`.
pub fn resolve_db_path() -> PathBuf {
    if let Ok(p) = std::env::var("ROOTED_DB") {
        return PathBuf::from(p);
    }
    let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("com.rooted.app").join("rooted.db")
}

/// Open the DB (creating it if needed) and apply the schema migration.
pub fn open() -> Result<Connection, String> {
    let path = resolve_db_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let conn = Connection::open(&path).map_err(|e| e.to_string())?;
    apply_schema(&conn)?;
    Ok(conn)
}

/// Every migration, in order. Each is written to be idempotent (`IF NOT
/// EXISTS`), so applying the whole list on every start is safe.
const MIGRATIONS: &[(&str, &str)] = &[
    ("0001_init", include_str!("../migrations/0001_init.sql")),
    ("0002_settings", include_str!("../migrations/0002_settings.sql")),
    ("0003_ingestion", include_str!("../migrations/0003_ingestion.sql")),
    ("0004_pages_spans", include_str!("../migrations/0004_pages_spans.sql")),
    ("0005_escalation", include_str!("../migrations/0005_escalation.sql")),
];

/// Enable foreign keys and apply the canonical schema (idempotent).
///
/// WAL matters from Phase 3 on: the Python ingestion worker holds its own
/// connection to this same file, and the default rollback journal would have
/// readers and the writer locking each other out.
pub fn apply_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = WAL;
         PRAGMA busy_timeout = 5000;
         CREATE TABLE IF NOT EXISTS schema_migrations (
           name       TEXT PRIMARY KEY,
           applied_at TEXT NOT NULL DEFAULT (datetime('now'))
         );",
    )
    .map_err(|e| e.to_string())?;

    // Applied once and recorded. The early migrations are individually
    // idempotent, but `ALTER TABLE ADD COLUMN` is not — hence the ledger.
    for (name, sql) in MIGRATIONS {
        let applied: bool = conn
            .query_row(
                "SELECT 1 FROM schema_migrations WHERE name = ?1",
                rusqlite::params![name],
                |_| Ok(true),
            )
            .optional()
            .map_err(|e| e.to_string())?
            .unwrap_or(false);
        if applied {
            continue;
        }
        conn.execute_batch(sql)
            .map_err(|e| format!("migration {name} failed: {e}"))?;
        conn.execute(
            "INSERT INTO schema_migrations (name) VALUES (?1)",
            rusqlite::params![name],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

pub fn get_setting(conn: &Connection, key: &str) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        rusqlite::params![key],
        |r| r.get(0),
    )
    .optional()
    .map_err(|e| e.to_string())
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<(), String> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value,
                                        updated_at = datetime('now')",
        rusqlite::params![key, value],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Query payloads
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct Translation {
    pub id: i64,
    pub abbrev: String,
    pub name: String,
    pub language: String,
}

#[derive(Serialize)]
pub struct Book {
    pub osis: String,
    pub name: String,
    pub testament: String,
    pub canonical_order: i64,
    pub chapter_count: i64,
}

#[derive(Serialize)]
pub struct Token {
    pub idx: i64,
    pub surface: String,
    pub char_start: i64,
    pub char_end: i64,
}

#[derive(Serialize)]
pub struct Verse {
    pub verse_id: String,
    pub verse: i64,
    pub text: String,
    pub tokens: Vec<Token>,
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

/// Translations that actually have text installed. A pack that was removed
/// keeps its `translations` row (so word anchors written against it still
/// resolve) but has no verses, and must not appear in the reader.
pub fn list_translations(conn: &Connection) -> Result<Vec<Translation>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, abbrev, name, language FROM translations t
              WHERE EXISTS (SELECT 1 FROM verses v WHERE v.translation_id = t.id)
              ORDER BY abbrev",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Translation {
                id: r.get(0)?,
                abbrev: r.get(1)?,
                name: r.get(2)?,
                language: r.get(3)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

/// Books that actually have verses in the given translation, in canonical order,
/// each with its max chapter number.
pub fn list_books(conn: &Connection, translation_id: i64) -> Result<Vec<Book>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT b.osis, b.name, b.testament, b.canonical_order, MAX(v.chapter)
             FROM books b
             JOIN verses v ON v.book_osis = b.osis AND v.translation_id = ?1
             GROUP BY b.osis
             ORDER BY b.canonical_order",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([translation_id], |r| {
            Ok(Book {
                osis: r.get(0)?,
                name: r.get(1)?,
                testament: r.get(2)?,
                canonical_order: r.get(3)?,
                chapter_count: r.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

/// All verses (with tokens) for one chapter, in verse order.
pub fn get_chapter(
    conn: &Connection,
    translation_id: i64,
    book_osis: &str,
    chapter: i64,
) -> Result<Vec<Verse>, String> {
    let mut vstmt = conn
        .prepare(
            "SELECT verse_id, verse, text FROM verses
             WHERE translation_id = ?1 AND book_osis = ?2 AND chapter = ?3
             ORDER BY verse",
        )
        .map_err(|e| e.to_string())?;
    let verse_rows = vstmt
        .query_map(rusqlite::params![translation_id, book_osis, chapter], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    let mut tstmt = conn
        .prepare(
            "SELECT idx, surface, char_start, char_end FROM tokens
             WHERE translation_id = ?1 AND verse_id = ?2 ORDER BY idx",
        )
        .map_err(|e| e.to_string())?;

    let mut out = Vec::with_capacity(verse_rows.len());
    for (verse_id, verse, text) in verse_rows {
        let tokens = tstmt
            .query_map(rusqlite::params![translation_id, verse_id], |r| {
                Ok(Token {
                    idx: r.get(0)?,
                    surface: r.get(1)?,
                    char_start: r.get(2)?,
                    char_end: r.get(3)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        out.push(Verse {
            verse_id,
            verse,
            text,
            tokens,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Notes & highlights (Phase 1)
// ---------------------------------------------------------------------------

/// Where a note or highlight is attached.
///
/// Verse anchors are deliberately translation-independent: `translation_id`,
/// `token_idx` and `surface` are cleared by [`Anchor::normalized`] so a verse
/// note never becomes bound to the translation it was written in. Word anchors
/// keep `(translation_id, token_idx)` plus a `surface` snapshot so they survive
/// a re-import and can degrade gracefully in another translation (Phase 2).
#[derive(Deserialize, Clone)]
pub struct Anchor {
    pub anchor_type: String, // 'verse' | 'word'
    pub verse_id: String,
    pub translation_id: Option<i64>,
    pub token_idx: Option<i64>,
    pub surface: Option<String>,
}

impl Anchor {
    fn is_word(&self) -> bool {
        self.anchor_type == "word"
    }

    fn normalized(self) -> Result<Anchor, String> {
        match self.anchor_type.as_str() {
            "verse" => Ok(Anchor {
                anchor_type: self.anchor_type,
                verse_id: self.verse_id,
                translation_id: None,
                token_idx: None,
                surface: None,
            }),
            "word" => {
                let translation_id = self
                    .translation_id
                    .ok_or("word anchor requires a translation_id")?;
                let token_idx = self.token_idx.ok_or("word anchor requires a token_idx")?;
                Ok(Anchor {
                    anchor_type: self.anchor_type,
                    verse_id: self.verse_id,
                    translation_id: Some(translation_id),
                    token_idx: Some(token_idx),
                    surface: self.surface,
                })
            }
            other => Err(format!("unknown anchor_type '{other}'")),
        }
    }
}

#[derive(Serialize)]
pub struct Note {
    pub note_id: i64,
    pub title: Option<String>,
    pub body: String,
    pub created_at: String,
    pub updated_at: String,
    pub anchor_type: String,
    pub verse_id: String,
    pub translation_id: Option<i64>,
    pub token_idx: Option<i64>,
    pub surface: Option<String>,
    /// Abbrev of the translation a word note was written in (`None` for verse
    /// notes, or if that pack's row has since vanished).
    pub origin_abbrev: Option<String>,
    /// True when this is a word note surfaced at verse level because the reader
    /// is in a different translation than the one it was anchored in (Tier-1
    /// graceful degradation — we never re-point a word anchor at another text).
    pub degraded: bool,
}

#[derive(Serialize)]
pub struct Highlight {
    pub id: i64,
    pub anchor_type: String,
    pub verse_id: String,
    pub translation_id: Option<i64>,
    pub token_idx: Option<i64>,
    pub color: String,
}

/// One "there is a note here" indicator for the reading pane.
#[derive(Serialize)]
pub struct NoteMark {
    pub verse_id: String,
    pub token_idx: Option<i64>, // None = verse-level indicator
    /// Notes anchored exactly here (verse notes, or word notes in the active
    /// translation).
    pub count: i64,
    /// Word notes from *other* translations, shown on the verse instead.
    pub degraded: i64,
}

#[derive(Serialize)]
pub struct ChapterAnnotations {
    pub highlights: Vec<Highlight>,
    pub note_marks: Vec<NoteMark>,
}

/// Create a note, optionally anchored, in one transaction. Returns the new id.
///
/// `anchor: None` is a standalone note — a general study note with no scripture
/// reference yet. [`set_note_anchor`] can give it one later.
pub fn create_note(
    conn: &mut Connection,
    anchor: Option<Anchor>,
    title: Option<String>,
    body: String,
) -> Result<i64, String> {
    let anchor = anchor.map(Anchor::normalized).transpose()?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO notes (title, body) VALUES (?1, ?2)",
        rusqlite::params![title, body],
    )
    .map_err(|e| e.to_string())?;
    let note_id = tx.last_insert_rowid();
    if let Some(anchor) = anchor {
        insert_anchor(&tx, note_id, &anchor)?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(note_id)
}

fn insert_anchor(conn: &Connection, note_id: i64, anchor: &Anchor) -> Result<(), String> {
    conn.execute(
        "INSERT INTO note_anchors
           (note_id, anchor_type, verse_id, translation_id, token_idx, surface)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            note_id,
            anchor.anchor_type,
            anchor.verse_id,
            anchor.translation_id,
            anchor.token_idx,
            anchor.surface,
        ],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// Point a note at a reference, move it, or (with `None`) unanchor it.
pub fn set_note_anchor(
    conn: &mut Connection,
    note_id: i64,
    anchor: Option<Anchor>,
) -> Result<(), String> {
    let anchor = anchor.map(Anchor::normalized).transpose()?;
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM notes WHERE note_id = ?1",
            rusqlite::params![note_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if exists == 0 {
        return Err(format!("note {note_id} not found"));
    }

    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "DELETE FROM note_anchors WHERE note_id = ?1",
        rusqlite::params![note_id],
    )
    .map_err(|e| e.to_string())?;
    if let Some(anchor) = anchor {
        insert_anchor(&tx, note_id, &anchor)?;
    }
    tx.execute(
        "UPDATE notes SET updated_at = datetime('now') WHERE note_id = ?1",
        rusqlite::params![note_id],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

pub fn update_note(
    conn: &Connection,
    note_id: i64,
    title: Option<String>,
    body: String,
) -> Result<(), String> {
    let changed = conn
        .execute(
            "UPDATE notes SET title = ?2, body = ?3, updated_at = datetime('now')
             WHERE note_id = ?1",
            rusqlite::params![note_id, title, body],
        )
        .map_err(|e| e.to_string())?;
    if changed == 0 {
        return Err(format!("note {note_id} not found"));
    }
    Ok(())
}

/// Delete a note; its anchors go with it (ON DELETE CASCADE).
pub fn delete_note(conn: &Connection, note_id: i64) -> Result<(), String> {
    let changed = conn
        .execute(
            "DELETE FROM notes WHERE note_id = ?1",
            rusqlite::params![note_id],
        )
        .map_err(|e| e.to_string())?;
    if changed == 0 {
        return Err(format!("note {note_id} not found"));
    }
    Ok(())
}

const NOTE_COLUMNS: &str = "SELECT n.note_id, n.title, n.body, n.created_at, n.updated_at,
            a.anchor_type, a.verse_id, a.translation_id, a.token_idx, a.surface, t.abbrev
       FROM notes n
       JOIN note_anchors a ON a.note_id = n.note_id
       LEFT JOIN translations t ON t.id = a.translation_id";

fn map_note(r: &rusqlite::Row, active_translation_id: i64) -> rusqlite::Result<Note> {
    let anchor_type: String = r.get(5)?;
    let translation_id: Option<i64> = r.get(7)?;
    let degraded = anchor_type == "word" && translation_id != Some(active_translation_id);
    Ok(Note {
        note_id: r.get(0)?,
        title: r.get(1)?,
        body: r.get(2)?,
        created_at: r.get(3)?,
        updated_at: r.get(4)?,
        anchor_type,
        verse_id: r.get(6)?,
        translation_id,
        token_idx: r.get(8)?,
        surface: r.get(9)?,
        origin_abbrev: r.get(10)?,
        degraded,
    })
}

/// Notes attached to one anchor, oldest first.
///
/// A **word** anchor returns only notes written on that exact word in that
/// exact translation. A **verse** anchor returns the verse's own
/// translation-independent notes *plus* any word notes made in a translation
/// other than `active_translation_id` — those can't be re-anchored to a text
/// they were never written against, so they surface here, flagged `degraded`,
/// carrying the word they were written on.
pub fn list_notes(
    conn: &Connection,
    anchor: Anchor,
    active_translation_id: i64,
) -> Result<Vec<Note>, String> {
    let anchor = anchor.normalized()?;

    if anchor.is_word() {
        let mut stmt = conn
            .prepare(&format!(
                "{NOTE_COLUMNS}
                  WHERE a.anchor_type = 'word' AND a.verse_id = ?1
                    AND a.translation_id = ?2 AND a.token_idx = ?3
                  ORDER BY n.created_at, n.note_id"
            ))
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(
                rusqlite::params![anchor.verse_id, anchor.translation_id, anchor.token_idx],
                |r| map_note(r, active_translation_id),
            )
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>();
        return rows.map_err(|e| e.to_string());
    }

    let mut stmt = conn
        .prepare(&format!(
            "{NOTE_COLUMNS}
              WHERE a.verse_id = ?1
                AND (a.anchor_type = 'verse'
                     OR (a.anchor_type = 'word' AND a.translation_id <> ?2))
              ORDER BY n.created_at, n.note_id"
        ))
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(
            rusqlite::params![anchor.verse_id, active_translation_id],
            |r| map_note(r, active_translation_id),
        )
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>();
    rows.map_err(|e| e.to_string())
}

/// Set (or replace) the highlight at an anchor. One highlight per anchor.
pub fn set_highlight(conn: &mut Connection, anchor: Anchor, color: String) -> Result<(), String> {
    let anchor = anchor.normalized()?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    delete_highlight_tx(&tx, &anchor)?;
    tx.execute(
        "INSERT INTO highlights
           (anchor_type, verse_id, translation_id, token_idx, surface, color)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            anchor.anchor_type,
            anchor.verse_id,
            anchor.translation_id,
            anchor.token_idx,
            anchor.surface,
            color,
        ],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

/// Remove the highlight at an anchor (no-op if there isn't one).
pub fn clear_highlight(conn: &Connection, anchor: Anchor) -> Result<(), String> {
    let anchor = anchor.normalized()?;
    delete_highlight_tx(conn, &anchor)
}

fn delete_highlight_tx(conn: &Connection, anchor: &Anchor) -> Result<(), String> {
    if anchor.is_word() {
        conn.execute(
            "DELETE FROM highlights
              WHERE anchor_type = 'word' AND verse_id = ?1
                AND translation_id = ?2 AND token_idx = ?3",
            rusqlite::params![anchor.verse_id, anchor.translation_id, anchor.token_idx],
        )
    } else {
        conn.execute(
            "DELETE FROM highlights WHERE anchor_type = 'verse' AND verse_id = ?1",
            rusqlite::params![anchor.verse_id],
        )
    }
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// Every highlight and note indicator for one chapter, in a single round trip.
///
/// Verse-level annotations are matched by `verse_id` alone (translation-
/// independent); word-level ones are scoped to the active translation.
pub fn chapter_annotations(
    conn: &Connection,
    translation_id: i64,
    book_osis: &str,
    chapter: i64,
) -> Result<ChapterAnnotations, String> {
    let verse_scope = "SELECT verse_id FROM verses
                        WHERE translation_id = ?1 AND book_osis = ?2 AND chapter = ?3";

    let mut hstmt = conn
        .prepare(&format!(
            "SELECT id, anchor_type, verse_id, translation_id, token_idx, color
               FROM highlights
              WHERE verse_id IN ({verse_scope})
                AND (anchor_type = 'verse' OR translation_id = ?1)"
        ))
        .map_err(|e| e.to_string())?;
    let highlights = hstmt
        .query_map(rusqlite::params![translation_id, book_osis, chapter], |r| {
            Ok(Highlight {
                id: r.get(0)?,
                anchor_type: r.get(1)?,
                verse_id: r.get(2)?,
                translation_id: r.get(3)?,
                token_idx: r.get(4)?,
                color: r.get(5)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    // Word notes in the active translation mark their own word; verse notes and
    // word notes from other translations both mark the verse.
    let mut nstmt = conn
        .prepare(&format!(
            "SELECT verse_id,
                    CASE WHEN anchor_type = 'word' AND translation_id = ?1
                         THEN token_idx END AS mark_token,
                    SUM(CASE WHEN anchor_type = 'word' AND translation_id <> ?1
                             THEN 0 ELSE 1 END),
                    SUM(CASE WHEN anchor_type = 'word' AND translation_id <> ?1
                             THEN 1 ELSE 0 END)
               FROM note_anchors
              WHERE verse_id IN ({verse_scope})
              GROUP BY verse_id, mark_token"
        ))
        .map_err(|e| e.to_string())?;
    let note_marks = nstmt
        .query_map(rusqlite::params![translation_id, book_osis, chapter], |r| {
            Ok(NoteMark {
                verse_id: r.get(0)?,
                token_idx: r.get(1)?,
                count: r.get(2)?,
                degraded: r.get(3)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    Ok(ChapterAnnotations {
        highlights,
        note_marks,
    })
}

// ---------------------------------------------------------------------------
// Study surfaces: library, chapter notes, stats (Phase 2.5)
// ---------------------------------------------------------------------------

/// Where a note sits, resolved for display.
#[derive(Serialize)]
pub struct AnchorInfo {
    pub anchor_type: String,
    pub verse_id: String,
    pub book_osis: String,
    pub book_name: Option<String>,
    pub chapter: i64,
    pub verse: i64,
    pub translation_id: Option<i64>,
    pub token_idx: Option<i64>,
    pub surface: Option<String>,
    pub origin_abbrev: Option<String>,
    pub degraded: bool,
}

/// A note as the library and chapter list show it: `anchor` is `None` for a
/// standalone note that has no scripture reference (yet).
#[derive(Serialize)]
pub struct LibraryNote {
    pub note_id: i64,
    pub title: Option<String>,
    pub body: String,
    pub created_at: String,
    pub updated_at: String,
    pub anchor: Option<AnchorInfo>,
}

const LIBRARY_COLUMNS: &str = "SELECT n.note_id, n.title, n.body, n.created_at, n.updated_at,
            a.anchor_type, a.verse_id, a.translation_id, a.token_idx, a.surface,
            t.abbrev, b.name
       FROM notes n
       LEFT JOIN note_anchors a ON a.note_id = n.note_id
       LEFT JOIN translations t ON t.id = a.translation_id
       LEFT JOIN books b ON b.osis = substr(a.verse_id, 1, instr(a.verse_id, '.') - 1)";

fn map_library_note(r: &rusqlite::Row, active_translation_id: i64) -> rusqlite::Result<LibraryNote> {
    let anchor_type: Option<String> = r.get(5)?;
    let verse_id: Option<String> = r.get(6)?;
    let anchor = match (anchor_type, verse_id) {
        (Some(anchor_type), Some(verse_id)) => {
            let translation_id: Option<i64> = r.get(7)?;
            let (book_osis, chapter, verse) = split_verse_id(&verse_id);
            Some(AnchorInfo {
                degraded: anchor_type == "word" && translation_id != Some(active_translation_id),
                anchor_type,
                verse_id,
                book_osis,
                book_name: r.get(11)?,
                chapter,
                verse,
                translation_id,
                token_idx: r.get(8)?,
                surface: r.get(9)?,
                origin_abbrev: r.get(10)?,
            })
        }
        _ => None,
    };
    Ok(LibraryNote {
        note_id: r.get(0)?,
        title: r.get(1)?,
        body: r.get(2)?,
        created_at: r.get(3)?,
        updated_at: r.get(4)?,
        anchor,
    })
}

/// `Gen.1.5` -> `("Gen", 1, 5)`. Malformed ids degrade to zeros rather than
/// failing the whole listing.
fn split_verse_id(verse_id: &str) -> (String, i64, i64) {
    let mut parts = verse_id.split('.');
    let book = parts.next().unwrap_or_default().to_string();
    let chapter = parts.next().and_then(|c| c.parse().ok()).unwrap_or(0);
    let verse = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    (book, chapter, verse)
}

/// Every note, newest first — the notes library.
///
/// `book_osis` filters to one book; `query` matches note title or body. Notes
/// with no anchor are included unless a book filter is set.
pub fn list_all_notes(
    conn: &Connection,
    active_translation_id: i64,
    book_osis: Option<String>,
    query: Option<String>,
    limit: i64,
) -> Result<Vec<LibraryNote>, String> {
    let mut stmt = conn
        .prepare(&format!(
            "{LIBRARY_COLUMNS}
              WHERE (?1 IS NULL OR a.verse_id LIKE ?1 || '.%')
                AND (?2 IS NULL OR n.body LIKE '%' || ?2 || '%'
                     OR n.title LIKE '%' || ?2 || '%')
              ORDER BY n.created_at DESC, n.note_id DESC
              LIMIT ?3"
        ))
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![book_osis, query, limit], |r| {
            map_library_note(r, active_translation_id)
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>();
    rows.map_err(|e| e.to_string())
}

/// Notes on one chapter, in verse order — including word notes written in
/// another translation, which belong to the verse here.
pub fn list_chapter_notes(
    conn: &Connection,
    translation_id: i64,
    book_osis: &str,
    chapter: i64,
) -> Result<Vec<LibraryNote>, String> {
    let mut stmt = conn
        .prepare(&format!(
            "{LIBRARY_COLUMNS}
               JOIN verses v ON v.verse_id = a.verse_id AND v.translation_id = ?1
              WHERE v.book_osis = ?2 AND v.chapter = ?3
              ORDER BY v.verse, a.token_idx, n.created_at"
        ))
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![translation_id, book_osis, chapter], |r| {
            map_library_note(r, translation_id)
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>();
    rows.map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct RecentHighlight {
    pub id: i64,
    pub anchor_type: String,
    pub verse_id: String,
    pub book_name: Option<String>,
    pub chapter: i64,
    pub verse: i64,
    pub color: String,
    pub surface: Option<String>,
    /// Verse text in the translation being read, when it has that verse.
    pub text: Option<String>,
    pub created_at: String,
}

pub fn list_recent_highlights(
    conn: &Connection,
    translation_id: i64,
    limit: i64,
) -> Result<Vec<RecentHighlight>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT h.id, h.anchor_type, h.verse_id, b.name, h.color, h.surface,
                    v.text, h.created_at
               FROM highlights h
               LEFT JOIN books b
                 ON b.osis = substr(h.verse_id, 1, instr(h.verse_id, '.') - 1)
               LEFT JOIN verses v
                 ON v.verse_id = h.verse_id AND v.translation_id = ?1
              ORDER BY h.created_at DESC, h.id DESC
              LIMIT ?2",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![translation_id, limit], |r| {
            let verse_id: String = r.get(2)?;
            let (_, chapter, verse) = split_verse_id(&verse_id);
            Ok(RecentHighlight {
                id: r.get(0)?,
                anchor_type: r.get(1)?,
                verse_id,
                book_name: r.get(3)?,
                chapter,
                verse,
                color: r.get(4)?,
                surface: r.get(5)?,
                text: r.get(6)?,
                created_at: r.get(7)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>();
    rows.map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct Count {
    pub key: String,
    pub label: Option<String>,
    pub count: i64,
}

#[derive(Serialize)]
pub struct Stats {
    pub notes_total: i64,
    pub notes_standalone: i64,
    pub highlights_total: i64,
    pub translations_installed: i64,
    /// Distinct books carrying a note or highlight, out of 66.
    pub books_annotated: i64,
    pub by_book: Vec<Count>,
    pub by_color: Vec<Count>,
    /// Notes per day for the last 30 days; days with none are omitted.
    pub notes_by_day: Vec<Count>,
}

pub fn stats(conn: &Connection) -> Result<Stats, String> {
    let scalar = |sql: &str| -> Result<i64, String> {
        conn.query_row(sql, [], |r| r.get(0)).map_err(|e| e.to_string())
    };

    let notes_total = scalar("SELECT COUNT(*) FROM notes")?;
    let notes_standalone = scalar(
        "SELECT COUNT(*) FROM notes n
          WHERE NOT EXISTS (SELECT 1 FROM note_anchors a WHERE a.note_id = n.note_id)",
    )?;
    let highlights_total = scalar("SELECT COUNT(*) FROM highlights")?;
    let translations_installed = scalar(
        "SELECT COUNT(*) FROM translations t
          WHERE EXISTS (SELECT 1 FROM verses v WHERE v.translation_id = t.id)",
    )?;

    // Books carrying any annotation, notes and highlights pooled.
    let annotated_books = "SELECT substr(verse_id, 1, instr(verse_id, '.') - 1) AS osis
                             FROM note_anchors
                            UNION ALL
                           SELECT substr(verse_id, 1, instr(verse_id, '.') - 1)
                             FROM highlights";
    let books_annotated = scalar(&format!(
        "SELECT COUNT(DISTINCT osis) FROM ({annotated_books})"
    ))?;

    let mut stmt = conn
        .prepare(&format!(
            "SELECT x.osis, b.name, COUNT(*)
               FROM ({annotated_books}) x
               LEFT JOIN books b ON b.osis = x.osis
              GROUP BY x.osis
              ORDER BY COUNT(*) DESC, b.canonical_order
              LIMIT 8"
        ))
        .map_err(|e| e.to_string())?;
    let by_book = stmt
        .query_map([], |r| {
            Ok(Count {
                key: r.get(0)?,
                label: r.get(1)?,
                count: r.get(2)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    let mut stmt = conn
        .prepare(
            "SELECT color, COUNT(*) FROM highlights
              GROUP BY color ORDER BY COUNT(*) DESC",
        )
        .map_err(|e| e.to_string())?;
    let by_color = stmt
        .query_map([], |r| {
            Ok(Count {
                key: r.get(0)?,
                label: None,
                count: r.get(1)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    let mut stmt = conn
        .prepare(
            // Local days, not UTC ones: a note written at 23:30 belongs to the
            // day the user wrote it, not to tomorrow.
            "SELECT date(created_at, 'localtime'), COUNT(*) FROM notes
              WHERE datetime(created_at, 'localtime')
                    >= date('now', 'localtime', '-29 days')
              GROUP BY 1 ORDER BY 1",
        )
        .map_err(|e| e.to_string())?;
    let notes_by_day = stmt
        .query_map([], |r| {
            Ok(Count {
                key: r.get(0)?,
                label: None,
                count: r.get(1)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    Ok(Stats {
        notes_total,
        notes_standalone,
        highlights_total,
        translations_installed,
        books_annotated,
        by_book,
        by_color,
        notes_by_day,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory DB with one book, two translations and Gen.1.1 in each.
    fn fixture() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO books (osis, canonical_order, name, testament)
                  VALUES ('Gen', 1, 'Genesis', 'OT');
             INSERT INTO translations (id, abbrev, name) VALUES (1, 'WEB', 'World English Bible');
             INSERT INTO translations (id, abbrev, name) VALUES (2, 'KJV', 'King James Version');
             INSERT INTO verses (verse_id, translation_id, book_osis, chapter, verse, text, canonical_order)
                  VALUES ('Gen.1.1', 1, 'Gen', 1, 1, 'In the beginning', 1),
                         ('Gen.1.2', 1, 'Gen', 1, 2, 'The earth was formless', 2),
                         ('Gen.1.1', 2, 'Gen', 1, 1, 'In the beginning', 1);
             INSERT INTO tokens (translation_id, verse_id, idx, surface, char_start, char_end)
                  VALUES (1, 'Gen.1.1', 0, 'In', 0, 2),
                         (1, 'Gen.1.1', 1, 'the', 3, 6),
                         (1, 'Gen.1.1', 2, 'beginning', 7, 16);",
        )
        .unwrap();
        conn
    }

    fn word(verse_id: &str, translation_id: i64, token_idx: i64, surface: &str) -> Anchor {
        Anchor {
            anchor_type: "word".into(),
            verse_id: verse_id.into(),
            translation_id: Some(translation_id),
            token_idx: Some(token_idx),
            surface: Some(surface.into()),
        }
    }

    fn verse(verse_id: &str) -> Anchor {
        Anchor {
            anchor_type: "verse".into(),
            verse_id: verse_id.into(),
            translation_id: None,
            token_idx: None,
            surface: None,
        }
    }

    #[test]
    fn note_crud_round_trips() {
        let mut conn = fixture();
        let id = create_note(&mut conn, Some(verse("Gen.1.1")), Some("Origins".into()), "body".into())
            .unwrap();

        let notes = list_notes(&conn, verse("Gen.1.1"), 1).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].title.as_deref(), Some("Origins"));
        assert_eq!(notes[0].body, "body");

        update_note(&conn, id, None, "edited".into()).unwrap();
        let notes = list_notes(&conn, verse("Gen.1.1"), 1).unwrap();
        assert_eq!(notes[0].title, None);
        assert_eq!(notes[0].body, "edited");

        delete_note(&conn, id).unwrap();
        assert!(list_notes(&conn, verse("Gen.1.1"), 1).unwrap().is_empty());
        // The anchor went with it (ON DELETE CASCADE).
        let anchors: i64 = conn
            .query_row("SELECT COUNT(*) FROM note_anchors", [], |r| r.get(0))
            .unwrap();
        assert_eq!(anchors, 0);
    }

    #[test]
    fn verse_notes_are_translation_independent() {
        let mut conn = fixture();
        // Written while reading WEB — the translation must not be recorded.
        let anchor = Anchor {
            translation_id: Some(1),
            ..verse("Gen.1.1")
        };
        create_note(&mut conn, Some(anchor), None, "shared".into()).unwrap();

        let stored: Option<i64> = conn
            .query_row("SELECT translation_id FROM note_anchors", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, None);

        // Visible from the KJV chapter view too.
        let ann = chapter_annotations(&conn, 2, "Gen", 1).unwrap();
        assert_eq!(ann.note_marks.len(), 1);
        assert_eq!(ann.note_marks[0].token_idx, None);
    }

    #[test]
    fn word_notes_are_scoped_to_their_translation() {
        let mut conn = fixture();
        create_note(
            &mut conn,
            Some(word("Gen.1.1", 1, 2, "beginning")),
            None,
            "on the word".into(),
        )
        .unwrap();

        let notes = list_notes(&conn, word("Gen.1.1", 1, 2, "beginning"), 1).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].surface.as_deref(), Some("beginning"));
        // A different token, and a different translation, are different anchors.
        assert!(list_notes(&conn, word("Gen.1.1", 1, 1, "the"), 1)
            .unwrap()
            .is_empty());
        assert!(list_notes(&conn, word("Gen.1.1", 2, 2, "beginning"), 2)
            .unwrap()
            .is_empty());

        // In WEB it marks the word itself.
        let marks = chapter_annotations(&conn, 1, "Gen", 1).unwrap().note_marks;
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].token_idx, Some(2));
        assert_eq!((marks[0].count, marks[0].degraded), (1, 0));
    }

    #[test]
    fn word_notes_degrade_to_verse_level_in_another_translation() {
        let mut conn = fixture();
        create_note(
            &mut conn,
            Some(word("Gen.1.1", 1, 2, "beginning")),
            None,
            "on the word".into(),
        )
        .unwrap();
        create_note(&mut conn, Some(verse("Gen.1.1")), None, "on the verse".into()).unwrap();

        // Reading KJV: the WEB word note must not point at any KJV word.
        let marks = chapter_annotations(&conn, 2, "Gen", 1).unwrap().note_marks;
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].token_idx, None, "must not re-anchor to a KJV word");
        assert_eq!((marks[0].count, marks[0].degraded), (1, 1));

        // It surfaces on the verse, flagged, and still knows its word + origin.
        let notes = list_notes(&conn, verse("Gen.1.1"), 2).unwrap();
        assert_eq!(notes.len(), 2);
        let word_note = notes.iter().find(|n| n.anchor_type == "word").unwrap();
        assert!(word_note.degraded);
        assert_eq!(word_note.surface.as_deref(), Some("beginning"));
        assert_eq!(word_note.origin_abbrev.as_deref(), Some("WEB"));
        let verse_note = notes.iter().find(|n| n.anchor_type == "verse").unwrap();
        assert!(!verse_note.degraded, "verse notes are translation-independent");

        // Back in WEB it belongs to the word again, not the verse.
        let notes = list_notes(&conn, verse("Gen.1.1"), 1).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].anchor_type, "verse");
        let notes = list_notes(&conn, word("Gen.1.1", 1, 2, "beginning"), 1).unwrap();
        assert_eq!(notes.len(), 1);
        assert!(!notes[0].degraded);
    }

    #[test]
    fn removed_packs_disappear_from_the_reader_but_keep_their_row() {
        let conn = fixture();
        assert_eq!(list_translations(&conn).unwrap().len(), 2);

        conn.execute("DELETE FROM verses WHERE translation_id = 2", [])
            .unwrap();
        let ts = list_translations(&conn).unwrap();
        assert_eq!(ts.len(), 1);
        assert_eq!(ts[0].abbrev, "WEB");
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM translations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 2, "the translation row itself must survive");
    }

    #[test]
    fn library_lists_anchored_and_standalone_notes() {
        let mut conn = fixture();
        create_note(&mut conn, None, Some("Covenant".into()), "a theme".into()).unwrap();
        create_note(
            &mut conn,
            Some(word("Gen.1.1", 1, 2, "beginning")),
            None,
            "on a word".into(),
        )
        .unwrap();
        create_note(&mut conn, Some(verse("Gen.1.2")), None, "on a verse".into()).unwrap();

        let all = list_all_notes(&conn, 1, None, None, 50).unwrap();
        assert_eq!(all.len(), 3);
        // Newest first.
        assert_eq!(all[0].body, "on a verse");

        let standalone = all.iter().find(|n| n.body == "a theme").unwrap();
        assert!(standalone.anchor.is_none());
        assert_eq!(standalone.title.as_deref(), Some("Covenant"));

        let anchored = all.iter().find(|n| n.body == "on a word").unwrap();
        let anchor = anchored.anchor.as_ref().unwrap();
        assert_eq!(anchor.book_osis, "Gen");
        assert_eq!(anchor.book_name.as_deref(), Some("Genesis"));
        assert_eq!((anchor.chapter, anchor.verse), (1, 1));
        assert_eq!(anchor.surface.as_deref(), Some("beginning"));
        assert!(!anchor.degraded);

        // Same note read from KJV: still listed, now flagged.
        let from_kjv = list_all_notes(&conn, 2, None, None, 50).unwrap();
        let anchored = from_kjv.iter().find(|n| n.body == "on a word").unwrap();
        assert!(anchored.anchor.as_ref().unwrap().degraded);
    }

    #[test]
    fn library_filters_by_book_and_text() {
        let mut conn = fixture();
        create_note(&mut conn, Some(verse("Gen.1.1")), None, "creation".into()).unwrap();
        create_note(&mut conn, Some(verse("Exod.1.1")), None, "exodus note".into()).unwrap();
        create_note(&mut conn, None, None, "unanchored creation thought".into()).unwrap();

        let gen = list_all_notes(&conn, 1, Some("Gen".into()), None, 50).unwrap();
        assert_eq!(gen.len(), 1, "'Gen' must not also match 'Exod' or unanchored");
        assert_eq!(gen[0].body, "creation");

        let found = list_all_notes(&conn, 1, None, Some("creation".into()), 50).unwrap();
        assert_eq!(found.len(), 2, "matches anchored and standalone bodies");

        assert!(list_all_notes(&conn, 1, None, Some("nothing here".into()), 50)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn chapter_notes_come_back_in_verse_order() {
        let mut conn = fixture();
        create_note(&mut conn, Some(verse("Gen.1.2")), None, "second".into()).unwrap();
        create_note(&mut conn, Some(verse("Gen.1.1")), None, "first".into()).unwrap();
        create_note(
            &mut conn,
            Some(word("Gen.1.1", 1, 2, "beginning")),
            None,
            "on a word".into(),
        )
        .unwrap();
        // Another chapter must not leak in.
        create_note(&mut conn, Some(verse("Gen.2.1")), None, "elsewhere".into()).unwrap();

        let notes = list_chapter_notes(&conn, 1, "Gen", 1).unwrap();
        let bodies: Vec<&str> = notes.iter().map(|n| n.body.as_str()).collect();
        assert_eq!(bodies, vec!["first", "on a word", "second"]);

        // Reading KJV, the WEB word note still belongs to this chapter, flagged.
        let notes = list_chapter_notes(&conn, 2, "Gen", 1).unwrap();
        let word_note = notes.iter().find(|n| n.body == "on a word").unwrap();
        assert!(word_note.anchor.as_ref().unwrap().degraded);
    }

    #[test]
    fn anchors_can_be_attached_moved_and_dropped() {
        let mut conn = fixture();
        let id = create_note(&mut conn, None, None, "later".into()).unwrap();
        assert!(list_all_notes(&conn, 1, None, None, 10).unwrap()[0]
            .anchor
            .is_none());

        set_note_anchor(&mut conn, id, Some(verse("Gen.1.1"))).unwrap();
        let anchor = list_all_notes(&conn, 1, None, None, 10).unwrap()[0]
            .anchor
            .as_ref()
            .map(|a| a.verse_id.clone());
        assert_eq!(anchor.as_deref(), Some("Gen.1.1"));

        // Moving replaces rather than accumulating.
        set_note_anchor(&mut conn, id, Some(verse("Gen.1.2"))).unwrap();
        let all = list_all_notes(&conn, 1, None, None, 10).unwrap();
        assert_eq!(all.len(), 1, "one row per note, not one per old anchor");
        assert_eq!(all[0].anchor.as_ref().unwrap().verse_id, "Gen.1.2");

        set_note_anchor(&mut conn, id, None).unwrap();
        assert!(list_all_notes(&conn, 1, None, None, 10).unwrap()[0]
            .anchor
            .is_none());

        assert!(set_note_anchor(&mut conn, 999, None).is_err());
    }

    #[test]
    fn stats_summarise_notes_and_highlights() {
        let mut conn = fixture();
        create_note(&mut conn, Some(verse("Gen.1.1")), None, "a".into()).unwrap();
        create_note(&mut conn, Some(verse("Gen.1.2")), None, "b".into()).unwrap();
        create_note(&mut conn, None, None, "loose".into()).unwrap();
        set_highlight(&mut conn, verse("Gen.1.1"), "yellow".into()).unwrap();
        set_highlight(&mut conn, verse("Gen.1.2"), "yellow".into()).unwrap();
        set_highlight(&mut conn, word("Gen.1.1", 1, 0, "In"), "green".into()).unwrap();

        let s = stats(&conn).unwrap();
        assert_eq!(s.notes_total, 3);
        assert_eq!(s.notes_standalone, 1);
        assert_eq!(s.highlights_total, 3);
        assert_eq!(s.translations_installed, 2);
        assert_eq!(s.books_annotated, 1);
        assert_eq!(s.by_book[0].key, "Gen");
        assert_eq!(s.by_book[0].label.as_deref(), Some("Genesis"));
        assert_eq!(s.by_book[0].count, 5, "notes and highlights pooled");
        assert_eq!(s.by_color[0].key, "yellow");
        assert_eq!(s.by_color[0].count, 2);
        assert_eq!(s.notes_by_day.iter().map(|d| d.count).sum::<i64>(), 3);
    }

    #[test]
    fn recent_highlights_carry_their_verse_text() {
        let mut conn = fixture();
        set_highlight(&mut conn, verse("Gen.1.2"), "pink".into()).unwrap();

        let recent = list_recent_highlights(&conn, 1, 10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].book_name.as_deref(), Some("Genesis"));
        assert_eq!((recent[0].chapter, recent[0].verse), (1, 2));
        assert_eq!(recent[0].text.as_deref(), Some("The earth was formless"));

        // KJV has no Gen.1.2 in this fixture: the highlight still lists, textless.
        let recent = list_recent_highlights(&conn, 2, 10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].text, None);
    }

    #[test]
    fn settings_round_trip() {
        let conn = fixture();
        assert_eq!(get_setting(&conn, "active_translation").unwrap(), None);
        set_setting(&conn, "active_translation", "WEB").unwrap();
        set_setting(&conn, "active_translation", "KJV").unwrap();
        assert_eq!(
            get_setting(&conn, "active_translation").unwrap().as_deref(),
            Some("KJV")
        );
    }

    #[test]
    fn setting_a_highlight_replaces_the_previous_one() {
        let mut conn = fixture();
        set_highlight(&mut conn, verse("Gen.1.1"), "yellow".into()).unwrap();
        set_highlight(&mut conn, verse("Gen.1.1"), "green".into()).unwrap();

        let ann = chapter_annotations(&conn, 1, "Gen", 1).unwrap();
        assert_eq!(ann.highlights.len(), 1);
        assert_eq!(ann.highlights[0].color, "green");

        // A word highlight in the same verse is an independent anchor.
        set_highlight(&mut conn, word("Gen.1.1", 1, 2, "beginning"), "blue".into()).unwrap();
        assert_eq!(chapter_annotations(&conn, 1, "Gen", 1).unwrap().highlights.len(), 2);

        clear_highlight(&conn, verse("Gen.1.1")).unwrap();
        let ann = chapter_annotations(&conn, 1, "Gen", 1).unwrap();
        assert_eq!(ann.highlights.len(), 1);
        assert_eq!(ann.highlights[0].anchor_type, "word");
    }

    #[test]
    fn annotations_are_scoped_to_the_requested_chapter() {
        let mut conn = fixture();
        set_highlight(&mut conn, verse("Gen.1.2"), "pink".into()).unwrap();
        // A verse that isn't in this chapter must not leak in.
        set_highlight(&mut conn, verse("Gen.2.1"), "pink".into()).unwrap();

        let ann = chapter_annotations(&conn, 1, "Gen", 1).unwrap();
        assert_eq!(ann.highlights.len(), 1);
        assert_eq!(ann.highlights[0].verse_id, "Gen.1.2");
    }

    #[test]
    fn malformed_anchors_are_rejected() {
        let mut conn = fixture();
        let no_token = Anchor {
            token_idx: None,
            ..word("Gen.1.1", 1, 0, "In")
        };
        assert!(create_note(&mut conn, Some(no_token), None, "x".into()).is_err());

        let bad_type = Anchor {
            anchor_type: "paragraph".into(),
            ..verse("Gen.1.1")
        };
        assert!(create_note(&mut conn, Some(bad_type), None, "x".into()).is_err());
    }

    #[test]
    fn updating_or_deleting_a_missing_note_errors() {
        let conn = fixture();
        assert!(update_note(&conn, 999, None, "x".into()).is_err());
        assert!(delete_note(&conn, 999).is_err());
    }
}
