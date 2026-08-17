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

#[derive(Serialize)]
pub struct JobDetail {
    #[serde(flatten)]
    pub job: Job,
    pub text: Option<String>,
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

/// File extensions this phase can read. Handwriting and audio arrive in Phase 4.
pub const SUPPORTED_FORMATS: [&str; 4] = ["txt", "md", "docx", "pdf"];

pub fn format_of(filename: &str) -> Result<String, String> {
    let ext = filename
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    if SUPPORTED_FORMATS.contains(&ext.as_str()) {
        Ok(ext)
    } else {
        Err(format!(
            "'{filename}' isn't a typed document this phase can read (expected {})",
            SUPPORTED_FORMATS.join(", ")
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
    stmt.query_row(rusqlite::params![job_id], |r| {
        map_job(r).map(|(job, text)| JobDetail { job, text })
    })
    .optional()
    .map_err(|e| e.to_string())?
    .ok_or_else(|| format!("job {job_id} not found"))
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

/// Push a failed job back to the front of the queue.
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

/// Delete a job, its document row and its extraction. A note that was already
/// created is left alone — it is the user's, not the pipeline's.
pub fn delete_job(conn: &Connection, job_id: i64) -> Result<(), String> {
    let changed = conn
        .execute(
            "DELETE FROM documents WHERE doc_id = (SELECT doc_id FROM jobs WHERE job_id = ?1)",
            rusqlite::params![job_id],
        )
        .map_err(|e| e.to_string())?;
    if changed == 0 {
        return Err(format!("job {job_id} not found"));
    }
    Ok(())
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
    fn only_typed_document_formats_are_accepted() {
        assert_eq!(format_of("notes.TXT").unwrap(), "txt");
        assert_eq!(format_of("talk.docx").unwrap(), "docx");
        // Phase 4 formats are deliberately refused here.
        assert!(format_of("sermon.mp3").is_err());
        assert!(format_of("scan.jpg").is_err());
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

        delete_job(&conn, job_id).unwrap();
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
}
