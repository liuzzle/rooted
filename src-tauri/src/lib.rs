mod db;
mod packs;

use db::{Anchor, Book, ChapterAnnotations, Db, Note, Translation, Verse};
use packs::Pack;
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

/// Key under which the reader's current translation is remembered.
const ACTIVE_TRANSLATION: &str = "active_translation";

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
    anchor: Anchor,
    title: Option<String>,
    body: String,
) -> Result<i64, String> {
    let mut conn = state.0.lock().map_err(|e| e.to_string())?;
    db::create_note(&mut conn, anchor, title, body)
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

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(Db(std::sync::Mutex::new(conn)))
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
            set_active_translation
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
