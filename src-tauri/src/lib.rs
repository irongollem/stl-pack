mod compressors;
mod file;
mod image;
mod models;
mod settings;
mod error;

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
use specta_typescript::Typescript;
use tauri_specta::{collect_commands, Builder};
use crate::image::handling::store_image;
use crate::file::commands::{store_model_file, save_model, create_release, finalize_release};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = Builder::<tauri::Wry>::new().commands(collect_commands![
        store_image,
        store_model_file,
        save_model,
        create_release,
        finalize_release,
        settings::get_settings,
        settings::set_settings,
    ]);

    #[cfg(debug_assertions)]
    builder
        .export(Typescript::default(), "../src/bindings.ts")
        .expect("failed to write typescript bindings");

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .invoke_handler(tauri::generate_handler![
            store_image,
            store_model_file,
            save_model,
            create_release,
            finalize_release,
            settings::get_settings,
            settings::set_settings,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
