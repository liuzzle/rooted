mod db;

use db::{Book, Db, Translation, Verse};
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let conn = db::open().expect("failed to open database");

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(Db(std::sync::Mutex::new(conn)))
        .invoke_handler(tauri::generate_handler![
            list_translations,
            list_books,
            get_chapter
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
