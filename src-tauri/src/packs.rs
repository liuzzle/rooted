//! Translation packs: which texts are available, and installing one into the
//! canonical BCV + token model.
//!
//! Only freely distributable texts live in the registry. Copyrighted
//! translations are deliberately absent — they arrive later through licensed
//! APIs or modules the user already owns.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// Canonical 66-book order: (OSIS code, display name, testament). Also the
/// fetch order — getbible numbers books 1..66 to match.
pub const BOOKS: [(&str, &str, &str); 66] = [
    ("Gen", "Genesis", "OT"),
    ("Exod", "Exodus", "OT"),
    ("Lev", "Leviticus", "OT"),
    ("Num", "Numbers", "OT"),
    ("Deut", "Deuteronomy", "OT"),
    ("Josh", "Joshua", "OT"),
    ("Judg", "Judges", "OT"),
    ("Ruth", "Ruth", "OT"),
    ("1Sam", "1 Samuel", "OT"),
    ("2Sam", "2 Samuel", "OT"),
    ("1Kgs", "1 Kings", "OT"),
    ("2Kgs", "2 Kings", "OT"),
    ("1Chr", "1 Chronicles", "OT"),
    ("2Chr", "2 Chronicles", "OT"),
    ("Ezra", "Ezra", "OT"),
    ("Neh", "Nehemiah", "OT"),
    ("Esth", "Esther", "OT"),
    ("Job", "Job", "OT"),
    ("Ps", "Psalms", "OT"),
    ("Prov", "Proverbs", "OT"),
    ("Eccl", "Ecclesiastes", "OT"),
    ("Song", "Song of Solomon", "OT"),
    ("Isa", "Isaiah", "OT"),
    ("Jer", "Jeremiah", "OT"),
    ("Lam", "Lamentations", "OT"),
    ("Ezek", "Ezekiel", "OT"),
    ("Dan", "Daniel", "OT"),
    ("Hos", "Hosea", "OT"),
    ("Joel", "Joel", "OT"),
    ("Amos", "Amos", "OT"),
    ("Obad", "Obadiah", "OT"),
    ("Jonah", "Jonah", "OT"),
    ("Mic", "Micah", "OT"),
    ("Nah", "Nahum", "OT"),
    ("Hab", "Habakkuk", "OT"),
    ("Zeph", "Zephaniah", "OT"),
    ("Hag", "Haggai", "OT"),
    ("Zech", "Zechariah", "OT"),
    ("Mal", "Malachi", "OT"),
    ("Matt", "Matthew", "NT"),
    ("Mark", "Mark", "NT"),
    ("Luke", "Luke", "NT"),
    ("John", "John", "NT"),
    ("Acts", "Acts", "NT"),
    ("Rom", "Romans", "NT"),
    ("1Cor", "1 Corinthians", "NT"),
    ("2Cor", "2 Corinthians", "NT"),
    ("Gal", "Galatians", "NT"),
    ("Eph", "Ephesians", "NT"),
    ("Phil", "Philippians", "NT"),
    ("Col", "Colossians", "NT"),
    ("1Thess", "1 Thessalonians", "NT"),
    ("2Thess", "2 Thessalonians", "NT"),
    ("1Tim", "1 Timothy", "NT"),
    ("2Tim", "2 Timothy", "NT"),
    ("Titus", "Titus", "NT"),
    ("Phlm", "Philemon", "NT"),
    ("Heb", "Hebrews", "NT"),
    ("Jas", "James", "NT"),
    ("1Pet", "1 Peter", "NT"),
    ("2Pet", "2 Peter", "NT"),
    ("1John", "1 John", "NT"),
    ("2John", "2 John", "NT"),
    ("3John", "3 John", "NT"),
    ("Jude", "Jude", "NT"),
    ("Rev", "Revelation", "NT"),
];

/// One entry in the shipped pack registry.
#[derive(Deserialize, Serialize, Clone)]
pub struct PackEntry {
    pub abbrev: String,
    pub name: String,
    pub slug: String,
    pub language: String,
    pub license: String,
    pub year: String,
    pub versification: String,
    pub blurb: String,
}

#[derive(Deserialize)]
struct Registry {
    packs: Vec<PackEntry>,
}

/// A registry entry plus its local install state.
#[derive(Serialize)]
pub struct Pack {
    #[serde(flatten)]
    pub entry: PackEntry,
    pub installed: bool,
    pub translation_id: Option<i64>,
    pub verse_count: i64,
}

pub fn registry() -> Result<&'static Vec<PackEntry>, String> {
    static REGISTRY: OnceLock<Result<Vec<PackEntry>, String>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| {
            serde_json::from_str::<Registry>(include_str!("../packs/registry.json"))
                .map(|r| r.packs)
                .map_err(|e| format!("pack registry is malformed: {e}"))
        })
        .as_ref()
        .map_err(|e| e.clone())
}

pub fn find_entry(abbrev: &str) -> Result<PackEntry, String> {
    registry()?
        .iter()
        .find(|p| p.abbrev == abbrev)
        .cloned()
        .ok_or_else(|| format!("no pack '{abbrev}' in the registry"))
}

/// The registry, annotated with what is actually installed locally.
pub fn list_packs(conn: &Connection) -> Result<Vec<Pack>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT t.id, COUNT(v.verse_id)
               FROM translations t
               LEFT JOIN verses v ON v.translation_id = t.id
              WHERE t.abbrev = ?1",
        )
        .map_err(|e| e.to_string())?;

    registry()?
        .iter()
        .map(|entry| {
            let (translation_id, verse_count) = stmt
                .query_row(rusqlite::params![entry.abbrev], |r| {
                    Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, i64>(1)?))
                })
                .unwrap_or((None, 0));
            Ok(Pack {
                entry: entry.clone(),
                installed: verse_count > 0,
                translation_id,
                verse_count,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tokenization
// ---------------------------------------------------------------------------

/// Words, with internal apostrophes and hyphens kept whole ("God's",
/// "well-beloved"). Mirrors the tokenizer in `scripts/import_bible.py` so both
/// importers produce identical token indices — word anchors depend on it.
fn word_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"[^\W_]+(?:['\u{2019}\-][^\W_]+)*").unwrap())
}

/// `(idx, surface, char_start, char_end)` per word.
///
/// Offsets are in **characters**, not bytes, because the UI slices the verse
/// text with them.
pub fn tokenize(text: &str) -> Vec<(i64, String, i64, i64)> {
    let ascii = text.is_ascii();
    let byte_to_char = |byte_idx: usize| -> i64 {
        if ascii {
            byte_idx as i64
        } else {
            text[..byte_idx].chars().count() as i64
        }
    };
    word_re()
        .find_iter(text)
        .enumerate()
        .map(|(i, m)| {
            (
                i as i64,
                m.as_str().to_string(),
                byte_to_char(m.start()),
                byte_to_char(m.end()),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Install / remove
// ---------------------------------------------------------------------------

/// One book's verses as `(chapter, verse, text)`.
pub type BookVerses = Vec<(i64, i64, String)>;

/// Fetch one book of a translation from getbible.
pub async fn fetch_book(slug: &str, book_nr: usize) -> Result<BookVerses, String> {
    let url = format!("https://api.getbible.net/v2/{slug}/{book_nr}.json");
    let resp = reqwest::Client::builder()
        .user_agent("rooted-bible-import/0.1")
        .build()
        .map_err(|e| e.to_string())?
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("{url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("{url}: HTTP {}", resp.status()));
    }
    let payload: serde_json::Value = resp.json().await.map_err(|e| format!("{url}: {e}"))?;
    parse_book(&payload).ok_or_else(|| format!("{url}: unexpected payload shape"))
}

fn parse_book(payload: &serde_json::Value) -> Option<BookVerses> {
    let mut out = Vec::new();
    for ch in payload.get("chapters")?.as_array()? {
        let ch_num = as_i64(ch.get("chapter")?)?;
        for v in ch.get("verses")?.as_array()? {
            let v_num = as_i64(v.get("verse")?)?;
            let text = v.get("text").and_then(|t| t.as_str()).unwrap_or("").trim();
            out.push((ch_num, v_num, text.to_string()));
        }
    }
    Some(out)
}

/// getbible returns chapter/verse numbers as either numbers or strings.
fn as_i64(v: &serde_json::Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str()?.parse().ok())
}

/// Seed the canonical book list (idempotent) and reserve the translation row,
/// clearing any previously imported text for it. Returns the translation id,
/// which is stable across re-imports so existing word anchors keep resolving.
pub fn begin_install(conn: &Connection, entry: &PackEntry) -> Result<i64, String> {
    let mut stmt = conn
        .prepare(
            "INSERT OR IGNORE INTO books (osis, canonical_order, name, testament)
             VALUES (?1, ?2, ?3, ?4)",
        )
        .map_err(|e| e.to_string())?;
    for (i, (osis, name, testament)) in BOOKS.iter().enumerate() {
        stmt.execute(rusqlite::params![osis, i as i64 + 1, name, testament])
            .map_err(|e| e.to_string())?;
    }

    conn.execute(
        "INSERT OR IGNORE INTO translations (abbrev, name, language, license, source_type, versification)
         VALUES (?1, ?2, ?3, ?4, 'bundled', ?5)",
        rusqlite::params![
            entry.abbrev,
            entry.name,
            entry.language,
            entry.license,
            entry.versification
        ],
    )
    .map_err(|e| e.to_string())?;

    let translation_id: i64 = conn
        .query_row(
            "SELECT id FROM translations WHERE abbrev = ?1",
            rusqlite::params![entry.abbrev],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;

    clear_text(conn, translation_id)?;
    Ok(translation_id)
}

/// Insert one fetched book. `canonical` carries the running global verse
/// counter across books.
pub fn insert_book(
    conn: &mut Connection,
    translation_id: i64,
    book_osis: &str,
    verses: &BookVerses,
    canonical: &mut i64,
) -> Result<usize, String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    {
        let mut vstmt = tx
            .prepare(
                "INSERT INTO verses
                   (verse_id, translation_id, book_osis, chapter, verse, text, canonical_order)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )
            .map_err(|e| e.to_string())?;
        let mut tstmt = tx
            .prepare(
                "INSERT INTO tokens
                   (translation_id, verse_id, idx, surface, char_start, char_end)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )
            .map_err(|e| e.to_string())?;

        for (chapter, verse, text) in verses {
            *canonical += 1;
            let verse_id = format!("{book_osis}.{chapter}.{verse}");
            vstmt
                .execute(rusqlite::params![
                    verse_id,
                    translation_id,
                    book_osis,
                    chapter,
                    verse,
                    text,
                    *canonical
                ])
                .map_err(|e| e.to_string())?;
            for (idx, surface, char_start, char_end) in tokenize(text) {
                tstmt
                    .execute(rusqlite::params![
                        translation_id,
                        verse_id,
                        idx,
                        surface,
                        char_start,
                        char_end
                    ])
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(verses.len())
}

/// Delete a pack's text. The `translations` row is deliberately kept: word
/// notes and highlights reference it, and reinstalling must land on the same
/// `translation_id` rather than orphaning them.
pub fn clear_text(conn: &Connection, translation_id: i64) -> Result<(), String> {
    conn.execute(
        "DELETE FROM tokens WHERE translation_id = ?1",
        rusqlite::params![translation_id],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "DELETE FROM verses WHERE translation_id = ?1",
        rusqlite::params![translation_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn remove_pack(conn: &Connection, abbrev: &str) -> Result<(), String> {
    let translation_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM translations WHERE abbrev = ?1",
            rusqlite::params![abbrev],
            |r| r.get(0),
        )
        .optional_id()?;
    match translation_id {
        Some(id) => clear_text(conn, id),
        None => Err(format!("'{abbrev}' is not installed")),
    }
}

/// Tiny helper so the `no rows` case isn't an error.
trait OptionalId {
    fn optional_id(self) -> Result<Option<i64>, String>;
}
impl OptionalId for rusqlite::Result<i64> {
    fn optional_id(self) -> Result<Option<i64>, String> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[test]
    fn registry_is_valid_and_unique() {
        let packs = registry().unwrap();
        assert!(packs.iter().any(|p| p.abbrev == "KJV"));
        assert!(packs.iter().any(|p| p.abbrev == "ASV"));
        let mut abbrevs: Vec<_> = packs.iter().map(|p| p.abbrev.as_str()).collect();
        abbrevs.sort_unstable();
        let count = abbrevs.len();
        abbrevs.dedup();
        assert_eq!(abbrevs.len(), count, "duplicate abbrev in registry");
    }

    #[test]
    fn tokenizer_keeps_words_whole_and_offsets_slice_back() {
        let text = "God's well-beloved Son, in whom I am well pleased.";
        let tokens = tokenize(text);
        let surfaces: Vec<&str> = tokens.iter().map(|t| t.1.as_str()).collect();
        assert_eq!(surfaces[0], "God's");
        assert_eq!(surfaces[1], "well-beloved");
        // Every offset pair slices back to exactly the recorded surface.
        let chars: Vec<char> = text.chars().collect();
        for (_, surface, start, end) in &tokens {
            let slice: String = chars[*start as usize..*end as usize].iter().collect();
            assert_eq!(&slice, surface);
        }
    }

    #[test]
    fn tokenizer_offsets_are_characters_not_bytes() {
        // Curly apostrophe is 3 bytes but 1 character.
        let text = "the Lord\u{2019}s word";
        let tokens = tokenize(text);
        let chars: Vec<char> = text.chars().collect();
        for (_, surface, start, end) in &tokens {
            let slice: String = chars[*start as usize..*end as usize].iter().collect();
            assert_eq!(&slice, surface);
        }
        assert_eq!(tokens.last().unwrap().1, "word");
    }

    #[test]
    fn parses_getbible_payloads_with_string_or_number_fields() {
        let payload = serde_json::json!({
            "chapters": [
                {"chapter": 1, "verses": [{"verse": 1, "text": " In the beginning "}]},
                {"chapter": "2", "verses": [{"verse": "3", "text": "Second"}]}
            ]
        });
        let verses = parse_book(&payload).unwrap();
        assert_eq!(verses[0], (1, 1, "In the beginning".to_string()));
        assert_eq!(verses[1], (2, 3, "Second".to_string()));
    }

    /// The Rust importer must tokenize exactly like `scripts/import_bible.py`,
    /// or a re-import would shift token indices and move every word anchor.
    /// Compares against text already imported by the Python script; skipped
    /// when there is no local database.
    #[test]
    fn matches_the_python_importer_on_installed_text() {
        let path = db::resolve_db_path();
        if !path.exists() {
            eprintln!("skipping: no database at {}", path.display());
            return;
        }
        let conn = Connection::open(&path).unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT v.translation_id, v.verse_id, v.text FROM verses v
                  ORDER BY v.canonical_order LIMIT 2000",
            )
            .unwrap();
        let verses: Vec<(i64, String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        if verses.is_empty() {
            eprintln!("skipping: database has no verses");
            return;
        }

        let mut tstmt = conn
            .prepare(
                "SELECT idx, surface, char_start, char_end FROM tokens
                  WHERE translation_id = ?1 AND verse_id = ?2 ORDER BY idx",
            )
            .unwrap();
        let mut compared = 0;
        for (tid, verse_id, text) in &verses {
            let python: Vec<(i64, String, i64, i64)> = tstmt
                .query_map(rusqlite::params![tid, verse_id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })
                .unwrap()
                .map(Result::unwrap)
                .collect();
            if python.is_empty() {
                continue;
            }
            assert_eq!(tokenize(text), python, "tokenization differs at {verse_id}");
            compared += 1;
        }
        assert!(compared > 0, "no tokens to compare against");
    }

    /// Hits the network; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn downloads_and_imports_a_real_book() {
        let mut conn = Connection::open_in_memory().unwrap();
        db::apply_schema(&conn).unwrap();
        let entry = find_entry("KJV").unwrap();
        let tid = begin_install(&conn, &entry).unwrap();

        let jude = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(fetch_book(&entry.slug, 65))
            .unwrap();
        let mut canonical = 0;
        insert_book(&mut conn, tid, "Jude", &jude, &mut canonical).unwrap();

        assert_eq!(jude.len(), 25, "Jude has 25 verses");
        let text: String = conn
            .query_row(
                "SELECT text FROM verses WHERE translation_id = ?1 AND verse_id = 'Jude.1.1'",
                rusqlite::params![tid],
                |r| r.get(0),
            )
            .unwrap();
        assert!(text.contains("Jude"), "unexpected text: {text}");
        let tokens: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tokens WHERE translation_id = ?1",
                rusqlite::params![tid],
                |r| r.get(0),
            )
            .unwrap();
        assert!(tokens > 400, "only {tokens} tokens");
    }

    #[test]
    fn install_reuses_the_translation_id_after_removal() {
        let mut conn = Connection::open_in_memory().unwrap();
        db::apply_schema(&conn).unwrap();
        let entry = find_entry("KJV").unwrap();

        let first = begin_install(&conn, &entry).unwrap();
        let mut canonical = 0;
        insert_book(
            &mut conn,
            first,
            "Gen",
            &vec![(1, 1, "In the beginning".to_string())],
            &mut canonical,
        )
        .unwrap();
        assert!(list_packs(&conn)
            .unwrap()
            .iter()
            .any(|p| p.entry.abbrev == "KJV" && p.installed));

        remove_pack(&conn, "KJV").unwrap();
        let packs = list_packs(&conn).unwrap();
        let kjv = packs.iter().find(|p| p.entry.abbrev == "KJV").unwrap();
        assert!(!kjv.installed);
        assert_eq!(kjv.verse_count, 0);
        // Row kept, so word anchors written against KJV still resolve.
        assert_eq!(kjv.translation_id, Some(first));

        let second = begin_install(&conn, &entry).unwrap();
        assert_eq!(first, second);
    }
}
