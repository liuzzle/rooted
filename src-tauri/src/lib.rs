mod db;

use db::{Anchor, Book, ChapterAnnotations, Db, Note, Translation, Verse};
use tauri::State;

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
fn list_notes(state: State<Db>, anchor: Anchor) -> Result<Vec<Note>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    db::list_notes(&conn, anchor)
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
            get_chapter_annotations
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
