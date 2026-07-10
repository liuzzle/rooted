use rusqlite::Connection;
use serde::Serialize;
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
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(|e| e.to_string())?;
    conn.execute_batch(include_str!("../migrations/0001_init.sql"))
        .map_err(|e| e.to_string())?;
    Ok(conn)
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

pub fn list_translations(conn: &Connection) -> Result<Vec<Translation>, String> {
    let mut stmt = conn
        .prepare("SELECT id, abbrev, name, language FROM translations ORDER BY abbrev")
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
