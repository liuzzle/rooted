mod db;
mod ingest;
mod packs;
mod sidecar;

use db::{
    Anchor, Book, ChapterAnnotations, Db, LibraryNote, Note, RecentHighlight, Stats, Translation,
    Verse,
};
use packs::Pack;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager, State};

/// Key under which the reader's current translation is remembered.
const ACTIVE_TRANSLATION: &str = "active_translation";
/// Key under which the last chapter read is remembered (`"Gen.1"`).
const LAST_READ: &str = "last_read";

#[tauri::command]
fn list_translations(state: State<Db>) -> Result<Vec<Translation>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    db::list_translations(&conn)
}

#[tauri::command]
fn list_books(state: State<Db>, translation_id: i64) -> Result<Vec<Book>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    db::list_books(&conn, translation_id)
}

#[tauri::command]
fn get_chapter(
    state: State<Db>,
    translation_id: i64,
    book_osis: String,
    chapter: i64,
) -> Result<Vec<Verse>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    db::get_chapter(&conn, translation_id, &book_osis, chapter)
}

// --- notes & highlights ----------------------------------------------------

#[tauri::command]
fn create_note(
    state: State<Db>,
    anchor: Option<Anchor>,
    title: Option<String>,
    body: String,
) -> Result<i64, String> {
    let mut conn = state.0.lock().map_err(|e| e.to_string())?;
    db::create_note(&mut conn, anchor, title, body)
}

/// Attach a reference to a note, move it, or drop it (`anchor: null`).
#[tauri::command]
fn set_note_anchor(
    state: State<Db>,
    note_id: i64,
    anchor: Option<Anchor>,
) -> Result<(), String> {
    let mut conn = state.0.lock().map_err(|e| e.to_string())?;
    db::set_note_anchor(&mut conn, note_id, anchor)
}

#[tauri::command]
fn update_note(
    state: State<Db>,
    note_id: i64,
    title: Option<String>,
    body: String,
) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    db::update_note(&conn, note_id, title, body)
}

#[tauri::command]
fn delete_note(state: State<Db>, note_id: i64) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    db::delete_note(&conn, note_id)
}

#[tauri::command]
fn list_notes(
    state: State<Db>,
    anchor: Anchor,
    translation_id: i64,
) -> Result<Vec<Note>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    db::list_notes(&conn, anchor, translation_id)
}

#[tauri::command]
fn set_highlight(state: State<Db>, anchor: Anchor, color: String) -> Result<(), String> {
    let mut conn = state.0.lock().map_err(|e| e.to_string())?;
    db::set_highlight(&mut conn, anchor, color)
}

#[tauri::command]
fn clear_highlight(state: State<Db>, anchor: Anchor) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    db::clear_highlight(&conn, anchor)
}

#[tauri::command]
fn get_chapter_annotations(
    state: State<Db>,
    translation_id: i64,
    book_osis: String,
    chapter: i64,
) -> Result<ChapterAnnotations, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    db::chapter_annotations(&conn, translation_id, &book_osis, chapter)
}

// --- study surfaces --------------------------------------------------------

#[tauri::command]
fn list_all_notes(
    state: State<Db>,
    translation_id: i64,
    book_osis: Option<String>,
    query: Option<String>,
    limit: i64,
) -> Result<Vec<LibraryNote>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    db::list_all_notes(&conn, translation_id, book_osis, query, limit)
}

#[tauri::command]
fn list_chapter_notes(
    state: State<Db>,
    translation_id: i64,
    book_osis: String,
    chapter: i64,
) -> Result<Vec<LibraryNote>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    db::list_chapter_notes(&conn, translation_id, &book_osis, chapter)
}

#[tauri::command]
fn list_recent_highlights(
    state: State<Db>,
    translation_id: i64,
    limit: i64,
) -> Result<Vec<RecentHighlight>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    db::list_recent_highlights(&conn, translation_id, limit)
}

#[tauri::command]
fn get_stats(state: State<Db>) -> Result<Stats, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    db::stats(&conn)
}

/// Last chapter read, as `"Gen.1"`.
#[tauri::command]
fn get_last_read(state: State<Db>) -> Result<Option<String>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    db::get_setting(&conn, LAST_READ)
}

#[tauri::command]
fn set_last_read(state: State<Db>, position: String) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    db::set_setting(&conn, LAST_READ, &position)
}

// --- ingestion -------------------------------------------------------------

/// Where uploaded source files are kept, beside the database.
fn documents_dir() -> Result<std::path::PathBuf, String> {
    let dir = db::resolve_db_path()
        .parent()
        .ok_or("cannot resolve the app data directory")?
        .join("documents");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Store an uploaded typed document and queue it for extraction.
#[tauri::command]
fn upload_document(
    state: State<Db>,
    filename: String,
    bytes: Vec<u8>,
    meta: ingest::DocumentMeta,
) -> Result<i64, String> {
    let format = ingest::format_of(&filename)?;
    if bytes.is_empty() {
        return Err(format!("'{filename}' is empty"));
    }

    let digest = Sha256::digest(&bytes);
    let sha256 = format!("{digest:x}");
    // Content-addressed on disk: the same file uploaded twice can't collide,
    // and a re-upload after an edit lands beside the original.
    let stored = documents_dir()?.join(format!("{}-{}", &sha256[..12], sanitize(&filename)));
    std::fs::write(&stored, &bytes).map_err(|e| e.to_string())?;

    let mut conn = state.0.lock().map_err(|e| e.to_string())?;
    ingest::create_document(
        &mut conn,
        &filename,
        &stored.to_string_lossy(),
        &format,
        bytes.len() as i64,
        &sha256,
        &meta,
    )
}

/// Keep a recognisable filename without letting it escape the documents dir.
fn sanitize(filename: &str) -> String {
    filename
        .chars()
        .map(|c| if c.is_alphanumeric() || "._- ".contains(c) { c } else { '_' })
        .collect::<String>()
        .trim_matches(['.', ' '])
        .to_string()
}

#[tauri::command]
fn list_jobs(state: State<Db>) -> Result<Vec<ingest::Job>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    ingest::list_jobs(&conn)
}

#[tauri::command]
fn get_job(state: State<Db>, job_id: i64) -> Result<ingest::JobDetail, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    ingest::get_job(&conn, job_id)
}

/// Accept extracted text, with any corrections. The worker turns it into a note.
#[tauri::command]
fn verify_job(state: State<Db>, job_id: i64, text: String) -> Result<(), String> {
    let mut conn = state.0.lock().map_err(|e| e.to_string())?;
    ingest::save_verification(&mut conn, job_id, &text)
}

#[tauri::command]
fn retry_job(state: State<Db>, job_id: i64) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    ingest::retry_job(&conn, job_id)
}

/// Remove a job and its uploaded file. A note it already produced is kept.
#[tauri::command]
fn delete_job(state: State<Db>, job_id: i64) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let path = ingest::stored_path(&conn, job_id)?;
    ingest::delete_job(&conn, job_id)?;
    if let Some(path) = path {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

#[tauri::command]
fn worker_status(
    state: State<Db>,
    sidecar: State<sidecar::Sidecar>,
) -> Result<sidecar::WorkerStatus, String> {
    let heartbeat = {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        db::get_setting(&conn, "worker_heartbeat")?
    };
    Ok(sidecar.status(heartbeat))
}

// --- translation packs -----------------------------------------------------

#[derive(Clone, Serialize)]
struct PackProgress {
    abbrev: String,
    book: usize,
    total: usize,
    book_name: String,
    verses: i64,
}

#[tauri::command]
fn list_packs(state: State<Db>) -> Result<Vec<Pack>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    packs::list_packs(&conn)
}

/// Download a pack book-by-book, emitting `pack-progress` as it goes. A failed
/// install leaves no half-imported text behind.
#[tauri::command]
async fn install_pack(
    app: AppHandle,
    state: State<'_, Db>,
    abbrev: String,
) -> Result<i64, String> {
    let entry = packs::find_entry(&abbrev)?;
    let translation_id = {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        packs::begin_install(&conn, &entry)?
    };

    match download_pack(&app, &state, &entry, translation_id).await {
        Ok(()) => Ok(translation_id),
        Err(e) => {
            if let Ok(conn) = state.0.lock() {
                let _ = packs::clear_text(&conn, translation_id);
            }
            Err(e)
        }
    }
}

async fn download_pack(
    app: &AppHandle,
    state: &State<'_, Db>,
    entry: &packs::PackEntry,
    translation_id: i64,
) -> Result<(), String> {
    let total = packs::BOOKS.len();
    let mut canonical = 0i64;
    let mut verses = 0i64;

    for (i, (osis, book_name, _)) in packs::BOOKS.iter().enumerate() {
        let fetched = packs::fetch_book(&entry.slug, i + 1).await?;
        {
            // Scoped so the lock is never held across an await.
            let mut conn = state.0.lock().map_err(|e| e.to_string())?;
            verses += packs::insert_book(&mut conn, translation_id, osis, &fetched, &mut canonical)?
                as i64;
        }
        app.emit(
            "pack-progress",
            PackProgress {
                abbrev: entry.abbrev.clone(),
                book: i + 1,
                total,
                book_name: book_name.to_string(),
                verses,
            },
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Remove a pack's text. Notes and highlights are user data and are kept.
#[tauri::command]
fn remove_pack(state: State<Db>, abbrev: String) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    packs::remove_pack(&conn, &abbrev)
}

#[tauri::command]
fn get_active_translation(state: State<Db>) -> Result<Option<String>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    db::get_setting(&conn, ACTIVE_TRANSLATION)
}

#[tauri::command]
fn set_active_translation(state: State<Db>, abbrev: String) -> Result<(), String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    db::set_setting(&conn, ACTIVE_TRANSLATION, &abbrev)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let conn = db::open().expect("failed to open database");

    // The ingestion worker runs alongside the app, sharing the database. If it
    // can't start, uploads simply queue until one does — nothing is lost.
    let worker = sidecar::Sidecar::new();
    worker.start(&db::resolve_db_path());

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(Db(std::sync::Mutex::new(conn)))
        .manage(worker)
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                window.state::<sidecar::Sidecar>().stop();
            }
        })
        .invoke_handler(tauri::generate_handler![
            list_translations,
            list_books,
            get_chapter,
            create_note,
            update_note,
            delete_note,
            list_notes,
            set_highlight,
            clear_highlight,
            get_chapter_annotations,
            list_packs,
            install_pack,
            remove_pack,
            get_active_translation,
            set_active_translation,
            set_note_anchor,
            list_all_notes,
            list_chapter_notes,
            list_recent_highlights,
            get_stats,
            get_last_read,
            set_last_read,
            upload_document,
            list_jobs,
            get_job,
            verify_job,
            retry_job,
            delete_job,
            worker_status
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
