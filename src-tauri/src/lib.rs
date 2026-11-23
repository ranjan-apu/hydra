//! Hydra backend: a tauri-free download engine (src/engine) plus the thin
//! Tauri bridge (src/bridge.rs). lib.rs wires plugins, managed state, and every
//! CONTRACT command into the invoke handler.

mod bridge;
pub mod engine;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(bridge::Registry::new())
        .invoke_handler(tauri::generate_handler![
            bridge::probe_url,
            bridge::start_download,
            bridge::pause_download,
            bridge::resume_download,
            bridge::cancel_download,
            bridge::pick_folder,
            bridge::reveal_path,
            bridge::get_settings,
            bridge::save_settings,
            bridge::meta_check
        ])
        .run(tauri::generate_context!())
        .expect("error while running hydra");
}
