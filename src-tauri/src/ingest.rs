//! Ingestion: uploaded documents and the job state machine around them.
//!
//! Rust owns everything a *person* does — uploading, reviewing, verifying,
//! retrying. The Python worker owns the machine stages (extraction, and later
//! OCR/ASR/concepts). Both talk to this same SQLite file; neither calls the
//! other.
//!
//! The invariant this module exists to protect: **a note is only ever created
//! from text a human has accepted.** `save_verification` is the only door from
//! machine output to `VERIFIED`, and the worker refuses to write a note for a
//! job whose extraction isn't verified.

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use std::fmt;

/// Where a job is in the pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    /// File is stored; nothing has read it yet.
    Uploaded,
    /// A worker has claimed it and is pulling text out.
    Extracting,
    /// Text exists and is waiting for a human to read it.
    NeedsReview,
    /// A human accepted the text. Only now may it become a note.
    Verified,
    /// The note exists. Terminal for Phase 3.
    Done,
    /// A stage failed. Retryable.
    Error,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Uploaded => "UPLOADED",
            JobState::Extracting => "EXTRACTING",
            JobState::NeedsReview => "NEEDS_REVIEW",
            JobState::Verified => "VERIFIED",
            JobState::Done => "DONE",
            JobState::Error => "ERROR",
        }
    }

    pub fn parse(s: &str) -> Result<JobState, String> {
        match s {
            "UPLOADED" => Ok(JobState::Uploaded),
            "EXTRACTING" => Ok(JobState::Extracting),
            "NEEDS_REVIEW" => Ok(JobState::NeedsReview),
            "VERIFIED" => Ok(JobState::Verified),
            "DONE" => Ok(JobState::Done),
            "ERROR" => Ok(JobState::Error),
            other => Err(format!("unknown job state '{other}'")),
        }
    }
}

impl fmt::Display for JobState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Payloads
// ---------------------------------------------------------------------------

/// Metadata captured at upload, carried into the note and its citations.
#[derive(serde::Deserialize, Clone, Default)]
pub struct DocumentMeta {
    pub title: Option<String>,
    pub doc_date: Option<String>,
    pub speaker: Option<String>,
    pub context: Option<String>,
}

#[derive(Serialize)]
pub struct Job {
    pub job_id: i64,
    pub doc_id: i64,
    pub state: String,
    pub engine_used: Option<String>,
    pub confidence: Option<f64>,
    pub attempts: i64,
    pub last_error: Option<String>,
    pub note_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
    // Document
    pub filename: String,
    pub format: String,
    pub byte_size: i64,
    pub title: Option<String>,
    pub doc_date: Option<String>,
    pub speaker: Option<String>,
    pub context: Option<String>,
    /// Whether an extraction exists, and whether a human signed it off.
    pub has_text: bool,
    pub verified: bool,
    pub edited: bool,
    /// First line or so, for the job list.
    pub preview: Option<String>,
}

/// A page of a scanned document, as shown in review.
#[derive(Serialize)]
pub struct Page {
    pub page_id: i64,
    pub page_no: i64,
    pub image_path: String,
    pub width: i64,
    pub height: i64,
}

/// A recognised piece of text, anchored where it was found: a box on the page
/// for a scan, a stretch of time for a recording.
#[derive(Serialize)]
pub struct Span {
    pub span_id: i64,
    pub page_id: Option<i64>,
    pub page_no: Option<i64>,
    pub idx: i64,
    pub text: String,
    pub confidence: Option<f64>,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub w: Option<f64>,
    pub h: Option<f64>,
    pub start_s: Option<f64>,
    pub end_s: Option<f64>,
    pub speaker: Option<String>,
    pub edited: bool,
}

/// One span's text as the reviewer left it.
#[derive(serde::Deserialize)]
pub struct SpanEdit {
    pub span_id: i64,
    pub text: String,
}

#[derive(Serialize)]
pub struct JobDetail {
    #[serde(flatten)]
    pub job: Job,
    pub text: Option<String>,
    /// "typed" | "image" | "audio" — which review UI this job needs.
    pub kind: String,
    pub pages: Vec<Page>,
    pub spans: Vec<Span>,
}

const JOB_COLUMNS: &str = "SELECT j.job_id, j.doc_id, j.state, j.engine_used, j.confidence,
            j.attempts, j.last_error, j.note_id, j.created_at, j.updated_at,
            d.filename, d.format, d.byte_size, d.title, d.doc_date, d.speaker,
            d.context, e.text, e.verified, e.edited
       FROM jobs j
       JOIN documents d ON d.doc_id = j.doc_id
       LEFT JOIN extractions e ON e.job_id = j.job_id";

fn map_job(r: &rusqlite::Row) -> rusqlite::Result<(Job, Option<String>)> {
    let text: Option<String> = r.get(17)?;
    let preview = text.as_deref().map(|t| {
        let flat = t.split_whitespace().collect::<Vec<_>>().join(" ");
        if flat.chars().count() > 140 {
            format!("{}…", flat.chars().take(140).collect::<String>())
        } else {
            flat
        }
    });
    let job = Job {
        job_id: r.get(0)?,
        doc_id: r.get(1)?,
        state: r.get(2)?,
        engine_used: r.get(3)?,
        confidence: r.get(4)?,
        attempts: r.get(5)?,
        last_error: r.get(6)?,
        note_id: r.get(7)?,
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
        filename: r.get(10)?,
        format: r.get(11)?,
        byte_size: r.get(12)?,
        title: r.get(13)?,
        doc_date: r.get(14)?,
        speaker: r.get(15)?,
        context: r.get(16)?,
        has_text: text.is_some(),
        verified: r.get::<_, Option<i64>>(18)?.unwrap_or(0) == 1,
        edited: r.get::<_, Option<i64>>(19)?.unwrap_or(0) == 1,
        preview,
    };
    Ok((job, text))
}

// ---------------------------------------------------------------------------
// Upload
// ---------------------------------------------------------------------------

/// Typed documents — text comes straight out of the file.
pub const TYPED_FORMATS: [&str; 4] = ["txt", "md", "docx", "pdf"];
/// Scans and photos — text comes from OCR, with positions on the page.
pub const IMAGE_FORMATS: [&str; 6] = ["jpg", "jpeg", "png", "heic", "tiff", "tif"];
/// Recordings — text comes from transcription, with times and speakers.
pub const AUDIO_FORMATS: [&str; 5] = ["mp3", "m4a", "wav", "aiff", "flac"];

/// What kind of source a document is, which decides both the engine that reads
/// it and the shape of the review UI.
pub fn kind_of(format: &str) -> &'static str {
    if IMAGE_FORMATS.contains(&format) {
        "image"
    } else if AUDIO_FORMATS.contains(&format) {
        "audio"
    } else {
        "typed"
    }
}

pub fn format_of(filename: &str) -> Result<String, String> {
    let ext = filename
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    if TYPED_FORMATS.contains(&ext.as_str())
        || IMAGE_FORMATS.contains(&ext.as_str())
        || AUDIO_FORMATS.contains(&ext.as_str())
    {
        Ok(ext)
    } else {
        Err(format!(
            "'{filename}' isn't a format Rooted can read (documents: {}; scans: {}; audio: {})",
            TYPED_FORMATS.join(", "),
            IMAGE_FORMATS.join(", "),
            AUDIO_FORMATS.join(", "),
        ))
    }
}

/// Record an uploaded document and queue its job. The bytes are already on disk
/// at `stored_path`.
pub fn create_document(
    conn: &mut Connection,
    filename: &str,
    stored_path: &str,
    format: &str,
    byte_size: i64,
    sha256: &str,
    meta: &DocumentMeta,
) -> Result<i64, String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO documents
           (filename, stored_path, format, byte_size, sha256, title, doc_date, speaker, context)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            filename,
            stored_path,
            format,
            byte_size,
            sha256,
            meta.title,
            meta.doc_date,
            meta.speaker,
            meta.context,
        ],
    )
    .map_err(|e| e.to_string())?;
    let doc_id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO jobs (doc_id, state) VALUES (?1, ?2)",
        rusqlite::params![doc_id, JobState::Uploaded.as_str()],
    )
    .map_err(|e| e.to_string())?;
    let job_id = tx.last_insert_rowid();
    tx.commit().map_err(|e| e.to_string())?;
    Ok(job_id)
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

pub fn list_jobs(conn: &Connection) -> Result<Vec<Job>, String> {
    let mut stmt = conn
        .prepare(&format!("{JOB_COLUMNS} ORDER BY j.created_at DESC, j.job_id DESC"))
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| map_job(r).map(|(job, _)| job))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>();
    rows.map_err(|e| e.to_string())
}

pub fn get_job(conn: &Connection, job_id: i64) -> Result<JobDetail, String> {
    let mut stmt = conn
        .prepare(&format!("{JOB_COLUMNS} WHERE j.job_id = ?1"))
        .map_err(|e| e.to_string())?;
    let (job, text) = stmt
        .query_row(rusqlite::params![job_id], map_job)
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("job {job_id} not found"))?;

    let kind = kind_of(&job.format).to_string();
    let pages = list_pages(conn, job.doc_id)?;
    let spans = list_spans(conn, job.doc_id)?;
    Ok(JobDetail {
        job,
        text,
        kind,
        pages,
        spans,
    })
}

pub fn list_pages(conn: &Connection, doc_id: i64) -> Result<Vec<Page>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT page_id, page_no, image_path, width, height FROM pages
              WHERE doc_id = ?1 ORDER BY page_no",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![doc_id], |r| {
            Ok(Page {
                page_id: r.get(0)?,
                page_no: r.get(1)?,
                image_path: r.get(2)?,
                width: r.get(3)?,
                height: r.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>();
    rows.map_err(|e| e.to_string())
}

pub fn list_spans(conn: &Connection, doc_id: i64) -> Result<Vec<Span>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT s.span_id, s.page_id, p.page_no, s.idx, s.text, s.confidence,
                    s.x, s.y, s.w, s.h, s.start_s, s.end_s, s.speaker, s.edited
               FROM spans s
               LEFT JOIN pages p ON p.page_id = s.page_id
              WHERE s.doc_id = ?1
              ORDER BY s.idx",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![doc_id], |r| {
            Ok(Span {
                span_id: r.get(0)?,
                page_id: r.get(1)?,
                page_no: r.get(2)?,
                idx: r.get(3)?,
                text: r.get(4)?,
                confidence: r.get(5)?,
                x: r.get(6)?,
                y: r.get(7)?,
                w: r.get(8)?,
                h: r.get(9)?,
                start_s: r.get(10)?,
                end_s: r.get(11)?,
                speaker: r.get(12)?,
                edited: r.get::<_, i64>(13)? == 1,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>();
    rows.map_err(|e| e.to_string())
}

/// The stored file path, so the caller can delete it with the record.
pub fn stored_path(conn: &Connection, job_id: i64) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT d.stored_path FROM jobs j JOIN documents d ON d.doc_id = j.doc_id
          WHERE j.job_id = ?1",
        rusqlite::params![job_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Human transitions
// ---------------------------------------------------------------------------

fn current_state(conn: &Connection, job_id: i64) -> Result<JobState, String> {
    let raw: String = conn
        .query_row(
            "SELECT state FROM jobs WHERE job_id = ?1",
            rusqlite::params![job_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("job {job_id} not found"))?;
    JobState::parse(&raw)
}

/// Accept the extracted text — possibly after correcting it — and move the job
/// to `VERIFIED`. This is the only path from machine output to a note.
///
/// Editing is allowed from `NEEDS_REVIEW` and from `VERIFIED` (a correction
/// before the worker has written the note). Once the job is `DONE` the note
/// itself is the thing to edit, so this refuses.
pub fn save_verification(conn: &mut Connection, job_id: i64, text: &str) -> Result<(), String> {
    let state = current_state(conn, job_id)?;
    if !matches!(state, JobState::NeedsReview | JobState::Verified) {
        return Err(format!(
            "job {job_id} is {state}; only a job awaiting review can be verified"
        ));
    }
    if text.trim().is_empty() {
        return Err("a verified document can't be empty".into());
    }

    let original: Option<String> = conn
        .query_row(
            "SELECT text FROM extractions WHERE job_id = ?1",
            rusqlite::params![job_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let original = original.ok_or_else(|| format!("job {job_id} has no extracted text"))?;

    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "UPDATE extractions
            SET text = ?2, verified = 1, edited = CASE WHEN ?3 THEN 1 ELSE edited END,
                updated_at = datetime('now')
          WHERE job_id = ?1",
        rusqlite::params![job_id, text, text != original],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "UPDATE jobs SET state = ?2, last_error = NULL, updated_at = datetime('now')
          WHERE job_id = ?1",
        rusqlite::params![job_id, JobState::Verified.as_str()],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

/// Accept a scan or recording, span by span.
///
/// Each span keeps its own text, box and confidence, so corrections stay
/// attached to the place they belong on the page. The extraction text — what
/// becomes the note body and what search will index — is rebuilt from the spans
/// in reading order, so it can never drift from what the reviewer actually saw.
///
/// A span may be emptied (a stray mark the engine read as a word); it is kept,
/// so the page's spans still line up with what's on it, but contributes nothing.
pub fn save_span_verification(
    conn: &mut Connection,
    job_id: i64,
    edits: &[SpanEdit],
) -> Result<(), String> {
    let state = current_state(conn, job_id)?;
    if !matches!(state, JobState::NeedsReview | JobState::Verified) {
        return Err(format!(
            "job {job_id} is {state}; only a job awaiting review can be verified"
        ));
    }
    let doc_id: i64 = conn
        .query_row(
            "SELECT doc_id FROM jobs WHERE job_id = ?1",
            rusqlite::params![job_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;

    if edits.iter().all(|e| e.text.trim().is_empty()) {
        return Err("a verified document can't be empty".into());
    }

    let tx = conn.transaction().map_err(|e| e.to_string())?;
    {
        let mut update = tx
            .prepare(
                "UPDATE spans
                    SET text = ?2,
                        edited = CASE WHEN text <> ?2 THEN 1 ELSE edited END
                  WHERE span_id = ?1 AND doc_id = ?3",
            )
            .map_err(|e| e.to_string())?;
        for edit in edits {
            let changed = update
                .execute(rusqlite::params![edit.span_id, edit.text, doc_id])
                .map_err(|e| e.to_string())?;
            if changed == 0 {
                return Err(format!(
                    "span {} does not belong to this document",
                    edit.span_id
                ));
            }
        }
    }

    // Rebuild the reviewed text from the spans themselves.
    let text: String = {
        let mut stmt = tx
            .prepare(
                "SELECT text FROM spans WHERE doc_id = ?1 AND trim(text) <> '' ORDER BY idx",
            )
            .map_err(|e| e.to_string())?;
        let lines = stmt
            .query_map(rusqlite::params![doc_id], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        lines.join("\n")
    };

    tx.execute(
        "UPDATE extractions SET text = ?2, verified = 1,
                edited = (SELECT MAX(edited) FROM spans WHERE doc_id = ?3),
                updated_at = datetime('now')
          WHERE job_id = ?1",
        rusqlite::params![job_id, text, doc_id],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "UPDATE jobs SET state = ?2, last_error = NULL, updated_at = datetime('now')
          WHERE job_id = ?1",
        rusqlite::params![job_id, JobState::Verified.as_str()],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

/// Push a failed job back to the front of the queue.
/// Ask for this scan's lines to be re-read off the machine.
///
/// Separate from `retry_job` on purpose. Retrying is free and local; this sends
/// cropped lines to a third party, so it is its own explicit action, recorded
/// on the job, and the worker clears the flag the moment it acts on it. A retry
/// afterwards re-reads on this machine only.
pub fn escalate_job(conn: &Connection, job_id: i64) -> Result<(), String> {
    let state = current_state(conn, job_id)?;
    if !matches!(state, JobState::Error | JobState::NeedsReview) {
        return Err(format!("job {job_id} is {state}; nothing to re-read"));
    }
    // Only a page has lines to crop. Asking whether pages were stored is the
    // exact question — a scanned PDF has them and a typed one doesn't, which
    // the file extension alone can't tell you.
    let pages: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pages p JOIN jobs j ON j.doc_id = p.doc_id
              WHERE j.job_id = ?1",
            [job_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if pages == 0 {
        return Err(
            "this was read as text on this machine; there are no page images to send"
                .into(),
        );
    }
    conn.execute(
        "UPDATE jobs
            SET escalate = 1, state = ?2, last_error = NULL,
                claimed_by = NULL, claimed_at = NULL, updated_at = datetime('now')
          WHERE job_id = ?1",
        rusqlite::params![job_id, JobState::Uploaded.as_str()],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn retry_job(conn: &Connection, job_id: i64) -> Result<(), String> {
    let state = current_state(conn, job_id)?;
    if !matches!(state, JobState::Error | JobState::NeedsReview) {
        return Err(format!("job {job_id} is {state}; nothing to retry"));
    }
    conn.execute(
        "UPDATE jobs
            SET state = ?2, last_error = NULL, claimed_by = NULL, claimed_at = NULL,
                updated_at = datetime('now')
          WHERE job_id = ?1",
        rusqlite::params![job_id, JobState::Uploaded.as_str()],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Delete a job.
///
/// A note that was already created is left alone — it is the user's, not the
/// pipeline's. If that note came off a scanned page, the document, its page
/// images and its spans are kept too, because the note *is* the page plus what
/// was read off it. Otherwise the source document goes with the job.
///
/// Returns the file paths that are no longer referenced, for the caller to
/// remove from disk.
pub fn delete_job(conn: &mut Connection, job_id: i64) -> Result<Vec<String>, String> {
    let row: Option<(i64, Option<i64>)> = conn
        .query_row(
            "SELECT doc_id, note_id FROM jobs WHERE job_id = ?1",
            rusqlite::params![job_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let (doc_id, note_id) = row.ok_or_else(|| format!("job {job_id} not found"))?;

    // Is the document still the source of a note?
    let referenced: bool = note_id.is_some()
        && conn
            .query_row(
                "SELECT 1 FROM notes WHERE doc_id = ?1",
                rusqlite::params![doc_id],
                |_| Ok(true),
            )
            .optional()
            .map_err(|e| e.to_string())?
            .unwrap_or(false);

    let mut orphaned = Vec::new();
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    if !referenced {
        let mut stmt = tx
            .prepare("SELECT stored_path FROM documents WHERE doc_id = ?1")
            .map_err(|e| e.to_string())?;
        orphaned.extend(
            stmt.query_map(rusqlite::params![doc_id], |r| r.get::<_, String>(0))
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?,
        );
        let mut stmt = tx
            .prepare("SELECT image_path FROM pages WHERE doc_id = ?1")
            .map_err(|e| e.to_string())?;
        orphaned.extend(
            stmt.query_map(rusqlite::params![doc_id], |r| r.get::<_, String>(0))
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?,
        );
        tx.execute(
            "DELETE FROM documents WHERE doc_id = ?1",
            rusqlite::params![doc_id],
        )
        .map_err(|e| e.to_string())?;
    } else {
        tx.execute("DELETE FROM jobs WHERE job_id = ?1", rusqlite::params![job_id])
            .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(orphaned)
}

/// Counts by state, for the pipeline summary strip.
pub fn job_counts(conn: &Connection) -> Result<Vec<(String, i64)>, String> {
    let mut stmt = conn
        .prepare("SELECT state, COUNT(*) FROM jobs GROUP BY state")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>();
    rows.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    /// Stand in for the worker having read a scan: a page and one span.
    fn add_page(conn: &Connection, doc_id: i64) {
        conn.execute(
            "INSERT INTO pages (doc_id, page_no, image_path, width, height)
             VALUES (?1, 1, '/tmp/page.png', 1000, 1400)",
            [doc_id],
        )
        .unwrap();
    }

    fn fixture() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        db::apply_schema(&conn).unwrap();
        let meta = DocumentMeta {
            title: Some("Sunday talk".into()),
            doc_date: Some("2026-08-16".into()),
            speaker: Some("A. Speaker".into()),
            context: Some("Notes from the evening service".into()),
        };
        create_document(&mut conn, "talk.txt", "/tmp/talk.txt", "txt", 42, "abc", &meta).unwrap();
        conn
    }

    /// Stand in for the worker's extraction stage.
    fn extract(conn: &Connection, job_id: i64, text: &str, confidence: f64) {
        conn.execute(
            "INSERT INTO extractions (job_id, text, engine, confidence)
             VALUES (?1, ?2, 'test', ?3)",
            rusqlite::params![job_id, text, confidence],
        )
        .unwrap();
        conn.execute(
            "UPDATE jobs SET state = 'NEEDS_REVIEW', confidence = ?2 WHERE job_id = ?1",
            rusqlite::params![job_id, confidence],
        )
        .unwrap();
    }

    #[test]
    fn upload_queues_a_job_with_its_metadata() {
        let conn = fixture();
        let jobs = list_jobs(&conn).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].state, "UPLOADED");
        assert_eq!(jobs[0].filename, "talk.txt");
        assert_eq!(jobs[0].speaker.as_deref(), Some("A. Speaker"));
        assert!(!jobs[0].has_text);
        assert!(!jobs[0].verified);
    }

    #[test]
    fn formats_map_to_the_engine_and_review_ui_they_need() {
        assert_eq!(format_of("notes.TXT").unwrap(), "txt");
        assert_eq!(format_of("talk.docx").unwrap(), "docx");
        assert_eq!(format_of("scan.JPG").unwrap(), "jpg");
        assert_eq!(format_of("sermon.mp3").unwrap(), "mp3");

        assert_eq!(kind_of("txt"), "typed");
        assert_eq!(kind_of("pdf"), "typed");
        assert_eq!(kind_of("jpg"), "image");
        assert_eq!(kind_of("heic"), "image");
        assert_eq!(kind_of("mp3"), "audio");
        assert_eq!(kind_of("wav"), "audio");

        assert!(format_of("clip.mov").is_err(), "video isn't supported");
        assert!(format_of("noextension").is_err());
    }

    #[test]
    fn verification_is_the_only_route_to_verified() {
        let mut conn = fixture();
        let job_id = list_jobs(&conn).unwrap()[0].job_id;

        // Straight from UPLOADED, there's nothing a human could have read.
        assert!(save_verification(&mut conn, job_id, "anything").is_err());

        extract(&conn, job_id, "In the beginning", 1.0);
        save_verification(&mut conn, job_id, "In the beginning").unwrap();

        let detail = get_job(&conn, job_id).unwrap();
        assert_eq!(detail.job.state, "VERIFIED");
        assert!(detail.job.verified);
        assert!(!detail.job.edited, "accepting unchanged text isn't an edit");
        assert_eq!(detail.text.as_deref(), Some("In the beginning"));
    }

    #[test]
    fn corrections_are_recorded_as_edits() {
        let mut conn = fixture();
        let job_id = list_jobs(&conn).unwrap()[0].job_id;
        extract(&conn, job_id, "ln the beginnlng", 0.7);

        save_verification(&mut conn, job_id, "In the beginning").unwrap();
        let detail = get_job(&conn, job_id).unwrap();
        assert_eq!(detail.text.as_deref(), Some("In the beginning"));
        assert!(detail.job.edited);

        // A second pass before the note is written is still allowed.
        save_verification(&mut conn, job_id, "In the beginning, God").unwrap();
        assert_eq!(
            get_job(&conn, job_id).unwrap().text.as_deref(),
            Some("In the beginning, God")
        );
    }

    #[test]
    fn empty_text_is_never_verifiable() {
        let mut conn = fixture();
        let job_id = list_jobs(&conn).unwrap()[0].job_id;
        extract(&conn, job_id, "something", 1.0);
        assert!(save_verification(&mut conn, job_id, "   ").is_err());
        assert_eq!(get_job(&conn, job_id).unwrap().job.state, "NEEDS_REVIEW");
    }

    #[test]
    fn a_finished_job_cannot_be_re_verified() {
        let mut conn = fixture();
        let job_id = list_jobs(&conn).unwrap()[0].job_id;
        extract(&conn, job_id, "text", 1.0);
        save_verification(&mut conn, job_id, "text").unwrap();
        conn.execute("UPDATE jobs SET state = 'DONE' WHERE job_id = ?1", [job_id])
            .unwrap();

        let err = save_verification(&mut conn, job_id, "changed").unwrap_err();
        assert!(err.contains("DONE"), "unexpected error: {err}");
    }

    #[test]
    fn failed_jobs_requeue_and_clear_their_claim() {
        let conn = fixture();
        let job_id = list_jobs(&conn).unwrap()[0].job_id;
        conn.execute(
            "UPDATE jobs SET state = 'ERROR', last_error = 'boom',
                    claimed_by = 'worker-1', claimed_at = datetime('now')
              WHERE job_id = ?1",
            [job_id],
        )
        .unwrap();

        retry_job(&conn, job_id).unwrap();
        let job = &list_jobs(&conn).unwrap()[0];
        assert_eq!(job.state, "UPLOADED");
        assert!(job.last_error.is_none());
        let claimed: Option<String> = conn
            .query_row("SELECT claimed_by FROM jobs WHERE job_id = ?1", [job_id], |r| r.get(0))
            .unwrap();
        assert_eq!(claimed, None, "a requeued job must be claimable again");
    }

    #[test]
    fn deleting_a_job_keeps_the_note_it_produced() {
        let mut conn = fixture();
        let job_id = list_jobs(&conn).unwrap()[0].job_id;
        extract(&conn, job_id, "kept text", 1.0);
        save_verification(&mut conn, job_id, "kept text").unwrap();
        let note_id = db::create_note(&mut conn, None, Some("Sunday talk".into()), "kept text".into())
            .unwrap();
        conn.execute(
            "UPDATE jobs SET state = 'DONE', note_id = ?2 WHERE job_id = ?1",
            rusqlite::params![job_id, note_id],
        )
        .unwrap();

        delete_job(&mut conn, job_id).unwrap();
        assert!(list_jobs(&conn).unwrap().is_empty());
        assert_eq!(
            db::list_all_notes(&conn, 1, None, None, 10).unwrap().len(),
            1,
            "the note outlives the pipeline record"
        );
        let extractions: i64 = conn
            .query_row("SELECT COUNT(*) FROM extractions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(extractions, 0, "extraction goes with the document");
    }

    /// Stand in for the OCR stage: a page with three recognised spans.
    fn ocr(conn: &Connection, job_id: i64) -> (i64, Vec<i64>) {
        let doc_id: i64 = conn
            .query_row("SELECT doc_id FROM jobs WHERE job_id = ?1", [job_id], |r| {
                r.get(0)
            })
            .unwrap();
        conn.execute(
            "INSERT INTO pages (doc_id, page_no, image_path, width, height)
             VALUES (?1, 1, '/tmp/page1.png', 1200, 1600)",
            [doc_id],
        )
        .unwrap();
        let page_id = conn.last_insert_rowid();

        let spans = [
            ("Covenant — Abraham", 0.94, 0.10, 0.08),
            ("promise repeated", 0.71, 0.15, 0.14),
            ("see Galatians 3", 0.55, 0.60, 0.22),
        ];
        let mut ids = Vec::new();
        for (idx, (text, confidence, x, y)) in spans.iter().enumerate() {
            conn.execute(
                "INSERT INTO spans (doc_id, page_id, idx, text, confidence, x, y, w, h)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0.3, 0.04)",
                rusqlite::params![doc_id, page_id, idx as i64, text, confidence, x, y],
            )
            .unwrap();
            ids.push(conn.last_insert_rowid());
        }
        conn.execute(
            "INSERT INTO extractions (job_id, text, engine, confidence)
             VALUES (?1, ?2, 'vision/ocr', 0.55)",
            rusqlite::params![job_id, "Covenant — Abraham\npromise repeated\nsee Galatians 3"],
        )
        .unwrap();
        conn.execute(
            "UPDATE jobs SET state = 'NEEDS_REVIEW' WHERE job_id = ?1",
            [job_id],
        )
        .unwrap();
        (page_id, ids)
    }

    fn image_fixture() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        db::apply_schema(&conn).unwrap();
        create_document(
            &mut conn,
            "page.jpg",
            "/tmp/page.jpg",
            "jpg",
            1024,
            "abc",
            &DocumentMeta::default(),
        )
        .unwrap();
        conn
    }

    #[test]
    fn a_scan_keeps_its_page_and_span_positions() {
        let conn = image_fixture();
        let job_id = list_jobs(&conn).unwrap()[0].job_id;
        ocr(&conn, job_id);

        let detail = get_job(&conn, job_id).unwrap();
        assert_eq!(detail.kind, "image");
        assert_eq!(detail.pages.len(), 1);
        assert_eq!(detail.pages[0].width, 1200);
        assert_eq!(detail.spans.len(), 3);
        // Spans come back in reading order, each with its box and confidence.
        assert_eq!(detail.spans[0].text, "Covenant — Abraham");
        assert_eq!(detail.spans[0].page_no, Some(1));
        assert_eq!(detail.spans[2].x, Some(0.60));
        assert!(detail.spans[2].confidence.unwrap() < 0.6);
    }

    #[test]
    fn span_corrections_stay_in_place_and_rebuild_the_text() {
        let mut conn = image_fixture();
        let job_id = list_jobs(&conn).unwrap()[0].job_id;
        let (_, span_ids) = ocr(&conn, job_id);

        let edits = vec![
            SpanEdit { span_id: span_ids[0], text: "Covenant — Abraham".into() },
            SpanEdit { span_id: span_ids[1], text: "promise repeated to Isaac".into() },
            SpanEdit { span_id: span_ids[2], text: "see Galatians 3".into() },
        ];
        save_span_verification(&mut conn, job_id, &edits).unwrap();

        let detail = get_job(&conn, job_id).unwrap();
        assert_eq!(detail.job.state, "VERIFIED");
        assert!(detail.job.verified);
        // Only the changed span is marked edited; the others keep their history.
        assert!(!detail.spans[0].edited);
        assert!(detail.spans[1].edited);
        assert_eq!(detail.spans[1].text, "promise repeated to Isaac");
        // The note body is rebuilt from the spans, so it can't drift from them.
        assert_eq!(
            detail.text.as_deref(),
            Some("Covenant — Abraham\npromise repeated to Isaac\nsee Galatians 3")
        );
    }

    #[test]
    fn an_emptied_span_is_kept_but_contributes_nothing() {
        let mut conn = image_fixture();
        let job_id = list_jobs(&conn).unwrap()[0].job_id;
        let (_, span_ids) = ocr(&conn, job_id);

        // The engine read a stray mark as a word; the reviewer clears it.
        let edits = vec![
            SpanEdit { span_id: span_ids[0], text: "Covenant — Abraham".into() },
            SpanEdit { span_id: span_ids[1], text: "".into() },
            SpanEdit { span_id: span_ids[2], text: "see Galatians 3".into() },
        ];
        save_span_verification(&mut conn, job_id, &edits).unwrap();

        let detail = get_job(&conn, job_id).unwrap();
        assert_eq!(detail.spans.len(), 3, "the span stays, so the page still lines up");
        assert_eq!(
            detail.text.as_deref(),
            Some("Covenant — Abraham\nsee Galatians 3")
        );
    }

    #[test]
    fn spans_from_another_document_are_refused() {
        let mut conn = image_fixture();
        let job_id = list_jobs(&conn).unwrap()[0].job_id;
        ocr(&conn, job_id);
        create_document(
            &mut conn,
            "other.jpg",
            "/tmp/other.jpg",
            "jpg",
            10,
            "def",
            &DocumentMeta::default(),
        )
        .unwrap();
        let other_job = list_jobs(&conn).unwrap()[0].job_id;
        let (_, other_spans) = ocr(&conn, other_job);

        let edits = vec![SpanEdit { span_id: other_spans[0], text: "hijacked".into() }];
        assert!(save_span_verification(&mut conn, job_id, &edits).is_err());
    }

    #[test]
    fn clearing_every_span_is_not_a_verification() {
        let mut conn = image_fixture();
        let job_id = list_jobs(&conn).unwrap()[0].job_id;
        let (_, span_ids) = ocr(&conn, job_id);
        let edits: Vec<SpanEdit> = span_ids
            .iter()
            .map(|id| SpanEdit { span_id: *id, text: "  ".into() })
            .collect();
        assert!(save_span_verification(&mut conn, job_id, &edits).is_err());
        assert_eq!(get_job(&conn, job_id).unwrap().job.state, "NEEDS_REVIEW");
    }

    #[test]
    fn deleting_a_job_keeps_the_page_its_note_was_read_from() {
        let mut conn = image_fixture();
        let job_id = list_jobs(&conn).unwrap()[0].job_id;
        let (_, span_ids) = ocr(&conn, job_id);
        let edits: Vec<SpanEdit> = span_ids
            .iter()
            .map(|id| SpanEdit { span_id: *id, text: "kept".into() })
            .collect();
        save_span_verification(&mut conn, job_id, &edits).unwrap();

        let doc_id: i64 = conn
            .query_row("SELECT doc_id FROM jobs WHERE job_id = ?1", [job_id], |r| r.get(0))
            .unwrap();
        let note_id =
            db::create_note(&mut conn, None, Some("Scan".into()), "kept".into()).unwrap();
        conn.execute(
            "UPDATE notes SET doc_id = ?2 WHERE note_id = ?1",
            rusqlite::params![note_id, doc_id],
        )
        .unwrap();
        conn.execute(
            "UPDATE jobs SET state = 'DONE', note_id = ?2 WHERE job_id = ?1",
            rusqlite::params![job_id, note_id],
        )
        .unwrap();

        let orphaned = delete_job(&mut conn, job_id).unwrap();
        assert!(orphaned.is_empty(), "nothing to delete: the note still needs the page");
        let pages: i64 = conn
            .query_row("SELECT COUNT(*) FROM pages", [], |r| r.get(0))
            .unwrap();
        let spans: i64 = conn
            .query_row("SELECT COUNT(*) FROM spans", [], |r| r.get(0))
            .unwrap();
        assert_eq!((pages, spans), (1, 3), "the page and its spans outlive the job");
    }

    /// The real thing: Rust queues a document, the actual Python worker
    /// extracts it, a human verifies through the Rust path, the worker writes
    /// the note. Needs `python3`; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn end_to_end_through_the_python_worker() {
        let dir = std::env::temp_dir().join(format!("rooted-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("e2e.db");
        let doc_path = dir.join("talk.txt");
        std::fs::write(&doc_path, "In the beginnning, God created.\nSecond line.\n").unwrap();

        let worker = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../sidecar/worker.py");
        let run_worker = || {
            let out = std::process::Command::new("python3")
                .arg(&worker)
                .arg("--db")
                .arg(&db_path)
                .arg("--once")
                .output()
                .expect("python3 must be on PATH for this test");
            assert!(
                out.status.success(),
                "worker failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };

        let mut conn = Connection::open(&db_path).unwrap();
        db::apply_schema(&conn).unwrap();
        let meta = DocumentMeta {
            title: Some("Sunday talk".into()),
            doc_date: Some("2026-08-16".into()),
            speaker: Some("A. Speaker".into()),
            context: None,
        };
        let job_id = create_document(
            &mut conn,
            "talk.txt",
            doc_path.to_str().unwrap(),
            "txt",
            10,
            "hash",
            &meta,
        )
        .unwrap();

        // Stage 1: the worker extracts and stops for a human.
        run_worker();
        let detail = get_job(&conn, job_id).unwrap();
        assert_eq!(detail.job.state, "NEEDS_REVIEW");
        assert!(detail.text.unwrap().contains("beginnning"));

        // The worker must not proceed on its own, however many times it runs.
        run_worker();
        assert_eq!(get_job(&conn, job_id).unwrap().job.state, "NEEDS_REVIEW");
        let notes: i64 = conn
            .query_row("SELECT COUNT(*) FROM notes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(notes, 0, "nothing becomes a note before a human says so");

        // Stage 2: a human fixes the typo and accepts it.
        save_verification(&mut conn, job_id, "In the beginning, God created.\nSecond line.\n")
            .unwrap();
        run_worker();

        let detail = get_job(&conn, job_id).unwrap();
        assert_eq!(detail.job.state, "DONE");
        assert!(detail.job.note_id.is_some());
        let (title, body, speaker): (String, String, String) = conn
            .query_row(
                "SELECT title, body, speaker FROM notes WHERE note_id = ?1",
                [detail.job.note_id.unwrap()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(title, "Sunday talk");
        assert_eq!(speaker, "A. Speaker");
        assert!(body.starts_with("In the beginning, God"), "got: {body}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn states_round_trip() {
        for state in [
            JobState::Uploaded,
            JobState::Extracting,
            JobState::NeedsReview,
            JobState::Verified,
            JobState::Done,
            JobState::Error,
        ] {
            assert_eq!(JobState::parse(state.as_str()).unwrap(), state);
        }
        assert!(JobState::parse("SOMETHING_ELSE").is_err());
    }

    #[test]
    fn escalation_is_only_offered_for_a_page() {
        let conn = fixture();
        let job_id: i64 = conn
            .query_row("SELECT job_id FROM jobs", [], |r| r.get(0))
            .unwrap();
        extract(&conn, job_id, "already text", 1.0);

        // A typed document was never read by an engine that could be wrong
        // about it, so there is nothing to send.
        let refused = escalate_job(&conn, job_id).unwrap_err();
        assert!(refused.contains("no page images"), "{refused}");
        assert_eq!(escalate_flag(&conn, job_id), 0);
    }

    #[test]
    fn escalating_queues_the_job_and_records_the_decision() {
        let conn = fixture();
        let job_id: i64 = conn
            .query_row("SELECT job_id FROM jobs", [], |r| r.get(0))
            .unwrap();
        let doc_id: i64 = conn
            .query_row("SELECT doc_id FROM jobs", [], |r| r.get(0))
            .unwrap();
        extract(&conn, job_id, "sm dscrnble", 0.4);
        add_page(&conn, doc_id);

        escalate_job(&conn, job_id).unwrap();

        assert_eq!(current_state(&conn, job_id).unwrap(), JobState::Uploaded);
        assert_eq!(escalate_flag(&conn, job_id), 1);
    }

    #[test]
    fn a_finished_job_is_not_sent_anywhere() {
        let conn = fixture();
        let job_id: i64 = conn
            .query_row("SELECT job_id FROM jobs", [], |r| r.get(0))
            .unwrap();
        let doc_id: i64 = conn
            .query_row("SELECT doc_id FROM jobs", [], |r| r.get(0))
            .unwrap();
        add_page(&conn, doc_id);
        conn.execute("UPDATE jobs SET state = 'DONE' WHERE job_id = ?1", [job_id])
            .unwrap();

        assert!(escalate_job(&conn, job_id).is_err());
        assert_eq!(escalate_flag(&conn, job_id), 0);
    }

    /// A plain retry re-reads on this machine; it never repeats a send.
    #[test]
    fn retrying_does_not_re_escalate() {
        let conn = fixture();
        let job_id: i64 = conn
            .query_row("SELECT job_id FROM jobs", [], |r| r.get(0))
            .unwrap();
        let doc_id: i64 = conn
            .query_row("SELECT doc_id FROM jobs", [], |r| r.get(0))
            .unwrap();
        extract(&conn, job_id, "sm dscrnble", 0.4);
        add_page(&conn, doc_id);
        escalate_job(&conn, job_id).unwrap();
        // The worker acted on it and cleared the flag.
        conn.execute("UPDATE jobs SET escalate = 0, state = 'NEEDS_REVIEW' WHERE job_id = ?1", [job_id])
            .unwrap();

        retry_job(&conn, job_id).unwrap();

        assert_eq!(escalate_flag(&conn, job_id), 0);
    }

    fn escalate_flag(conn: &Connection, job_id: i64) -> i64 {
        conn.query_row("SELECT escalate FROM jobs WHERE job_id = ?1", [job_id], |r| {
            r.get(0)
        })
        .unwrap()
    }
}
