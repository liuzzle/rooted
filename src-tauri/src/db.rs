use rusqlite::Connection;
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

/// Enable foreign keys and apply the canonical schema (idempotent).
pub fn apply_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(|e| e.to_string())?;
    conn.execute_batch(include_str!("../migrations/0001_init.sql"))
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
    pub token_idx: Option<i64>, // None = verse-level note
    pub count: i64,
}

#[derive(Serialize)]
pub struct ChapterAnnotations {
    pub highlights: Vec<Highlight>,
    pub note_marks: Vec<NoteMark>,
}

/// Create a note and its anchor in one transaction. Returns the new note id.
pub fn create_note(
    conn: &mut Connection,
    anchor: Anchor,
    title: Option<String>,
    body: String,
) -> Result<i64, String> {
    let anchor = anchor.normalized()?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO notes (title, body) VALUES (?1, ?2)",
        rusqlite::params![title, body],
    )
    .map_err(|e| e.to_string())?;
    let note_id = tx.last_insert_rowid();
    tx.execute(
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
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(note_id)
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

/// Notes attached to one anchor, newest last.
pub fn list_notes(conn: &Connection, anchor: Anchor) -> Result<Vec<Note>, String> {
    let anchor = anchor.normalized()?;
    let sql = if anchor.is_word() {
        "SELECT n.note_id, n.title, n.body, n.created_at, n.updated_at,
                a.anchor_type, a.verse_id, a.translation_id, a.token_idx, a.surface
           FROM notes n JOIN note_anchors a ON a.note_id = n.note_id
          WHERE a.anchor_type = 'word' AND a.verse_id = ?1
            AND a.translation_id = ?2 AND a.token_idx = ?3
          ORDER BY n.created_at, n.note_id"
    } else {
        "SELECT n.note_id, n.title, n.body, n.created_at, n.updated_at,
                a.anchor_type, a.verse_id, a.translation_id, a.token_idx, a.surface
           FROM notes n JOIN note_anchors a ON a.note_id = n.note_id
          WHERE a.anchor_type = 'verse' AND a.verse_id = ?1
          ORDER BY n.created_at, n.note_id"
    };

    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let map_row = |r: &rusqlite::Row| {
        Ok(Note {
            note_id: r.get(0)?,
            title: r.get(1)?,
            body: r.get(2)?,
            created_at: r.get(3)?,
            updated_at: r.get(4)?,
            anchor_type: r.get(5)?,
            verse_id: r.get(6)?,
            translation_id: r.get(7)?,
            token_idx: r.get(8)?,
            surface: r.get(9)?,
        })
    };

    let rows = if anchor.is_word() {
        stmt.query_map(
            rusqlite::params![anchor.verse_id, anchor.translation_id, anchor.token_idx],
            map_row,
        )
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
    } else {
        stmt.query_map(rusqlite::params![anchor.verse_id], map_row)
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
    };
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

    let mut nstmt = conn
        .prepare(&format!(
            "SELECT verse_id, token_idx, COUNT(*)
               FROM note_anchors
              WHERE verse_id IN ({verse_scope})
                AND (anchor_type = 'verse' OR translation_id = ?1)
              GROUP BY verse_id, token_idx"
        ))
        .map_err(|e| e.to_string())?;
    let note_marks = nstmt
        .query_map(rusqlite::params![translation_id, book_osis, chapter], |r| {
            Ok(NoteMark {
                verse_id: r.get(0)?,
                token_idx: r.get(1)?,
                count: r.get(2)?,
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
        let id = create_note(&mut conn, verse("Gen.1.1"), Some("Origins".into()), "body".into())
            .unwrap();

        let notes = list_notes(&conn, verse("Gen.1.1")).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].title.as_deref(), Some("Origins"));
        assert_eq!(notes[0].body, "body");

        update_note(&conn, id, None, "edited".into()).unwrap();
        let notes = list_notes(&conn, verse("Gen.1.1")).unwrap();
        assert_eq!(notes[0].title, None);
        assert_eq!(notes[0].body, "edited");

        delete_note(&conn, id).unwrap();
        assert!(list_notes(&conn, verse("Gen.1.1")).unwrap().is_empty());
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
        create_note(&mut conn, anchor, None, "shared".into()).unwrap();

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
            word("Gen.1.1", 1, 2, "beginning"),
            None,
            "on the word".into(),
        )
        .unwrap();

        let notes = list_notes(&conn, word("Gen.1.1", 1, 2, "beginning")).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].surface.as_deref(), Some("beginning"));
        // A different token, and a different translation, are different anchors.
        assert!(list_notes(&conn, word("Gen.1.1", 1, 1, "the"))
            .unwrap()
            .is_empty());
        assert!(list_notes(&conn, word("Gen.1.1", 2, 2, "beginning"))
            .unwrap()
            .is_empty());

        assert_eq!(chapter_annotations(&conn, 1, "Gen", 1).unwrap().note_marks.len(), 1);
        assert!(chapter_annotations(&conn, 2, "Gen", 1)
            .unwrap()
            .note_marks
            .is_empty());
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
        assert!(create_note(&mut conn, no_token, None, "x".into()).is_err());

        let bad_type = Anchor {
            anchor_type: "paragraph".into(),
            ..verse("Gen.1.1")
        };
        assert!(create_note(&mut conn, bad_type, None, "x".into()).is_err());
    }

    #[test]
    fn updating_or_deleting_a_missing_note_errors() {
        let conn = fixture();
        assert!(update_note(&conn, 999, None, "x".into()).is_err());
        assert!(delete_note(&conn, 999).is_err());
    }
}
