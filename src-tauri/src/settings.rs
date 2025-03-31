use once_cell::sync::Lazy;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Wry};
use tauri_plugin_store::{Store, StoreExt as _};
use crate::models::{CompressionType, Settings};

pub(crate) static SETTINGS_CACHE: Lazy<Mutex<Settings>> = Lazy::new(|| {
    Mutex::new(Settings {
        scratch_dir: None,
        target_dir: None,
        compression_type: None,
        chunk_size: None,
    })
});

const STORE_PATH: &str = "settings.json";

async fn get_store_arc(app_handle: &AppHandle) -> Result<Arc<Store<Wry>>, String> {
    let store_res = app_handle.get_store(STORE_PATH);
    match store_res {
        Some(store) => Ok(store),
        None => app_handle.store(STORE_PATH).map_err(|err| err.to_string()),
    }
}
#[tauri::command]
#[specta::specta]
pub async fn get_settings(app_handle: AppHandle) -> Result<Settings, String> {
    let store = get_store_arc(&app_handle)
        .await
        .map_err(|_| "Failed to get store".to_string())?;

    // Access each setting directly using store.get()
    let scratch_dir = store
        .get("scratch_dir")
        .and_then(|v| v.as_str().map(String::from));

    let target_dir = store
        .get("target_dir")
        .and_then(|v| v.as_str().map(String::from));

    let compression_type = store
        .get("compression_type")
        .and_then(|v| v.as_str().map(String::from))
        .and_then(|s| match s.as_str() {
            "Zip" => Some(CompressionType::Zip),
            "7zip" => Some(CompressionType::SevenZip),
            "TarGz" => Some(CompressionType::TarGz),
            "TarXz" => Some(CompressionType::TarXz),
            _ => None,
        });

    let chunk_size = store
        .get("chunk_size")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);
    let settings = Settings {
        scratch_dir,
        target_dir,
        compression_type,
        chunk_size,
    };

    {
        let mut cache = SETTINGS_CACHE
            .lock()
            .map_err(|e| format!("Failed to get cache: {}", e))?;
        *cache = settings.clone();
    }

    Ok(settings)
}

#[tauri::command]
#[specta::specta]
pub async fn set_settings(app_handle: AppHandle, settings: Settings) -> Result<(), String> {
    // Update cache
    {
        let mut cache = SETTINGS_CACHE
            .lock()
            .map_err(|e| format!("Failed to lock cache: {}", e))?;
        *cache = settings.clone();
    }

    // Get store
    let store = get_store_arc(&app_handle)
        .await
        .map_err(|e| e.to_string())?;

    // Save each setting individually to match your existing pattern
    if let Some(dir) = &settings.scratch_dir {
        store.set("scratch_dir", dir.to_string());
    }
    if let Some(dir) = &settings.target_dir {
        store.set("target_dir", dir.to_string());
    }
    if let Some(compression) = &settings.compression_type {
        let compression_str = match compression {
            CompressionType::Zip => "Zip",
            CompressionType::SevenZip => "7zip",
            CompressionType::TarGz => "TarGz",
            CompressionType::TarXz => "TarXz",
        };
        store.set("compression_type", compression_str);
    }

    store.save().map_err(|e| e.to_string())
}
