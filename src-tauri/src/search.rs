//! Search: passages, never answers.
//!
//! A query comes back as two lists — passages from your notes and verses from
//! the Bible — and every entry in either is quoted whole from the table it lives
//! in, with the place it came from. Nothing here writes a sentence. A query
//! nothing matches returns two empty lists, and that is the correct answer.
//!
//! Each lane is a union of independent ways of finding a passage, and every hit
//! says which of them found it:
//!
//! * `words` — the full-text index. Exact: it found what was written.
//! * `topic` — the query is a topic the graph knows; its mentions are the hits,
//!   marked at the offsets the worker recorded.
//! * `cited` — (Bible lane) a verse that a matching note cites. The graph
//!   doing the walking: query → notes → the references they wrote.
//! * `meaning` — vector similarity, when a local model has read the text. It
//!   ranks passages that exist; it cannot produce one. Below a similarity floor
//!   a passage is not reported, so an unrelated query still finds nothing.
//!
//! Ranks are merged by reciprocal rank fusion rather than by adding scores:
//! bm25 and cosine are on different scales, and a passage found two ways
//! should beat one found strongly one way.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// Below this cosine a passage is not "about" the query.
///
/// Calibrated on bge-m3 over all 31,070 verses of ELB71, not a sample: with
/// that many candidates the best-scoring *noise* climbs to 0.40–0.49 ("asdfgh"
/// finds a genealogy, "Steuererklärung Finanzamt" finds something), while real
/// multi-word queries top out at 0.63–0.67. A relative test (z-score against
/// the query's own distribution) does not separate them; this does. Single
/// words score like noise and so add nothing here — a literal word is the
/// words lane's job.
pub const DEFAULT_FLOOR: f32 = 0.55;

/// How many passages each way of finding contributes before fusion.
const PER_SOURCE: usize = 50;
const PER_LANE: usize = 30;
/// Reciprocal rank fusion constant; 60 is the value from the original paper
/// and nothing here is tuned finely enough to argue with it.
const RRF_K: f64 = 60.0;
/// A cited range wider than this is a passage, not a verse; listing every
/// verse of it would drown the lane.
const MAX_RANGE: i64 = 12;

#[derive(Serialize, Debug)]
pub struct NoteHit {
    pub note_id: i64,
    pub note_title: Option<String>,
    pub date: Option<String>,
    pub speaker: Option<String>,
    /// Where this passage starts in the note body.
    pub chunk_start: i64,
    /// The passage, exactly as stored.
    pub snippet: String,
    /// `[start, end)` character offsets into `snippet`.
    pub marks: Vec<(i64, i64)>,
    pub matched_by: Vec<String>,
    pub score: f64,
}

#[derive(Serialize, Debug)]
pub struct VerseHit {
    pub verse_id: String,
    pub book_osis: String,
    pub book_name: String,
    pub chapter: i64,
    pub verse: i64,
    pub snippet: String,
    pub marks: Vec<(i64, i64)>,
    pub matched_by: Vec<String>,
    /// How many of the matching notes cite this verse.
    pub cited_by: i64,
    pub score: f64,
}

/// Whether search by meaning took part, and if not, why — the UI says so
/// rather than letting a thinner result list look like the whole truth.
#[derive(Serialize, Debug, Default)]
pub struct Meaning {
    pub used: bool,
    pub model: Option<String>,
    pub detail: String,
    pub notes_read: i64,
    pub notes_total: i64,
    pub verses_read: i64,
    pub verses_total: i64,
}

#[derive(Serialize, Debug)]
pub struct Results {
    pub query: String,
    pub notes: Vec<NoteHit>,
    pub bible: Vec<VerseHit>,
    pub meaning: Meaning,
}

/// What the worker said about its embedding model (`settings.embedding`).
#[derive(Deserialize, Debug, Clone)]
pub struct EmbeddingConfig {
    pub model: String,
    pub host: String,
    #[serde(default)]
    pub available: bool,
}

pub fn embedding_config(conn: &Connection) -> Result<Option<EmbeddingConfig>, String> {
    let raw: Option<String> = conn
        .query_row("SELECT value FROM settings WHERE key = 'embedding'", [], |r| r.get(0))
        .optional()
        .map_err(|e| e.to_string())?;
    Ok(raw.and_then(|json| serde_json::from_str(&json).ok()))
}

/// Embed the query with the same model the worker used for the passages.
pub async fn embed_query(config: &EmbeddingConfig, text: &str) -> Result<Vec<f32>, String> {
    #[derive(Deserialize)]
    struct Reply {
        embeddings: Vec<Vec<f32>>,
    }
    let url = format!("{}/api/embed", config.host.trim_end_matches('/'));
    let reply: Reply = reqwest::Client::builder()
        // Generous: the first query after a while loads the model.
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?
        .post(&url)
        .json(&serde_json::json!({ "model": config.model, "input": [text] }))
        .send()
        .await
        .map_err(|e| format!("{url}: {e}"))?
        .error_for_status()
        .map_err(|e| format!("{url}: {e}"))?
        .json()
        .await
        .map_err(|e| format!("{url}: {e}"))?;
    let mut vector = reply
        .embeddings
        .into_iter()
        .next()
        .ok_or_else(|| format!("{url}: no vector in reply"))?;
    normalise(&mut vector);
    Ok(vector)
}

fn normalise(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
}

pub(crate) fn unpack(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

pub(crate) fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// A translation's verse vectors, held in memory between queries.
///
/// Thirty thousand vectors is too much to read from disk per keystroke and
/// little enough to keep. Keyed by how many there are, so the cache refills
/// while the worker is still filling the table.
#[derive(Default)]
pub struct VectorCache(Mutex<Option<CachedVerses>>);

struct CachedVerses {
    translation_id: i64,
    model: String,
    count: i64,
    dim: usize,
    ids: Vec<String>,
    flat: Vec<f32>,
}

impl VectorCache {
    pub(crate) fn nearest(
        &self,
        conn: &Connection,
        translation_id: i64,
        model: &str,
        query: &[f32],
        floor: f32,
    ) -> Result<Vec<(String, f32)>, String> {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM verse_vectors WHERE translation_id = ?1 AND model = ?2",
                rusqlite::params![translation_id, model],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        let mut guard = self.0.lock().map_err(|e| e.to_string())?;
        let fresh = matches!(&*guard, Some(c)
            if c.translation_id == translation_id && c.model == model && c.count == count);
        if !fresh {
            let mut stmt = conn
                .prepare(
                    "SELECT verse_id, vector FROM verse_vectors
                      WHERE translation_id = ?1 AND model = ?2",
                )
                .map_err(|e| e.to_string())?;
            let mut ids = Vec::new();
            let mut flat = Vec::new();
            let mut dim = 0;
            let rows = stmt
                .query_map(rusqlite::params![translation_id, model], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
                })
                .map_err(|e| e.to_string())?;
            for row in rows {
                let (id, blob) = row.map_err(|e| e.to_string())?;
                let v = unpack(&blob);
                if dim == 0 {
                    dim = v.len();
                }
                // A vector of another width can't be compared; skip it rather
                // than misalign every vector after it.
                if v.len() == dim {
                    ids.push(id);
                    flat.extend(v);
                }
            }
            *guard = Some(CachedVerses {
                translation_id,
                model: model.to_string(),
                count,
                dim,
                ids,
                flat,
            });
        }
        let cached = guard.as_ref().expect("filled above");
        if cached.dim != query.len() {
            return Ok(Vec::new());
        }
        // Score by position and copy an id only for what clears the floor:
        // cloning thirty thousand ids per query cost more than the arithmetic.
        Ok(top_k(
            cached
                .flat
                .chunks_exact(cached.dim.max(1))
                .enumerate()
                .map(|(i, v)| (i, dot(v, query))),
            floor,
        )
        .into_iter()
        .map(|(i, score)| (cached.ids[i].clone(), score))
        .collect())
    }
}

fn top_k<I: Iterator<Item = (T, f32)>, T>(scored: I, floor: f32) -> Vec<(T, f32)> {
    let mut kept: Vec<(T, f32)> = scored.filter(|(_, s)| *s >= floor).collect();
    kept.sort_by(|a, b| b.1.total_cmp(&a.1));
    kept.truncate(PER_SOURCE);
    kept
}

// --- the query -------------------------------------------------------------

/// Case and diacritics folded, as the full-text index does it, so marks land
/// on the same words the index matched.
fn fold(word: &str) -> String {
    word.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'ñ' => 'n',
            'ç' => 'c',
            'ý' | 'ÿ' => 'y',
            other => other,
        })
        .collect()
}

/// Words with their `[start, end)` positions, in characters.
fn words(text: &str) -> Vec<(String, i64, i64)> {
    let mut out = Vec::new();
    let mut start: Option<(usize, String)> = None;
    let mut i = 0usize;
    for c in text.chars() {
        if c.is_alphanumeric() {
            match &mut start {
                Some((_, w)) => w.push(c),
                None => start = Some((i, c.to_string())),
            }
        } else if let Some((s, w)) = start.take() {
            out.push((w, s as i64, i as i64));
        }
        i += 1;
    }
    if let Some((s, w)) = start {
        out.push((w, s as i64, i as i64));
    }
    out
}

/// The query as search terms, folded.
fn terms(query: &str) -> Vec<String> {
    words(query).into_iter().map(|(w, _, _)| fold(&w)).collect()
}

/// An FTS5 expression: every term, each as a prefix, all required. Prefixes
/// are how "Gnade" finds "Gnaden" without a German stemmer on this side.
/// Each term is quoted, so nothing the user types is read as FTS syntax.
fn fts_expression(terms: &[String]) -> Option<String> {
    if terms.is_empty() {
        return None;
    }
    Some(
        terms
            .iter()
            .map(|t| format!("\"{}\"*", t.replace('"', "")))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// Words in `text` that begin with one of the terms.
fn word_marks(text: &str, terms: &[String]) -> Vec<(i64, i64)> {
    words(text)
        .into_iter()
        .filter(|(w, _, _)| {
            let folded = fold(w);
            terms.iter().any(|t| folded.starts_with(t.as_str()))
        })
        .map(|(_, s, e)| (s, e))
        .collect()
}

fn merge_marks(mut marks: Vec<(i64, i64)>) -> Vec<(i64, i64)> {
    marks.sort();
    let mut out: Vec<(i64, i64)> = Vec::new();
    for (s, e) in marks {
        match out.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// Ranked lists from several ways of finding, fused. Keeps insertion order of
/// first appearance as the tiebreak, so results are stable between runs.
struct Fusion<K: std::hash::Hash + Eq + Clone> {
    scores: HashMap<K, f64>,
    found_by: HashMap<K, Vec<String>>,
    order: Vec<K>,
}

impl<K: std::hash::Hash + Eq + Clone> Fusion<K> {
    fn new() -> Self {
        Fusion {
            scores: HashMap::new(),
            found_by: HashMap::new(),
            order: Vec::new(),
        }
    }

    fn add(&mut self, how: &str, ranked: impl IntoIterator<Item = K>) {
        for (rank, key) in ranked.into_iter().enumerate() {
            if !self.scores.contains_key(&key) {
                self.order.push(key.clone());
            }
            *self.scores.entry(key.clone()).or_default() += 1.0 / (RRF_K + rank as f64 + 1.0);
            let by = self.found_by.entry(key).or_default();
            if !by.iter().any(|b| b == how) {
                by.push(how.to_string());
            }
        }
    }

    fn ranked(mut self, limit: usize) -> Vec<(K, f64, Vec<String>)> {
        let position: HashMap<K, usize> =
            self.order.iter().enumerate().map(|(i, k)| (k.clone(), i)).collect();
        let mut all: Vec<K> = self.order.clone();
        all.sort_by(|a, b| {
            self.scores[b]
                .total_cmp(&self.scores[a])
                .then(position[a].cmp(&position[b]))
        });
        all.truncate(limit);
        all.into_iter()
            .map(|k| {
                let score = self.scores[&k];
                let by = self.found_by.remove(&k).unwrap_or_default();
                (k, score, by)
            })
            .collect()
    }
}

fn collect<T, F>(conn: &Connection, sql: &str, params: impl rusqlite::Params, f: F) -> Result<Vec<T>, String>
where
    F: FnMut(&rusqlite::Row) -> rusqlite::Result<T>,
{
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let rows = stmt.query_map(params, f).map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

/// Topics whose label, or any written form of which, is the query.
///
/// Compared here rather than in SQL because SQLite's `lower()` only knows
/// ASCII, and "Gnade" has to find a topic labelled "GNADE" as much as "gnade".
fn matching_concepts(conn: &Connection, query: &str) -> Result<Vec<i64>, String> {
    let wanted = terms(query).join(" ");
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let forms: Vec<(i64, String)> = collect(
        conn,
        "SELECT concept_id, label FROM concepts
         UNION SELECT DISTINCT concept_id, surface FROM concept_mentions",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let mut ids: Vec<i64> = forms
        .into_iter()
        .filter(|(_, form)| terms(form).join(" ") == wanted)
        .map(|(id, _)| id)
        .collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// Run a search. `query_vector` is `None` when meaning isn't available; the
/// lanes then come from words, topics and citations alone.
pub fn search(
    conn: &Connection,
    query: &str,
    translation_id: i64,
    query_vector: Option<(&str, &[f32])>,
    cache: &VectorCache,
    floor: f32,
) -> Result<(Vec<NoteHit>, Vec<VerseHit>), String> {
    let terms = terms(query);
    let Some(expr) = fts_expression(&terms) else {
        return Ok((Vec::new(), Vec::new()));
    };

    // --- notes -------------------------------------------------------------
    let mut notes = Fusion::<i64>::new();

    let by_words: Vec<i64> = collect(
        conn,
        "SELECT rowid FROM chunks_fts WHERE chunks_fts MATCH ?1 ORDER BY rank LIMIT ?2",
        rusqlite::params![expr, PER_SOURCE as i64],
        |r| r.get(0),
    )?;
    notes.add("words", by_words);

    // Mentions of a matching topic, keyed by chunk, with their offsets: those
    // offsets are the evidence and are what gets marked.
    let mut mention_marks: HashMap<i64, Vec<(i64, i64)>> = HashMap::new();
    let concept_ids = matching_concepts(conn, query)?;
    if !concept_ids.is_empty() {
        let list = concept_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",");
        let mentions: Vec<(i64, i64, i64)> = collect(
            conn,
            &format!(
                "SELECT chunk_id, char_start, char_end FROM concept_mentions
                  WHERE concept_id IN ({list}) ORDER BY chunk_id, char_start"
            ),
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let mut chunks_in_order = Vec::new();
        for (chunk_id, s, e) in mentions {
            let entry = mention_marks.entry(chunk_id).or_default();
            if entry.is_empty() {
                chunks_in_order.push(chunk_id);
            }
            entry.push((s, e));
        }
        // More mentions in a passage, earlier in the list.
        chunks_in_order.sort_by_key(|c| std::cmp::Reverse(mention_marks[c].len()));
        notes.add("topic", chunks_in_order);
    }

    if let Some((model, qv)) = query_vector {
        let embedded: Vec<(i64, Vec<u8>)> = collect(
            conn,
            "SELECT chunk_id, embedding FROM chunks
              WHERE embedded_by = ?1 AND embedding IS NOT NULL",
            [model],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let near = top_k(
            embedded.into_iter().filter_map(|(id, blob)| {
                let v = unpack(&blob);
                (v.len() == qv.len()).then(|| (id, dot(&v, qv)))
            }),
            floor,
        );
        notes.add("meaning", near.into_iter().map(|(id, _)| id));
    }

    // Every matching passage, not just the ones shown — the Bible lane follows
    // citations from all of them.
    let matched_chunks: Vec<i64> = notes.order.clone();
    let note_ranked = notes.ranked(PER_LANE);

    let mut note_hits = Vec::new();
    for (chunk_id, score, matched_by) in note_ranked {
        let row = conn
            .query_row(
                "SELECT n.note_id, n.title, n.date, n.speaker, ch.char_start, ch.text
                   FROM chunks ch
                   JOIN sources s ON s.source_id = ch.source_id
                   JOIN notes n ON n.note_id = s.note_id
                  WHERE ch.chunk_id = ?1",
                [chunk_id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| e.to_string())?;
        // A chunk the worker replaced between the two queries: gone, not wrong.
        let Some((note_id, title, date, speaker, chunk_start, text)) = row else {
            continue;
        };
        let mut marks = word_marks(&text, &terms);
        marks.extend(mention_marks.remove(&chunk_id).unwrap_or_default());
        note_hits.push(NoteHit {
            note_id,
            note_title: title,
            date,
            speaker,
            chunk_start,
            snippet: text,
            marks: merge_marks(marks),
            matched_by,
            score,
        });
    }

    // --- bible -------------------------------------------------------------
    let mut bible = Fusion::<String>::new();

    let by_words: Vec<String> = collect(
        conn,
        "SELECT v.verse_id FROM verses_fts
           JOIN verses v ON v.rowid = verses_fts.rowid
          WHERE verses_fts MATCH ?1 AND v.translation_id = ?2
          ORDER BY verses_fts.rank LIMIT ?3",
        rusqlite::params![expr, translation_id, PER_SOURCE as i64],
        |r| r.get(0),
    )?;
    bible.add("words", by_words);

    // Verses the matching notes cite, most-cited first.
    let mut cited_by: HashMap<String, HashSet<i64>> = HashMap::new();
    if !matched_chunks.is_empty() {
        let list = matched_chunks.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",");
        let links: Vec<(i64, String, i64, i64, Option<i64>)> = collect(
            conn,
            &format!(
                "SELECT s.note_id, v.book_osis, v.chapter, v.verse, v.verse_end
                   FROM verse_links v
                   JOIN chunks ch ON ch.chunk_id = v.chunk_id
                   JOIN sources s ON s.source_id = ch.source_id
                  WHERE v.chunk_id IN ({list}) AND v.verse IS NOT NULL"
            ),
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )?;
        for (note_id, book, chapter, verse, verse_end) in links {
            let last = verse_end.unwrap_or(verse).min(verse + MAX_RANGE - 1).max(verse);
            for v in verse..=last {
                cited_by
                    .entry(format!("{book}.{chapter}.{v}"))
                    .or_default()
                    .insert(note_id);
            }
        }
        // A verse a person accepted for a matching note is linked as surely as
        // one the note wrote down.
        let accepted: Vec<(i64, String)> = collect(
            conn,
            &format!(
                "SELECT DISTINCT vs.note_id, vs.verse_id FROM verse_suggestions vs
                   JOIN sources s ON s.note_id = vs.note_id
                   JOIN chunks ch ON ch.source_id = s.source_id
                  WHERE ch.chunk_id IN ({list}) AND vs.status = 'accepted'"
            ),
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        for (note_id, verse_id) in accepted {
            cited_by.entry(verse_id).or_default().insert(note_id);
        }
        let mut cited: Vec<(&String, usize)> =
            cited_by.iter().map(|(id, notes)| (id, notes.len())).collect();
        cited.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        bible.add("cited", cited.into_iter().map(|(id, _)| id.clone()));
    }

    if let Some((model, qv)) = query_vector {
        let near = cache.nearest(conn, translation_id, model, qv, floor)?;
        bible.add("meaning", near.into_iter().map(|(id, _)| id));
    }

    let mut verse_hits = Vec::new();
    for (verse_id, score, matched_by) in bible.ranked(PER_LANE) {
        let row = conn
            .query_row(
                "SELECT v.book_osis, b.name, v.chapter, v.verse, v.text
                   FROM verses v JOIN books b ON b.osis = v.book_osis
                  WHERE v.translation_id = ?1 AND v.verse_id = ?2",
                rusqlite::params![translation_id, verse_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| e.to_string())?;
        // A note can cite a verse this translation doesn't have. Not shown,
        // rather than shown from somewhere else.
        let Some((book_osis, book_name, chapter, verse, text)) = row else {
            continue;
        };
        let marks = merge_marks(word_marks(&text, &terms));
        verse_hits.push(VerseHit {
            cited_by: cited_by.get(&verse_id).map_or(0, |n| n.len() as i64),
            verse_id,
            book_osis,
            book_name,
            chapter,
            verse,
            snippet: text,
            marks,
            matched_by,
            score,
        });
    }

    Ok((note_hits, verse_hits))
}

/// How much of the corpus the model has read, so a partial result can say so.
pub fn coverage(
    conn: &Connection,
    translation_id: i64,
    model: &str,
) -> Result<(i64, i64, i64, i64), String> {
    let counts: BTreeMap<&str, i64> = [
        ("notes_read", "SELECT COUNT(*) FROM chunks WHERE embedded_by = ?1 AND ?2 = ?2"),
        ("notes_total", "SELECT COUNT(*) FROM chunks WHERE ?1 = ?1 AND ?2 = ?2"),
        (
            "verses_read",
            "SELECT COUNT(*) FROM verse_vectors WHERE model = ?1 AND translation_id = ?2",
        ),
        ("verses_total", "SELECT COUNT(*) FROM verses WHERE ?1 = ?1 AND translation_id = ?2"),
    ]
    .into_iter()
    .map(|(name, sql)| {
        conn.query_row(sql, rusqlite::params![model, translation_id], |r| r.get(0))
            .map(|n: i64| (name, n))
            .map_err(|e| e.to_string())
    })
    .collect::<Result<_, _>>()?;
    Ok((
        counts["notes_read"],
        counts["notes_total"],
        counts["verses_read"],
        counts["verses_total"],
    ))
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
        conn.execute_batch(
            "INSERT INTO translations (id, abbrev, name, language) VALUES (1, 'ELB71', 'Elberfelder', 'de');",
        )
        .unwrap();
        conn
    }

    fn verse(conn: &Connection, verse_id: &str, text: &str) {
        let parts: Vec<&str> = verse_id.split('.').collect();
        conn.execute(
            "INSERT INTO verses (verse_id, translation_id, book_osis, chapter, verse, text, canonical_order)
             VALUES (?1, 1, ?2, ?3, ?4, ?5, 0)",
            rusqlite::params![verse_id, parts[0], parts[1], parts[2], text],
        )
        .unwrap();
    }

    /// Stand in for the worker: a note made of one chunk.
    fn note(conn: &Connection, title: &str, text: &str) -> i64 {
        conn.execute("INSERT INTO notes (title, body) VALUES (?1, ?2)", rusqlite::params![title, text])
            .unwrap();
        let note_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO sources (kind, note_id, text_hash) VALUES ('note', ?1, 'h')",
            [note_id],
        )
        .unwrap();
        let source_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO chunks (source_id, idx, char_start, char_end, text) VALUES (?1, 0, 0, ?2, ?3)",
            rusqlite::params![source_id, text.chars().count() as i64, text],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn run(conn: &Connection, query: &str) -> (Vec<NoteHit>, Vec<VerseHit>) {
        search(conn, query, 1, None, &VectorCache::default(), DEFAULT_FLOOR).unwrap()
    }

    fn marked(snippet: &str, marks: &[(i64, i64)]) -> Vec<String> {
        let chars: Vec<char> = snippet.chars().collect();
        marks
            .iter()
            .map(|(s, e)| chars[*s as usize..*e as usize].iter().collect())
            .collect()
    }

    #[test]
    fn an_unknown_topic_finds_nothing_rather_than_something() {
        let conn = fixture();
        note(&conn, "Bund", "Der Bund mit Abraham.");
        verse(&conn, "Gen.1.1", "Im Anfang schuf Gott die Himmel und die Erde.");

        let (notes, bible) = run(&conn, "Raumschiff");

        assert!(notes.is_empty());
        assert!(bible.is_empty());
    }

    #[test]
    fn results_are_quoted_whole_with_the_words_marked() {
        let conn = fixture();
        note(&conn, "Gnade", "Aus Gnaden seid ihr errettet, nicht aus Werken.");
        verse(&conn, "Eph.2.8", "Denn durch die Gnade seid ihr errettet.");

        let (notes, bible) = run(&conn, "gnade");

        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].snippet, "Aus Gnaden seid ihr errettet, nicht aus Werken.");
        // A prefix, so the declined form is found and marked whole.
        assert_eq!(marked(&notes[0].snippet, &notes[0].marks), vec!["Gnaden"]);
        assert_eq!(notes[0].matched_by, vec!["words"]);
        assert_eq!(bible.len(), 1);
        assert_eq!(bible[0].verse_id, "Eph.2.8");
        assert_eq!(bible[0].book_name, "Ephesians");
        assert_eq!(marked(&bible[0].snippet, &bible[0].marks), vec!["Gnade"]);
    }

    #[test]
    fn diacritics_dont_have_to_be_typed() {
        let conn = fixture();
        note(&conn, "Römer", "Im Römerbrief geht es um Glauben.");

        let (notes, _) = run(&conn, "romerbrief");

        assert_eq!(marked(&notes[0].snippet, &notes[0].marks), vec!["Römerbrief"]);
    }

    #[test]
    fn search_syntax_in_the_query_is_just_text() {
        let conn = fixture();
        note(&conn, "x", "Der Bund mit Abraham.");
        for hostile in ["\"", "Bund OR", "NEAR(", "*", "Bund\" OR \"x", "-Bund", "^"] {
            assert!(
                search(&conn, hostile, 1, None, &VectorCache::default(), DEFAULT_FLOOR).is_ok(),
                "{hostile:?} must not reach FTS as syntax"
            );
        }
    }

    #[test]
    fn verses_cited_by_matching_notes_come_with_them() {
        let conn = fixture();
        let chunk = note(&conn, "Glaube", "Abraham glaubte, siehe Röm 4,3.");
        conn.execute(
            "INSERT INTO verse_links (chunk_id, char_start, char_end, surface, book_osis, chapter, verse, verse_id)
             VALUES (?1, 23, 30, 'Röm 4,3', 'Rom', 4, 3, 'Rom.4.3')",
            [chunk],
        )
        .unwrap();
        verse(&conn, "Rom.4.3", "Denn was sagt die Schrift? Abraham aber glaubte Gott.");
        verse(&conn, "Gen.15.6", "Und er glaubte Jehova.");

        // "Abraham" is in the verse too, but the citation is the stronger tie:
        // found by words *and* cited.
        let (_, bible) = run(&conn, "Abraham");

        assert_eq!(bible[0].verse_id, "Rom.4.3");
        assert_eq!(bible[0].matched_by, vec!["words", "cited"]);
        assert_eq!(bible[0].cited_by, 1);
    }

    #[test]
    fn a_verse_accepted_for_a_matching_note_counts_as_cited() {
        let conn = fixture();
        note(&conn, "Erbe", "Das Los und das Erbe.");
        verse(&conn, "Ps.16.5", "Jehova ist das Teil meines Erbteils und meines Bechers.");
        conn.execute(
            "INSERT INTO verse_suggestions (note_id, verse_id, status) VALUES (1, 'Ps.16.5', 'accepted')",
            [],
        )
        .unwrap();

        let (_, bible) = run(&conn, "Los");

        assert_eq!(bible.len(), 1);
        assert_eq!(bible[0].matched_by, vec!["cited"]);
    }

    #[test]
    fn a_cited_verse_the_translation_lacks_is_left_out() {
        let conn = fixture();
        let chunk = note(&conn, "Glaube", "Abraham glaubte, siehe Röm 4,3.");
        conn.execute(
            "INSERT INTO verse_links (chunk_id, char_start, char_end, surface, book_osis, chapter, verse, verse_id)
             VALUES (?1, 23, 30, 'Röm 4,3', 'Rom', 4, 3, 'Rom.4.3')",
            [chunk],
        )
        .unwrap();

        let (notes, bible) = run(&conn, "Abraham");

        assert_eq!(notes.len(), 1);
        assert!(bible.is_empty(), "never shown from another translation");
    }

    #[test]
    fn a_topic_marks_the_mentions_the_worker_recorded() {
        let conn = fixture();
        let chunk = note(&conn, "Herr", "Dem Herrn sei Dank.");
        conn.execute("INSERT INTO concepts (label, key) VALUES ('Herr', 'herr')", []).unwrap();
        conn.execute(
            "INSERT INTO concept_mentions (concept_id, chunk_id, char_start, char_end, surface, extracted_by)
             VALUES (1, ?1, 4, 9, 'Herrn', 'phrases')",
            [chunk],
        )
        .unwrap();

        let (notes, _) = run(&conn, "Herr");

        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].matched_by, vec!["words", "topic"]);
        assert_eq!(marked(&notes[0].snippet, &notes[0].marks), vec!["Herrn"]);
    }

    #[test]
    fn meaning_adds_passages_but_only_above_the_floor() {
        let conn = fixture();
        let near = note(&conn, "Gunst", "Unverdiente Gunst.");
        let far = note(&conn, "Auto", "Reifenwechsel.");
        verse(&conn, "Eph.2.8", "Denn durch die Gnade seid ihr errettet.");
        verse(&conn, "Gen.1.1", "Im Anfang schuf Gott.");
        let pack = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        for (chunk, v) in [(near, [1.0f32, 0.0]), (far, [0.0, 1.0])] {
            conn.execute(
                "UPDATE chunks SET embedding = ?1, embedded_by = 'm' WHERE chunk_id = ?2",
                rusqlite::params![pack(&v), chunk],
            )
            .unwrap();
        }
        for (id, v) in [("Eph.2.8", [0.9f32, 0.436]), ("Gen.1.1", [0.0, 1.0])] {
            conn.execute(
                "INSERT INTO verse_vectors (translation_id, verse_id, model, vector) VALUES (1, ?1, 'm', ?2)",
                rusqlite::params![id, pack(&v)],
            )
            .unwrap();
        }
        let query = [1.0f32, 0.0];

        let (notes, bible) = search(
            &conn, "Gnade", 1, Some(("m", &query)), &VectorCache::default(), DEFAULT_FLOOR,
        )
        .unwrap();

        assert_eq!(notes.len(), 1, "the unrelated note stays below the floor");
        assert_eq!(notes[0].snippet, "Unverdiente Gunst.");
        assert_eq!(notes[0].matched_by, vec!["meaning"]);
        assert!(notes[0].marks.is_empty(), "nothing to mark: the words differ");
        assert_eq!(bible.len(), 1);
        assert_eq!(bible[0].matched_by, vec!["words", "meaning"]);
    }

    #[test]
    fn vectors_from_another_model_are_not_compared() {
        let conn = fixture();
        let chunk = note(&conn, "Gunst", "Unverdiente Gunst.");
        let blob: Vec<u8> = [1.0f32, 0.0].iter().flat_map(|x| x.to_le_bytes()).collect();
        conn.execute(
            "UPDATE chunks SET embedding = ?1, embedded_by = 'old' WHERE chunk_id = ?2",
            rusqlite::params![blob, chunk],
        )
        .unwrap();

        let (notes, _) = search(
            &conn, "Gnade", 1, Some(("new", &[1.0, 0.0])), &VectorCache::default(), DEFAULT_FLOOR,
        )
        .unwrap();

        assert!(notes.is_empty());
    }

    #[test]
    fn the_verse_cache_follows_the_table() {
        let conn = fixture();
        verse(&conn, "Gen.1.1", "a");
        verse(&conn, "Gen.1.2", "b");
        let blob: Vec<u8> = [1.0f32, 0.0].iter().flat_map(|x| x.to_le_bytes()).collect();
        let cache = VectorCache::default();
        conn.execute(
            "INSERT INTO verse_vectors VALUES (1, 'Gen.1.1', 'm', ?1)",
            [&blob],
        )
        .unwrap();
        assert_eq!(cache.nearest(&conn, 1, "m", &[1.0, 0.0], 0.5).unwrap().len(), 1);

        conn.execute("INSERT INTO verse_vectors VALUES (1, 'Gen.1.2', 'm', ?1)", [&blob])
            .unwrap();

        assert_eq!(cache.nearest(&conn, 1, "m", &[1.0, 0.0], 0.5).unwrap().len(), 2);
    }

    #[test]
    fn coverage_counts_what_the_model_has_read() {
        let conn = fixture();
        verse(&conn, "Gen.1.1", "a");
        verse(&conn, "Gen.1.2", "b");
        note(&conn, "n", "text");
        let blob: Vec<u8> = [1.0f32].iter().flat_map(|x| x.to_le_bytes()).collect();
        conn.execute("INSERT INTO verse_vectors VALUES (1, 'Gen.1.1', 'm', ?1)", [&blob])
            .unwrap();

        assert_eq!(coverage(&conn, 1, "m").unwrap(), (0, 1, 1, 2));
    }
}
