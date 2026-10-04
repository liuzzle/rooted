//! Decisions about the graph that only a person can make.
//!
//! Two kinds, both offered and never applied on their own:
//!
//! * **Two topics are one.** Suggested when their labels are close in meaning;
//!   merged only when a person says so. A merge moves every recorded mention —
//!   each still pointing at the words written — and leaves an alias, so the
//!   spelling keeps joining the surviving topic when notes are read again.
//! * **A verse belongs with a note.** Suggested when the verse is close in
//!   meaning to a passage of the note that doesn't already cite it. Accepting
//!   records the link; dismissing makes sure it isn't offered again.
//!
//! Calibrated on the real corpus, and honestly weak: label similarity catches
//! variants and near-synonyms ("Opfer"/"Opfergabe" 0.78, "Sünde"/"Schuld"
//! 0.67) but not paraphrase ("Gnade"/"unverdiente Gunst" 0.49, below
//! "Abraham"/"Isaak"). Passage-to-verse similarity finds good verses and poor
//! ones at the same scores. That is exactly why these are suggestions: the
//! person is the gate, and a manual merge covers what similarity can't.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use crate::search::{dot, unpack, VectorCache};

/// Labels at least this close are offered as possibly the same topic.
///
/// Measured on the real topics: above 0.70 the pairs were real questions
/// ("Herr" / "Herr Jesus", "Heilige" / "Heilige Geist", "Lieblichkeiten" /
/// "Liebliche Örter"); between 0.62 and 0.70 they were mostly a shared word
/// ("Gott" / "Willen Gottes", "Gott" / "Haus Gottes") or nothing ("Mensch" /
/// "Leben"). Near-synonyms just below ("Sünde" / "Schuld" 0.67) are left to a
/// manual merge, which exists for exactly what similarity can't see.
pub const MERGE_FLOOR: f32 = 0.70;

/// Verses at least this close to a passage are offered for its note. Below it,
/// over a whole translation, nothing was useful; above it, about half was.
pub const SUGGEST_FLOOR: f32 = 0.58;
/// At most this many verses offered per note at once.
const SUGGEST_LIMIT: usize = 5;
/// At most this many for any one passage. Without it, one passage that is
/// close to everything — "Das innere Leben vor Gottes Angesicht" took all five
/// — crowds out the passage that has the right verse (V5 → Psalm 16:5).
const PER_PASSAGE: usize = 2;

// --- merging ---------------------------------------------------------------

/// Merge `from` into `into`: every mention moves, `from`'s spelling becomes an
/// alias of `into`, and `from` is gone. One transaction, so a failure leaves
/// both topics exactly as they were.
pub fn merge_concepts(conn: &mut Connection, from: i64, into: i64) -> Result<(), String> {
    if from == into {
        return Err("a topic can't be merged into itself".into());
    }
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let (key, label): (String, String) = tx
        .query_row(
            "SELECT key, label FROM concepts WHERE concept_id = ?1",
            [from],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no topic {from}"))?;
    let exists: bool = tx
        .query_row("SELECT 1 FROM concepts WHERE concept_id = ?1", [into], |_| Ok(true))
        .optional()
        .map_err(|e| e.to_string())?
        .unwrap_or(false);
    if !exists {
        return Err(format!("no topic {into}"));
    }

    // A span both topics already claim keeps the surviving one's mention.
    tx.execute(
        "UPDATE OR IGNORE concept_mentions SET concept_id = ?2 WHERE concept_id = ?1",
        [from, into],
    )
    .map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM concept_mentions WHERE concept_id = ?1", [from])
        .map_err(|e| e.to_string())?;
    // Spellings merged into `from` earlier now belong to `into` as well.
    tx.execute(
        "UPDATE concept_aliases SET concept_id = ?2 WHERE concept_id = ?1",
        [from, into],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT OR REPLACE INTO concept_aliases (key, concept_id, label) VALUES (?1, ?2, ?3)",
        rusqlite::params![key, into, label],
    )
    .map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM concepts WHERE concept_id = ?1", [from])
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

/// Undo a merge. The alias goes, and every note citing the surviving topic is
/// marked for re-reading: the worker then files that spelling under its own
/// topic again, from the text, rather than this guessing which mentions were
/// whose.
pub fn unmerge(conn: &mut Connection, key: &str) -> Result<(), String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let concept_id: i64 = tx
        .query_row(
            "SELECT concept_id FROM concept_aliases WHERE key = ?1",
            [key],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no merged spelling {key:?}"))?;
    tx.execute("DELETE FROM concept_aliases WHERE key = ?1", [key])
        .map_err(|e| e.to_string())?;
    tx.execute(
        "UPDATE sources SET scheme = NULL
          WHERE source_id IN (SELECT ch.source_id FROM concept_mentions m
                                JOIN chunks ch ON ch.chunk_id = m.chunk_id
                               WHERE m.concept_id = ?1)",
        [concept_id],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

/// "Not the same" — never suggest this pair again.
pub fn mark_distinct(conn: &Connection, a: i64, b: i64) -> Result<(), String> {
    conn.execute(
        "INSERT OR IGNORE INTO concept_distinct (a, b) VALUES (?1, ?2)",
        [a.min(b), a.max(b)],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[derive(Serialize, Debug)]
pub struct Alias {
    pub key: String,
    pub label: String,
    pub merged_at: String,
}

/// The spellings merged into a topic, newest first.
pub fn aliases(conn: &Connection, concept_id: i64) -> Result<Vec<Alias>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT key, label, merged_at FROM concept_aliases
              WHERE concept_id = ?1 ORDER BY merged_at DESC, label",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([concept_id], |r| {
            Ok(Alias {
                key: r.get(0)?,
                label: r.get(1)?,
                merged_at: r.get(2)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

/// One side of a suggested merge: the topic, and one passage that says it.
#[derive(Serialize, Debug)]
pub struct MergeSide {
    pub concept_id: i64,
    pub label: String,
    pub notes: i64,
    pub mentions: i64,
    pub sample: Option<String>,
    pub sample_marks: Vec<(i64, i64)>,
}

#[derive(Serialize, Debug)]
pub struct MergeSuggestion {
    pub a: MergeSide,
    pub b: MergeSide,
    pub score: f32,
}

/// Pairs of topics whose labels are close in meaning, closest first, leaving
/// out pairs already judged distinct. Only topics something still cites.
pub fn merge_suggestions(
    conn: &Connection,
    model: &str,
    floor: f32,
    limit: usize,
) -> Result<Vec<MergeSuggestion>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT c.concept_id, c.embedding FROM concepts c
              WHERE c.embedded_by = ?1 AND c.embedding IS NOT NULL
                AND EXISTS (SELECT 1 FROM concept_mentions m WHERE m.concept_id = c.concept_id)",
        )
        .map_err(|e| e.to_string())?;
    let topics: Vec<(i64, Vec<f32>)> = stmt
        .query_map([model], |r| Ok((r.get::<_, i64>(0)?, unpack(&r.get::<_, Vec<u8>>(1)?))))
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare("SELECT a, b FROM concept_distinct")
        .map_err(|e| e.to_string())?;
    let distinct: HashSet<(i64, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;

    let mut pairs = Vec::new();
    for i in 0..topics.len() {
        for j in i + 1..topics.len() {
            let (a, va) = &topics[i];
            let (b, vb) = &topics[j];
            if va.len() != vb.len() || distinct.contains(&(*a.min(b), *a.max(b))) {
                continue;
            }
            let score = dot(va, vb);
            if score >= floor {
                pairs.push((*a, *b, score));
            }
        }
    }
    pairs.sort_by(|x, y| y.2.total_cmp(&x.2));
    pairs.truncate(limit);
    pairs
        .into_iter()
        .map(|(a, b, score)| {
            Ok(MergeSuggestion {
                a: side(conn, a)?,
                b: side(conn, b)?,
                score,
            })
        })
        .collect()
}

fn side(conn: &Connection, concept_id: i64) -> Result<MergeSide, String> {
    let (label, notes, mentions) = conn
        .query_row(
            "SELECT c.label, COUNT(DISTINCT s.note_id), COUNT(m.mention_id)
               FROM concepts c
               JOIN concept_mentions m ON m.concept_id = c.concept_id
               JOIN chunks ch ON ch.chunk_id = m.chunk_id
               JOIN sources s ON s.source_id = ch.source_id
              WHERE c.concept_id = ?1",
            [concept_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)),
        )
        .map_err(|e| e.to_string())?;
    let sample = conn
        .query_row(
            "SELECT ch.text, m.char_start, m.char_end FROM concept_mentions m
               JOIN chunks ch ON ch.chunk_id = m.chunk_id
              WHERE m.concept_id = ?1 ORDER BY m.mention_id LIMIT 1",
            [concept_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    Ok(MergeSide {
        concept_id,
        label,
        notes,
        mentions,
        sample_marks: sample.as_ref().map(|(_, s, e)| vec![(*s, *e)]).unwrap_or_default(),
        sample: sample.map(|(text, _, _)| text),
    })
}

// --- verse suggestions -----------------------------------------------------

#[derive(Serialize, Debug)]
pub struct NoteVerse {
    pub verse_id: String,
    pub book_osis: String,
    pub book_name: String,
    pub chapter: i64,
    pub verse: i64,
    /// The verse in the translation being read; `None` if it lacks the verse.
    pub text: Option<String>,
    pub score: Option<f32>,
    /// The passage of the note it is close to — the reason it's offered.
    pub passage: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct NoteVerses {
    pub suggested: Vec<NoteVerse>,
    pub accepted: Vec<NoteVerse>,
}

/// Verses offered for a note, and the ones already accepted.
///
/// Offered: close in meaning to one of the note's passages, not cited by the
/// note, not already decided. Only computed when `model` is given — it means
/// comparing every passage with every verse, so the library asks for it when
/// a person does, not for every note it lists. `None` returns the accepted ones.
pub fn verses_for_note(
    conn: &Connection,
    note_id: i64,
    translation_id: i64,
    model: Option<&str>,
    cache: &VectorCache,
) -> Result<NoteVerses, String> {
    let decided: HashMap<String, (String, Option<f32>)> = {
        let mut stmt = conn
            .prepare("SELECT verse_id, status, score FROM verse_suggestions WHERE note_id = ?1")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([note_id], |r| {
                Ok((r.get::<_, String>(0)?, (r.get::<_, String>(1)?, r.get::<_, Option<f32>>(2)?)))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
    };

    let mut accepted = Vec::new();
    for (verse_id, (status, score)) in &decided {
        if status == "accepted" {
            if let Some(v) = describe(conn, translation_id, verse_id, *score, None)? {
                accepted.push(v);
            }
        }
    }
    accepted.sort_by(|a, b| (&a.book_osis, a.chapter, a.verse).cmp(&(&b.book_osis, b.chapter, b.verse)));

    let Some(model) = model else {
        return Ok(NoteVerses { suggested: Vec::new(), accepted });
    };

    // What the note already cites, verse by verse, so it isn't offered back.
    let mut cited: HashSet<String> = HashSet::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT v.book_osis, v.chapter, v.verse, v.verse_end FROM verse_links v
                   JOIN chunks ch ON ch.chunk_id = v.chunk_id
                   JOIN sources s ON s.source_id = ch.source_id
                  WHERE s.note_id = ?1 AND v.verse IS NOT NULL",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([note_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, Option<i64>>(3)?))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (book, chapter, verse, end) = row.map_err(|e| e.to_string())?;
            for v in verse..=end.unwrap_or(verse).max(verse) {
                cited.insert(format!("{book}.{chapter}.{v}"));
            }
        }
    }

    let chunks: Vec<(String, Vec<f32>)> = {
        let mut stmt = conn
            .prepare(
                "SELECT ch.text, ch.embedding FROM chunks ch
                   JOIN sources s ON s.source_id = ch.source_id
                  WHERE s.note_id = ?1 AND ch.embedded_by = ?2 AND ch.embedding IS NOT NULL",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params![note_id, model], |r| {
                Ok((r.get::<_, String>(0)?, unpack(&r.get::<_, Vec<u8>>(1)?)))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
    };

    // Each verse's best passage: a verse close to two passages is offered once,
    // with the closer one as its reason.
    let mut best: HashMap<String, (f32, usize)> = HashMap::new();
    for (i, (_, vector)) in chunks.iter().enumerate() {
        for (verse_id, score) in cache.nearest(conn, translation_id, model, vector, SUGGEST_FLOOR)? {
            if cited.contains(&verse_id) || decided.contains_key(&verse_id) {
                continue;
            }
            let entry = best.entry(verse_id).or_insert((score, i));
            if score > entry.0 {
                *entry = (score, i);
            }
        }
    }
    let mut ranked: Vec<(String, f32, usize)> =
        best.into_iter().map(|(id, (s, i))| (id, s, i)).collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut per_passage: HashMap<usize, usize> = HashMap::new();
    ranked.retain(|(_, _, chunk)| {
        let n = per_passage.entry(*chunk).or_default();
        *n += 1;
        *n <= PER_PASSAGE
    });
    ranked.truncate(SUGGEST_LIMIT);

    let mut suggested = Vec::new();
    for (verse_id, score, chunk) in ranked {
        if let Some(v) = describe(conn, translation_id, &verse_id, Some(score), Some(chunks[chunk].0.clone()))? {
            suggested.push(v);
        }
    }
    Ok(NoteVerses { suggested, accepted })
}

fn describe(
    conn: &Connection,
    translation_id: i64,
    verse_id: &str,
    score: Option<f32>,
    passage: Option<String>,
) -> Result<Option<NoteVerse>, String> {
    let mut parts = verse_id.split('.');
    let (Some(book), Some(chapter), Some(verse)) = (parts.next(), parts.next(), parts.next()) else {
        return Ok(None);
    };
    let (Ok(chapter), Ok(verse)) = (chapter.parse::<i64>(), verse.parse::<i64>()) else {
        return Ok(None);
    };
    let book_name: String = conn
        .query_row("SELECT name FROM books WHERE osis = ?1", [book], |r| r.get(0))
        .optional()
        .map_err(|e| e.to_string())?
        .unwrap_or_else(|| book.to_string());
    let text: Option<String> = conn
        .query_row(
            "SELECT text FROM verses WHERE translation_id = ?1 AND verse_id = ?2",
            rusqlite::params![translation_id, verse_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    Ok(Some(NoteVerse {
        verse_id: verse_id.to_string(),
        book_osis: book.to_string(),
        book_name,
        chapter,
        verse,
        text,
        score,
        passage,
    }))
}

/// Accept or dismiss a suggested verse. `None` forgets the decision, so an
/// accepted verse can be removed and a dismissed one offered again.
pub fn decide_verse(
    conn: &Connection,
    note_id: i64,
    verse_id: &str,
    status: Option<&str>,
    score: Option<f32>,
    model: Option<&str>,
) -> Result<(), String> {
    match status {
        None => conn.execute(
            "DELETE FROM verse_suggestions WHERE note_id = ?1 AND verse_id = ?2",
            rusqlite::params![note_id, verse_id],
        ),
        Some(s @ ("accepted" | "dismissed")) => conn.execute(
            "INSERT INTO verse_suggestions (note_id, verse_id, status, score, model)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(note_id, verse_id) DO UPDATE SET
               status = excluded.status, score = excluded.score,
               model = excluded.model, decided_at = datetime('now')",
            rusqlite::params![note_id, verse_id, s, score, model],
        ),
        Some(other) => return Err(format!("unknown decision {other:?}")),
    }
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn fixture() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db::apply_schema(&conn).unwrap();
        for (i, (osis, name, testament)) in crate::packs::BOOKS.iter().enumerate() {
            conn.execute(
                "INSERT INTO books (osis, canonical_order, name, testament) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![osis, i as i64 + 1, name, testament],
            )
            .unwrap();
        }
        conn.execute("INSERT INTO translations (id, abbrev, name) VALUES (1, 'ELB71', 'Elberfelder')", [])
            .unwrap();
        conn
    }

    fn pack(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    /// A note of one chunk, and the chunk's id.
    fn note(conn: &Connection, text: &str) -> (i64, i64) {
        conn.execute("INSERT INTO notes (title, body) VALUES ('n', ?1)", [text]).unwrap();
        let note_id = conn.last_insert_rowid();
        conn.execute("INSERT INTO sources (kind, note_id, text_hash, scheme) VALUES ('note', ?1, 'h', 'x')", [note_id])
            .unwrap();
        let source = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO chunks (source_id, idx, char_start, char_end, text) VALUES (?1, 0, 0, 1, ?2)",
            rusqlite::params![source, text],
        )
        .unwrap();
        (note_id, conn.last_insert_rowid())
    }

    fn topic(conn: &Connection, label: &str, chunk: i64, at: (i64, i64), vector: Option<&[f32]>) -> i64 {
        conn.execute(
            "INSERT INTO concepts (label, key, embedding, embedded_by) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![label, label.to_lowercase(), vector.map(pack), vector.map(|_| "m")],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO concept_mentions (concept_id, chunk_id, char_start, char_end, surface, extracted_by)
             VALUES (?1, ?2, ?3, ?4, ?5, 'phrases')",
            rusqlite::params![id, chunk, at.0, at.1, label],
        )
        .unwrap();
        id
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn a_merge_moves_the_evidence_and_remembers_the_spelling() {
        let mut conn = fixture();
        let (_, chunk) = note(&conn, "Opfer und Opfergabe");
        let opfer = topic(&conn, "Opfer", chunk, (0, 5), None);
        let gabe = topic(&conn, "Opfergabe", chunk, (10, 19), None);

        merge_concepts(&mut conn, gabe, opfer).unwrap();

        // Both mentions now belong to the survivor, still at their own offsets.
        let mut stmt = conn
            .prepare("SELECT concept_id, char_start, surface FROM concept_mentions ORDER BY char_start")
            .unwrap();
        let rows: Vec<(i64, i64, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(rows, vec![(opfer, 0, "Opfer".into()), (opfer, 10, "Opfergabe".into())]);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM concepts"), 1);
        let aliases = aliases(&conn, opfer).unwrap();
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases[0].label, "Opfergabe");
        assert_eq!(aliases[0].key, "opfergabe");
    }

    #[test]
    fn merging_twice_keeps_every_spelling_with_the_survivor() {
        let mut conn = fixture();
        let (_, chunk) = note(&conn, "Opfer, Opfergabe, Gabe");
        let a = topic(&conn, "Opfer", chunk, (0, 5), None);
        let b = topic(&conn, "Opfergabe", chunk, (7, 16), None);
        let c = topic(&conn, "Gabe", chunk, (18, 22), None);

        merge_concepts(&mut conn, c, b).unwrap();
        merge_concepts(&mut conn, b, a).unwrap();

        let labels: Vec<String> = aliases(&conn, a).unwrap().into_iter().map(|x| x.label).collect();
        assert_eq!(labels.len(), 2);
        assert!(labels.contains(&"Gabe".to_string()) && labels.contains(&"Opfergabe".to_string()));
    }

    #[test]
    fn a_failed_merge_changes_nothing() {
        let mut conn = fixture();
        let (_, chunk) = note(&conn, "Opfer");
        let a = topic(&conn, "Opfer", chunk, (0, 5), None);

        assert!(merge_concepts(&mut conn, a, 999).is_err());
        assert!(merge_concepts(&mut conn, a, a).is_err());
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM concept_mentions WHERE concept_id = 1"), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM concept_aliases"), 0);
    }

    #[test]
    fn undoing_a_merge_has_the_notes_read_again() {
        let mut conn = fixture();
        let (_, chunk) = note(&conn, "Opfer und Opfergabe");
        let opfer = topic(&conn, "Opfer", chunk, (0, 5), None);
        let gabe = topic(&conn, "Opfergabe", chunk, (10, 19), None);
        merge_concepts(&mut conn, gabe, opfer).unwrap();

        unmerge(&mut conn, "opfergabe").unwrap();

        assert_eq!(count(&conn, "SELECT COUNT(*) FROM concept_aliases"), 0);
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM sources WHERE scheme IS NULL"),
            1,
            "the worker re-reads the note and brings the spelling back as its own topic"
        );
    }

    #[test]
    fn close_labels_are_suggested_and_a_no_is_remembered() {
        let conn = fixture();
        let (_, chunk) = note(&conn, "Opfer Opfergabe Abraham");
        let a = topic(&conn, "Opfer", chunk, (0, 5), Some(&[1.0, 0.0]));
        let b = topic(&conn, "Opfergabe", chunk, (6, 15), Some(&[0.8, 0.6]));
        topic(&conn, "Abraham", chunk, (16, 23), Some(&[0.0, 1.0]));

        let found = merge_suggestions(&conn, "m", MERGE_FLOOR, 10).unwrap();
        assert_eq!(found.len(), 1, "Abraham is close to neither");
        assert_eq!((found[0].a.label.as_str(), found[0].b.label.as_str()), ("Opfer", "Opfergabe"));
        assert_eq!(found[0].a.sample.as_deref(), Some("Opfer Opfergabe Abraham"));

        mark_distinct(&conn, b, a).unwrap();
        assert!(merge_suggestions(&conn, "m", MERGE_FLOOR, 10).unwrap().is_empty());
    }

    fn verse(conn: &Connection, id: &str, text: &str, vector: &[f32]) {
        let p: Vec<&str> = id.split('.').collect();
        conn.execute(
            "INSERT INTO verses (verse_id, translation_id, book_osis, chapter, verse, text, canonical_order)
             VALUES (?1, 1, ?2, ?3, ?4, ?5, 0)",
            rusqlite::params![id, p[0], p[1], p[2], text],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO verse_vectors VALUES (1, ?1, 'm', ?2)",
            rusqlite::params![id, pack(vector)],
        )
        .unwrap();
    }

    fn embedded_note(conn: &Connection, text: &str, vector: &[f32]) -> (i64, i64) {
        let (note_id, chunk) = note(conn, text);
        conn.execute(
            "UPDATE chunks SET embedding = ?1, embedded_by = 'm' WHERE chunk_id = ?2",
            rusqlite::params![pack(vector), chunk],
        )
        .unwrap();
        (note_id, chunk)
    }

    #[test]
    fn a_close_verse_the_note_doesnt_cite_is_offered_with_its_reason() {
        let conn = fixture();
        verse(&conn, "Ps.16.5", "Jehova ist das Teil meines Erbes.", &[1.0, 0.0]);
        verse(&conn, "Ps.16.1", "Bewahre mich, Gott.", &[0.95, 0.312]);
        verse(&conn, "Gen.1.1", "Im Anfang.", &[0.0, 1.0]);
        let (note_id, chunk) = embedded_note(&conn, "Das Los und das Erbe, siehe Ps 16,1", &[1.0, 0.0]);
        conn.execute(
            "INSERT INTO verse_links (chunk_id, char_start, char_end, surface, book_osis, chapter, verse, verse_id)
             VALUES (?1, 27, 35, 'Ps 16,1', 'Ps', 16, 1, 'Ps.16.1')",
            [chunk],
        )
        .unwrap();

        let found = verses_for_note(&conn, note_id, 1, Some("m"), &VectorCache::default()).unwrap();

        let ids: Vec<&str> = found.suggested.iter().map(|v| v.verse_id.as_str()).collect();
        assert_eq!(ids, vec!["Ps.16.5"], "the cited verse and the unrelated one are left out");
        assert_eq!(found.suggested[0].passage.as_deref(), Some("Das Los und das Erbe, siehe Ps 16,1"));
        assert_eq!(found.suggested[0].book_name, "Psalms");
    }

    #[test]
    fn one_passage_cannot_take_every_suggestion() {
        let conn = fixture();
        for (i, v) in [[1.0f32, 0.0], [0.99, 0.141], [0.98, 0.199], [0.97, 0.243]].iter().enumerate() {
            verse(&conn, &format!("Ps.1.{}", i + 1), "a", v);
        }
        verse(&conn, "Ps.16.5", "b", &[0.0, 1.0]);
        let (note_id, _) = embedded_note(&conn, "first passage", &[1.0, 0.0]);
        // A second, weaker passage of the same note, close only to Ps 16:5.
        let source: i64 = conn
            .query_row("SELECT source_id FROM sources WHERE note_id = ?1", [note_id], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO chunks (source_id, idx, char_start, char_end, text, embedding, embedded_by)
             VALUES (?1, 1, 2, 3, 'second passage', ?2, 'm')",
            rusqlite::params![source, pack(&[0.0, 0.8])],
        )
        .unwrap();

        let found = verses_for_note(&conn, note_id, 1, Some("m"), &VectorCache::default()).unwrap();

        let ids: Vec<&str> = found.suggested.iter().map(|v| v.verse_id.as_str()).collect();
        assert_eq!(ids, vec!["Ps.1.1", "Ps.1.2", "Ps.16.5"]);
    }

    #[test]
    fn a_decision_holds_and_can_be_taken_back() {
        let conn = fixture();
        verse(&conn, "Ps.16.5", "Jehova ist das Teil meines Erbes.", &[1.0, 0.0]);
        verse(&conn, "Ps.16.6", "Die Meßschnüre sind mir gefallen.", &[0.98, 0.2]);
        let (note_id, _) = embedded_note(&conn, "Das Erbe", &[1.0, 0.0]);
        let cache = VectorCache::default();

        decide_verse(&conn, note_id, "Ps.16.5", Some("accepted"), Some(0.9), Some("m")).unwrap();
        decide_verse(&conn, note_id, "Ps.16.6", Some("dismissed"), Some(0.8), Some("m")).unwrap();
        let found = verses_for_note(&conn, note_id, 1, Some("m"), &cache).unwrap();
        assert!(found.suggested.is_empty(), "neither is offered again");
        assert_eq!(found.accepted.len(), 1);
        assert_eq!(found.accepted[0].text.as_deref(), Some("Jehova ist das Teil meines Erbes."));

        decide_verse(&conn, note_id, "Ps.16.6", None, None, None).unwrap();
        let found = verses_for_note(&conn, note_id, 1, Some("m"), &cache).unwrap();
        assert_eq!(found.suggested.len(), 1, "a dismissal taken back is offered again");

        assert!(decide_verse(&conn, note_id, "Ps.16.6", Some("maybe"), None, None).is_err());
    }

    #[test]
    fn without_a_model_only_what_was_accepted_comes_back() {
        let conn = fixture();
        verse(&conn, "Ps.16.5", "Jehova ist das Teil meines Erbes.", &[1.0, 0.0]);
        let (note_id, _) = embedded_note(&conn, "Das Erbe", &[1.0, 0.0]);
        decide_verse(&conn, note_id, "Ps.16.5", Some("accepted"), None, None).unwrap();

        let found = verses_for_note(&conn, note_id, 1, None, &VectorCache::default()).unwrap();

        assert!(found.suggested.is_empty());
        assert_eq!(found.accepted.len(), 1);
    }
}
