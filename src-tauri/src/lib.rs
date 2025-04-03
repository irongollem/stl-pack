mod compressors;
mod error;
mod file;
mod image;
mod models;
mod settings;

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
use crate::file::commands::{create_release, finalize_release, save_model, store_model_file};
use crate::image::handling::store_image;
use specta_typescript::Typescript;
use tauri_specta::{collect_commands, Builder};

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
        .setup(|app| {
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                match settings::get_settings(app_handle).await {
                    Ok(settings) => println!("Settings loaded succesfully: {:?}", settings),
                    Err(err) => eprintln!("Failed to load settings: {}", err),
                }
            });
            Ok(())
        })
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
