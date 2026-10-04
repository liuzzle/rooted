//! Reading the concept graph.
//!
//! Every query here ends at a citation. There is no call that returns a topic
//! as a claim — a topic comes back with the passages that say it, each one a
//! range of a chunk of a real note, and the snippet handed to the UI is that
//! chunk's own text. Nothing in this module composes prose, and there is
//! nowhere for prose to come from: the tables hold offsets and the notes hold
//! words.
//!
//! The worker writes this graph; the app only reads it.

use rusqlite::Connection;
use serde::Serialize;

/// A topic in a list: how often it is mentioned, and across how many notes.
#[derive(Serialize)]
pub struct ConceptSummary {
    pub concept_id: i64,
    pub label: String,
    pub mentions: i64,
    pub notes: i64,
}

/// One passage that mentions a topic, with everything needed to cite it.
///
/// `snippet` is the chunk's text, unmodified, and `char_start`/`char_end` are
/// offsets into *that string* — so the UI can mark the term without editing the
/// passage. `surface` is what was written there, kept so a mismatch would be
/// visible rather than silent.
#[derive(Serialize)]
pub struct Citation {
    pub note_id: i64,
    pub note_title: Option<String>,
    pub date: Option<String>,
    pub speaker: Option<String>,
    pub snippet: String,
    pub char_start: i64,
    pub char_end: i64,
    pub surface: String,
    pub extracted_by: String,
}

#[derive(Serialize)]
pub struct ConceptPage {
    pub concept_id: i64,
    pub label: String,
    pub mentions: i64,
    pub notes: i64,
    pub citations: Vec<Citation>,
}

const SUMMARY_COLUMNS: &str = "SELECT c.concept_id, c.label,
            COUNT(m.mention_id) AS mentions,
            COUNT(DISTINCT s.note_id) AS notes
       FROM concepts c
       JOIN concept_mentions m ON m.concept_id = c.concept_id
       JOIN chunks ch ON ch.chunk_id = m.chunk_id
       JOIN sources s ON s.source_id = ch.source_id";

fn map_summary(r: &rusqlite::Row) -> rusqlite::Result<ConceptSummary> {
    Ok(ConceptSummary {
        concept_id: r.get(0)?,
        label: r.get(1)?,
        mentions: r.get(2)?,
        notes: r.get(3)?,
    })
}

/// Topics, most-cited first. `query` filters by substring of the label.
///
/// The inner join is doing real work: a concept with no surviving mentions is
/// not a topic, it's a leftover, and it must not appear anywhere.
pub fn list_concepts(
    conn: &Connection,
    query: Option<String>,
    limit: i64,
) -> Result<Vec<ConceptSummary>, String> {
    let filter = query.unwrap_or_default();
    let sql = format!(
        "{SUMMARY_COLUMNS}
          WHERE (?1 = '' OR c.label LIKE '%' || ?1 || '%')
          GROUP BY c.concept_id
          ORDER BY notes DESC, mentions DESC, c.label
          LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![filter, limit], map_summary)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>();
    rows.map_err(|e| e.to_string())
}

/// The topics a single note mentions — its backlinks, from the note's side.
pub fn concepts_for_note(
    conn: &Connection,
    note_id: i64,
) -> Result<Vec<ConceptSummary>, String> {
    let sql = format!(
        "{SUMMARY_COLUMNS}
          WHERE s.note_id = ?1
          GROUP BY c.concept_id
          ORDER BY mentions DESC, c.label"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([note_id], map_summary)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>();
    rows.map_err(|e| e.to_string())
}

/// A topic page: the label, and every passage that mentions it.
pub fn get_concept(conn: &Connection, concept_id: i64) -> Result<ConceptPage, String> {
    let (label, mentions, notes) = conn
        .query_row(
            "SELECT c.label, COUNT(m.mention_id), COUNT(DISTINCT s.note_id)
               FROM concepts c
               JOIN concept_mentions m ON m.concept_id = c.concept_id
               JOIN chunks ch ON ch.chunk_id = m.chunk_id
               JOIN sources s ON s.source_id = ch.source_id
              WHERE c.concept_id = ?1
              GROUP BY c.concept_id",
            [concept_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)),
        )
        .map_err(|_| format!("no topic {concept_id}, or nothing cites it any more"))?;

    let mut stmt = conn
        .prepare(
            "SELECT n.note_id, n.title, n.date, n.speaker, ch.text,
                    m.char_start, m.char_end, m.surface, m.extracted_by
               FROM concept_mentions m
               JOIN chunks ch ON ch.chunk_id = m.chunk_id
               JOIN sources s ON s.source_id = ch.source_id
               JOIN notes n ON n.note_id = s.note_id
              WHERE m.concept_id = ?1
              ORDER BY n.date IS NULL, n.date DESC, n.note_id, ch.idx, m.char_start",
        )
        .map_err(|e| e.to_string())?;
    let citations = stmt
        .query_map([concept_id], |r| {
            Ok(Citation {
                note_id: r.get(0)?,
                note_title: r.get(1)?,
                date: r.get(2)?,
                speaker: r.get(3)?,
                snippet: r.get(4)?,
                char_start: r.get(5)?,
                char_end: r.get(6)?,
                surface: r.get(7)?,
                extracted_by: r.get(8)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    Ok(ConceptPage {
        concept_id,
        label,
        mentions,
        notes,
        citations,
    })
}

/// A scripture reference written inside a note.
///
/// `char_start`/`char_end` are offsets into the **note body**, not the chunk —
/// the chunk's own offset is added here, because the reader shows the note
/// whole and would otherwise have to reassemble it.
#[derive(Serialize)]
pub struct NoteReference {
    pub char_start: i64,
    pub char_end: i64,
    pub surface: String,
    pub book_osis: String,
    pub chapter: i64,
    pub verse: Option<i64>,
    pub verse_end: Option<i64>,
    pub verse_id: Option<String>,
}

pub fn references_for_note(
    conn: &Connection,
    note_id: i64,
) -> Result<Vec<NoteReference>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT ch.char_start + v.char_start, ch.char_start + v.char_end,
                    v.surface, v.book_osis, v.chapter, v.verse, v.verse_end, v.verse_id
               FROM verse_links v
               JOIN chunks ch ON ch.chunk_id = v.chunk_id
               JOIN sources s ON s.source_id = ch.source_id
              WHERE s.note_id = ?1
              ORDER BY 1",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([note_id], |r| {
            Ok(NoteReference {
                char_start: r.get(0)?,
                char_end: r.get(1)?,
                surface: r.get(2)?,
                book_osis: r.get(3)?,
                chapter: r.get(4)?,
                verse: r.get(5)?,
                verse_end: r.get(6)?,
                verse_id: r.get(7)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>();
    rows.map_err(|e| e.to_string())
}

/// The text of one verse in one translation, for previewing a reference.
///
/// `None` when the translation doesn't have it — a note can cite a verse the
/// installed pack doesn't contain, and saying so is better than showing
/// something from elsewhere.
pub fn verse_text(
    conn: &Connection,
    translation_id: i64,
    verse_id: &str,
) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT text FROM verses WHERE translation_id = ?1 AND verse_id = ?2",
        rusqlite::params![translation_id, verse_id],
        |r| r.get(0),
    )
    .map(Some)
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(other.to_string()),
    })
}

/// A topic in the graph.
#[derive(Serialize)]
pub struct GraphNode {
    pub concept_id: i64,
    pub label: String,
    pub notes: i64,
}

/// Two topics written about in the same passage, and in how many passages.
/// The passages themselves are the evidence, fetched by `shared_passages`.
#[derive(Serialize)]
pub struct GraphEdge {
    pub a: i64,
    pub b: i64,
    pub passages: i64,
}

#[derive(Serialize)]
pub struct ConceptGraph {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

/// The topic graph: the `limit` most-reached topics, joined where a passage
/// mentions both.
///
/// An edge is co-occurrence in one chunk — one paragraph of one note — and
/// nothing weaker. Not "similar", not "related": a person wrote these two
/// things together, and the edge can show you where.
pub fn concept_graph(conn: &Connection, limit: i64) -> Result<ConceptGraph, String> {
    let nodes = collect_rows(
        conn,
        &format!(
            "{SUMMARY_COLUMNS}
              GROUP BY c.concept_id
              ORDER BY notes DESC, mentions DESC, c.label
              LIMIT ?1"
        ),
        [limit],
        |r| {
            Ok(GraphNode {
                concept_id: r.get(0)?,
                label: r.get(1)?,
                notes: r.get(3)?,
            })
        },
    )?;
    let ids = nodes
        .iter()
        .map(|n| n.concept_id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    if ids.is_empty() {
        return Ok(ConceptGraph { nodes, edges: Vec::new() });
    }
    let edges = collect_rows(
        conn,
        &format!(
            "SELECT x.concept_id, y.concept_id, COUNT(DISTINCT x.chunk_id)
               FROM concept_mentions x
               JOIN concept_mentions y
                 ON y.chunk_id = x.chunk_id AND y.concept_id > x.concept_id
              WHERE x.concept_id IN ({ids}) AND y.concept_id IN ({ids})
              GROUP BY x.concept_id, y.concept_id"
        ),
        [],
        |r| {
            Ok(GraphEdge {
                a: r.get(0)?,
                b: r.get(1)?,
                passages: r.get(2)?,
            })
        },
    )?;
    Ok(ConceptGraph { nodes, edges })
}

/// The passages behind an edge: every chunk that mentions both topics, with
/// both marked. Each mark is a citation of its own.
#[derive(Serialize)]
pub struct SharedPassage {
    pub note_id: i64,
    pub note_title: Option<String>,
    pub date: Option<String>,
    pub speaker: Option<String>,
    pub snippet: String,
    pub marks: Vec<(i64, i64)>,
}

pub fn shared_passages(conn: &Connection, a: i64, b: i64) -> Result<Vec<SharedPassage>, String> {
    let chunks = collect_rows(
        conn,
        "SELECT ch.chunk_id, n.note_id, n.title, n.date, n.speaker, ch.text
           FROM chunks ch
           JOIN sources s ON s.source_id = ch.source_id
           JOIN notes n ON n.note_id = s.note_id
          WHERE EXISTS (SELECT 1 FROM concept_mentions WHERE chunk_id = ch.chunk_id AND concept_id = ?1)
            AND EXISTS (SELECT 1 FROM concept_mentions WHERE chunk_id = ch.chunk_id AND concept_id = ?2)
          ORDER BY n.date IS NULL, n.date DESC, n.note_id, ch.idx",
        [a, b],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                SharedPassage {
                    note_id: r.get(1)?,
                    note_title: r.get(2)?,
                    date: r.get(3)?,
                    speaker: r.get(4)?,
                    snippet: r.get(5)?,
                    marks: Vec::new(),
                },
            ))
        },
    )?;
    let mut out = Vec::new();
    for (chunk_id, mut passage) in chunks {
        passage.marks = collect_rows(
            conn,
            "SELECT char_start, char_end FROM concept_mentions
              WHERE chunk_id = ?1 AND concept_id IN (?2, ?3)
              ORDER BY char_start",
            [chunk_id, a, b],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        out.push(passage);
    }
    Ok(out)
}

fn collect_rows<T, P, F>(conn: &Connection, sql: &str, params: P, f: F) -> Result<Vec<T>, String>
where
    P: rusqlite::Params,
    F: FnMut(&rusqlite::Row) -> rusqlite::Result<T>,
{
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let rows = stmt.query_map(params, f).map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    /// Stand in for the worker: a note, its chunk, and a mention of a topic.
    fn indexed(conn: &Connection, title: &str, chunk: &str, label: &str, at: (i64, i64)) -> i64 {
        conn.execute(
            "INSERT INTO notes (title, body) VALUES (?1, ?2)",
            rusqlite::params![title, chunk],
        )
        .unwrap();
        let note_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO sources (kind, note_id, text_hash, indexed_by)
             VALUES ('note', ?1, 'hash', 'phrases')",
            [note_id],
        )
        .unwrap();
        let source_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO chunks (source_id, idx, char_start, char_end, text)
             VALUES (?1, 0, 0, ?2, ?3)",
            rusqlite::params![source_id, chunk.len() as i64, chunk],
        )
        .unwrap();
        let chunk_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO concepts (label, key) VALUES (?1, ?2)
             ON CONFLICT(key) DO NOTHING",
            rusqlite::params![label, label.to_lowercase()],
        )
        .unwrap();
        let concept_id: i64 = conn
            .query_row(
                "SELECT concept_id FROM concepts WHERE key = ?1",
                [label.to_lowercase()],
                |r| r.get(0),
            )
            .unwrap();
        let surface = &chunk[at.0 as usize..at.1 as usize];
        conn.execute(
            "INSERT INTO concept_mentions
               (concept_id, chunk_id, char_start, char_end, surface, extracted_by)
             VALUES (?1, ?2, ?3, ?4, ?5, 'phrases')",
            rusqlite::params![concept_id, chunk_id, at.0, at.1, surface],
        )
        .unwrap();
        concept_id
    }

    fn fixture() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db::apply_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn a_topic_page_carries_the_passage_that_says_it() {
        let conn = fixture();
        let id = indexed(&conn, "Covenant study", "The covenant is not earned.", "covenant", (4, 12));

        let page = get_concept(&conn, id).unwrap();

        assert_eq!(page.label, "covenant");
        assert_eq!(page.citations.len(), 1);
        let cited = &page.citations[0];
        // The snippet is the passage itself, and the offsets index into it.
        assert_eq!(cited.snippet, "The covenant is not earned.");
        let marked = &cited.snippet[cited.char_start as usize..cited.char_end as usize];
        assert_eq!(marked, cited.surface);
        assert_eq!(cited.note_title.as_deref(), Some("Covenant study"));
    }

    #[test]
    fn topics_rank_by_how_many_notes_reach_them() {
        let conn = fixture();
        indexed(&conn, "One", "the covenant holds", "covenant", (4, 12));
        indexed(&conn, "Two", "the covenant again", "covenant", (4, 12));
        indexed(&conn, "Three", "about Melchizedek here", "Melchizedek", (6, 17));

        let listed = list_concepts(&conn, None, 10).unwrap();

        assert_eq!(listed[0].label, "covenant");
        assert_eq!(listed[0].notes, 2);
        assert_eq!(listed[0].mentions, 2);
        assert_eq!(listed[1].label, "Melchizedek");
    }

    #[test]
    fn a_topic_nothing_cites_any_more_is_not_a_topic() {
        let conn = fixture();
        let id = indexed(&conn, "One", "the covenant holds", "covenant", (4, 12));
        // A note edited past its only mention: the cascade takes the mention.
        conn.execute("DELETE FROM chunks", []).unwrap();

        assert!(list_concepts(&conn, None, 10).unwrap().is_empty());
        assert!(
            get_concept(&conn, id).is_err(),
            "a bare label with no evidence must not render as a topic"
        );
    }

    #[test]
    fn a_reference_is_reported_where_it_sits_in_the_note() {
        let conn = fixture();
        // A note whose second paragraph starts at offset 20 and cites Röm 4,3
        // eight characters in: the reader needs 28, not 8.
        indexed(&conn, "Bund", "siehe Röm 4,3 dort", "Bund", (0, 5));
        let chunk_id: i64 = conn
            .query_row("SELECT chunk_id FROM chunks", [], |r| r.get(0))
            .unwrap();
        conn.execute("UPDATE chunks SET char_start = 20 WHERE chunk_id = ?1", [chunk_id])
            .unwrap();
        conn.execute(
            "INSERT INTO verse_links
               (chunk_id, char_start, char_end, surface, book_osis, chapter, verse, verse_id)
             VALUES (?1, 6, 13, 'Röm 4,3', 'Rom', 4, 3, 'Rom.4.3')",
            [chunk_id],
        )
        .unwrap();
        let note_id: i64 = conn
            .query_row("SELECT note_id FROM notes", [], |r| r.get(0))
            .unwrap();

        let found = references_for_note(&conn, note_id).unwrap();

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].char_start, 26);
        assert_eq!(found[0].verse_id.as_deref(), Some("Rom.4.3"));
    }

    #[test]
    fn a_verse_the_installed_pack_lacks_is_absent_not_wrong() {
        let conn = fixture();
        assert_eq!(verse_text(&conn, 1, "Rom.4.3").unwrap(), None);
    }

    #[test]
    fn a_note_can_be_asked_which_topics_it_mentions() {
        let conn = fixture();
        indexed(&conn, "One", "the covenant holds", "covenant", (4, 12));
        let note_id: i64 = conn
            .query_row("SELECT note_id FROM notes", [], |r| r.get(0))
            .unwrap();

        let found = concepts_for_note(&conn, note_id).unwrap();

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].label, "covenant");
    }

    /// A second topic mentioned in the chunk `indexed` just made.
    fn also_mentions(conn: &Connection, label: &str, at: (i64, i64)) -> i64 {
        let chunk_id: i64 = conn
            .query_row("SELECT MAX(chunk_id) FROM chunks", [], |r| r.get(0))
            .unwrap();
        let text: String = conn
            .query_row("SELECT text FROM chunks WHERE chunk_id = ?1", [chunk_id], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO concepts (label, key) VALUES (?1, ?2) ON CONFLICT(key) DO NOTHING",
            rusqlite::params![label, label.to_lowercase()],
        )
        .unwrap();
        let concept_id: i64 = conn
            .query_row("SELECT concept_id FROM concepts WHERE key = ?1", [label.to_lowercase()], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO concept_mentions (concept_id, chunk_id, char_start, char_end, surface, extracted_by)
             VALUES (?1, ?2, ?3, ?4, ?5, 'phrases')",
            rusqlite::params![concept_id, chunk_id, at.0, at.1, &text[at.0 as usize..at.1 as usize]],
        )
        .unwrap();
        concept_id
    }

    #[test]
    fn an_edge_is_two_topics_written_in_one_passage() {
        let conn = fixture();
        let covenant = indexed(&conn, "One", "covenant and grace", "covenant", (0, 8));
        let grace = also_mentions(&conn, "grace", (13, 18));
        indexed(&conn, "Two", "covenant alone", "covenant", (0, 8));
        indexed(&conn, "Three", "Melchizedek alone", "Melchizedek", (0, 11));

        let graph = concept_graph(&conn, 10).unwrap();

        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(graph.edges.len(), 1, "topics never written together aren't joined");
        let edge = &graph.edges[0];
        assert_eq!((edge.a.min(edge.b), edge.a.max(edge.b)), (covenant.min(grace), covenant.max(grace)));
        assert_eq!(edge.passages, 1);
    }

    #[test]
    fn an_edge_shows_the_passage_that_makes_it() {
        let conn = fixture();
        let covenant = indexed(&conn, "One", "covenant and grace", "covenant", (0, 8));
        let grace = also_mentions(&conn, "grace", (13, 18));

        let shared = shared_passages(&conn, covenant, grace).unwrap();

        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0].snippet, "covenant and grace");
        assert_eq!(shared[0].marks, vec![(0, 8), (13, 18)]);
    }

    #[test]
    fn filtering_matches_on_the_label() {
        let conn = fixture();
        indexed(&conn, "One", "the covenant holds", "covenant", (4, 12));
        indexed(&conn, "Two", "about Melchizedek here", "Melchizedek", (6, 17));

        let found = list_concepts(&conn, Some("coven".into()), 10).unwrap();

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].label, "covenant");
    }
}
