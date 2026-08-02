use crate::basecutter::cutters::MagnetSpec;
use crate::models::{CompressionType, Settings};
use once_cell::sync::Lazy;
use serde_json::json;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Wry};
use tauri_plugin_store::{Store, StoreExt as _};

pub(crate) static SETTINGS_CACHE: Lazy<Mutex<Settings>> = Lazy::new(|| {
    Mutex::new(Settings {
        scratch_dir: None,
        target_dir: None,
        compression_type: None,
        chunk_size: None,
        max_compression_threads: None,
        blender_path: None,
        catalog_root: None,
        catalog_roots: None,
        catalog_primary_root: None,
        known_designers: None,
        print_action: None,
        release_field_defaults: None,
        pack_level: None,
        pack_cleanup_after: None,
        blender_setup_acknowledged: None,
        scale_reference_path: None,
        scale_reference_height_mm: None,
        licence_path: None,
        magnet_inventory: None,
        scatter_library_dir: None,
        edge_stats_max_tris: None,
    })
});

const STORE_PATH: &str = "settings.json";

/// The designer lexicon the UI starts from before the user edits it.
pub fn default_designers() -> Vec<String> {
    crate::catalog::scanner::DEFAULT_DESIGNERS
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Starter magnet inventory (docs/BASECUTTER.md "Hollow, with magnet
/// mounts" — "ships with a starter inventory of the common hobby sizes so
/// it works before anyone opens settings"). Seeded on first load, same
/// pattern as default_designers; the user's saved list wins thereafter.
pub fn default_magnet_inventory() -> Vec<MagnetSpec> {
    [
        (5.0, 1.0),
        (5.0, 2.0),
        (6.0, 2.0),
        (8.0, 3.0),
        (10.0, 2.0),
    ]
    .into_iter()
    .map(|(diameter_mm, height_mm)| MagnetSpec {
        diameter_mm,
        height_mm,
        count: 1,
    })
    .collect()
}

/// Split out of get_settings so the decision is unit-testable without a
/// live tauri Store and AppHandle.
fn resolve_edge_stats_max_tris(stored: Option<u64>) -> u32 {
    match stored {
        Some(v) => (v as u32).max(crate::catalog::stl_facts::EDGE_STATS_MAX_TRIS),
        None => crate::catalog::geometry::recommended_edge_cap(),
    }
}

async fn get_store_arc(app_handle: &AppHandle) -> Result<Arc<Store<Wry>>, String> {
    let store_res = app_handle.get_store(STORE_PATH);
    match store_res {
        Some(store) => Ok(store),
        None => app_handle.store(STORE_PATH).map_err(|err| err.to_string()),
    }
}

fn compression_type_from_str(value: &str) -> CompressionType {
    match value {
        // "7zip" is the legacy spelling older builds wrote to the store
        "SevenZip" | "7zip" => CompressionType::SevenZip,
        _ => CompressionType::Zip,
    }
}

fn compression_type_to_str(value: &CompressionType) -> &'static str {
    match value {
        CompressionType::SevenZip => "SevenZip",
        CompressionType::Zip => "Zip",
    }
}

#[tauri::command]
#[specta::specta]
pub async fn get_settings(app_handle: AppHandle) -> Result<Settings, String> {
    let store = get_store_arc(&app_handle)
        .await
        .map_err(|_| "Failed to get store".to_string())?;

    let scratch_dir = store
        .get("scratch_dir")
        .and_then(|v| v.as_str().map(String::from));

    let target_dir = store
        .get("target_dir")
        .and_then(|v| v.as_str().map(String::from));

    let compression_type = store
        .get("compression_type")
        .and_then(|v| v.as_str().map(compression_type_from_str))
        .unwrap_or(CompressionType::Zip);

    let chunk_size = store
        .get("chunk_size")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);

    let max_compression_threads = store
        .get("max_compression_threads")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);

    let blender_path = store
        .get("blender_path")
        .and_then(|v| v.as_str().map(String::from));

    let catalog_root = store
        .get("catalog_root")
        .and_then(|v| v.as_str().map(String::from));

    // Migration happens on read: a store from a single-root build has no
    // catalog_roots key, so the old folder seeds a one-entry list. Nothing
    // is written back until the user saves — the old build keeps working
    // off catalog_root if they downgrade before touching settings.
    let catalog_roots = store
        .get("catalog_roots")
        .and_then(|v| v.as_array().cloned())
        .map(|arr| {
            arr.into_iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
        .filter(|list| !list.is_empty())
        .or_else(|| catalog_root.clone().map(|root| vec![root]));

    let catalog_primary_root = store
        .get("catalog_primary_root")
        .and_then(|v| v.as_str().map(String::from));

    let print_action = store
        .get("print_action")
        .and_then(|v| v.as_str().map(String::from));

    let release_field_defaults = store
        .get("release_field_defaults")
        .and_then(|v| serde_json::from_value(v).ok());

    let pack_level = store
        .get("pack_level")
        .and_then(|v| v.as_i64())
        .map(|v| v as i32);

    let pack_cleanup_after = store.get("pack_cleanup_after").and_then(|v| v.as_bool());

    let blender_setup_acknowledged = store
        .get("blender_setup_acknowledged")
        .and_then(|v| v.as_str().map(String::from));

    let scale_reference_path = store
        .get("scale_reference_path")
        .and_then(|v| v.as_str().map(String::from));

    let scale_reference_height_mm = store
        .get("scale_reference_height_mm")
        .and_then(|v| v.as_f64());

    let licence_path = store
        .get("licence_path")
        .and_then(|v| v.as_str().map(String::from));

    let scatter_library_dir = store
        .get("scatter_library_dir")
        .and_then(|v| v.as_str().map(String::from));

    // A read that writes: seeding and clamping are both persisted, so the
    // store converges on a valid value instead of being re-fixed every load.
    let stored_edge_cap = store.get("edge_stats_max_tris").and_then(|v| v.as_u64());
    let edge_stats_max_tris = resolve_edge_stats_max_tris(stored_edge_cap);
    if stored_edge_cap != Some(u64::from(edge_stats_max_tris)) {
        store.set("edge_stats_max_tris", json!(edge_stats_max_tris));
        store.save().ok();
    }

    // Seed the lexicon on first load so the UI has something to show and the
    // scanner has something to match; the user's saved list wins thereafter.
    let known_designers = store
        .get("known_designers")
        .and_then(|v| v.as_array().cloned())
        .map(|arr| {
            arr.into_iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
        .filter(|list| !list.is_empty())
        .unwrap_or_else(default_designers);

    // Same seed-on-first-load pattern as known_designers above: an absent
    // or empty store value (older store, or a user who cleared the list)
    // falls back to the starter inventory rather than shipping empty.
    let magnet_inventory = store
        .get("magnet_inventory")
        .and_then(|v| serde_json::from_value::<Vec<MagnetSpec>>(v).ok())
        .filter(|list| !list.is_empty())
        .unwrap_or_else(default_magnet_inventory);

    let settings = Settings {
        scratch_dir,
        target_dir,
        compression_type: Some(compression_type),
        chunk_size,
        max_compression_threads,
        blender_path,
        catalog_root,
        catalog_roots,
        catalog_primary_root,
        known_designers: Some(known_designers),
        print_action,
        release_field_defaults,
        pack_level,
        pack_cleanup_after,
        blender_setup_acknowledged,
        scale_reference_path,
        scale_reference_height_mm,
        licence_path,
        magnet_inventory: Some(magnet_inventory),
        scatter_library_dir,
        edge_stats_max_tris: Some(edge_stats_max_tris),
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
    let store = get_store_arc(&app_handle)
        .await
        .map_err(|e| e.to_string())?;

    // Some -> write, None -> delete; without the delete a cleared setting
    // would resurrect from disk on the next load
    fn set_or_delete(store: &Store<Wry>, key: &str, value: Option<serde_json::Value>) {
        match value {
            Some(v) => store.set(key, v),
            None => {
                store.delete(key);
            }
        }
    }

    set_or_delete(
        &store,
        "scratch_dir",
        settings.scratch_dir.as_deref().map(|v| json!(v)),
    );
    set_or_delete(
        &store,
        "target_dir",
        settings.target_dir.as_deref().map(|v| json!(v)),
    );
    set_or_delete(
        &store,
        "compression_type",
        settings
            .compression_type
            .as_ref()
            .map(|v| json!(compression_type_to_str(v))),
    );
    set_or_delete(&store, "chunk_size", settings.chunk_size.map(|v| json!(v)));
    set_or_delete(
        &store,
        "max_compression_threads",
        settings.max_compression_threads.map(|v| json!(v)),
    );
    set_or_delete(
        &store,
        "blender_path",
        settings.blender_path.as_deref().map(|v| json!(v)),
    );
    set_or_delete(
        &store,
        "catalog_root",
        settings.catalog_root.as_deref().map(|v| json!(v)),
    );
    set_or_delete(
        &store,
        "catalog_roots",
        settings
            .catalog_roots
            .as_ref()
            .filter(|list| !list.is_empty())
            .map(|v| json!(v)),
    );
    set_or_delete(
        &store,
        "catalog_primary_root",
        settings.catalog_primary_root.as_deref().map(|v| json!(v)),
    );
    set_or_delete(
        &store,
        "known_designers",
        settings.known_designers.as_ref().map(|v| json!(v)),
    );
    set_or_delete(
        &store,
        "print_action",
        settings.print_action.as_deref().map(|v| json!(v)),
    );
    set_or_delete(
        &store,
        "release_field_defaults",
        settings.release_field_defaults.as_ref().map(|v| json!(v)),
    );
    set_or_delete(&store, "pack_level", settings.pack_level.map(|v| json!(v)));
    set_or_delete(
        &store,
        "pack_cleanup_after",
        settings.pack_cleanup_after.map(|v| json!(v)),
    );
    set_or_delete(
        &store,
        "blender_setup_acknowledged",
        settings.blender_setup_acknowledged.as_deref().map(|v| json!(v)),
    );
    set_or_delete(
        &store,
        "scale_reference_path",
        settings.scale_reference_path.as_deref().map(|v| json!(v)),
    );
    set_or_delete(
        &store,
        "scale_reference_height_mm",
        settings.scale_reference_height_mm.map(|v| json!(v)),
    );
    set_or_delete(
        &store,
        "licence_path",
        settings.licence_path.as_deref().map(|v| json!(v)),
    );
    set_or_delete(
        &store,
        "magnet_inventory",
        settings.magnet_inventory.as_ref().map(|v| json!(v)),
    );
    set_or_delete(
        &store,
        "scatter_library_dir",
        settings.scatter_library_dir.as_deref().map(|v| json!(v)),
    );
    // Clamped on write too, not just on read — EDGE_STATS_MAX_TRIS is a
    // floor the store can never sit below, whichever path put a value there.
    set_or_delete(
        &store,
        "edge_stats_max_tris",
        settings
            .edge_stats_max_tris
            .map(|v| json!(v.max(crate::catalog::stl_facts::EDGE_STATS_MAX_TRIS))),
    );
    store.save().map_err(|e| e.to_string())?;

    // Update the cache only after the store persisted, so memory and disk
    // can't diverge on a failed save
    let mut cache = SETTINGS_CACHE
        .lock()
        .map_err(|e| format!("Failed to lock cache: {}", e))?;
    *cache = settings;

    Ok(())
}

pub fn get_optimal_thread_count() -> u32 {
    let settings_result = SETTINGS_CACHE.lock();
    if let Ok(settings) = settings_result {
        if let Some(thread_count) = settings.max_compression_threads {
            return std::cmp::max(1, thread_count);
        }
    }
    let cpu_count = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1);
    std::cmp::max(1, cpu_count.saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The starter inventory docs/BASECUTTER.md promises ("5x1, 6x2, 10x2,
    /// ...") — pinned so a seed-list edit here is a deliberate change, not
    /// an accidental drop of a size someone's boss-fit test already relies
    /// on.
    #[test]
    fn default_magnet_inventory_has_the_common_hobby_sizes() {
        let inventory = default_magnet_inventory();
        let dims: Vec<(f64, f64)> = inventory
            .iter()
            .map(|m| (m.diameter_mm, m.height_mm))
            .collect();
        assert_eq!(
            dims,
            vec![
                (5.0, 1.0),
                (5.0, 2.0),
                (6.0, 2.0),
                (8.0, 3.0),
                (10.0, 2.0),
            ]
        );
        assert!(
            inventory.iter().all(|m| m.count == 1),
            "starter entries are single magnets, not pre-set multi-boss groups"
        );
    }

    /// get_settings/set_settings round-trip magnet_inventory through the
    /// store as plain `serde_json::to_value`/`from_value::<Vec<MagnetSpec>>`
    /// (see both functions above) — pin that shape so a future MagnetSpec
    /// field change can't silently break the store round-trip without a
    /// test noticing.
    #[test]
    fn magnet_inventory_round_trips_through_json_like_the_store_does() {
        let inventory = vec![MagnetSpec {
            diameter_mm: 6.0,
            height_mm: 2.0,
            count: 1,
        }];
        let stored = serde_json::to_value(&inventory).unwrap();
        let restored: Vec<MagnetSpec> = serde_json::from_value(stored).unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].diameter_mm, 6.0);
        assert_eq!(restored[0].height_mm, 2.0);
        assert_eq!(restored[0].count, 1);
    }

    /// Comparing against a live recommended_edge_cap() is safe: it is
    /// deterministic within one test run (same process, same RAM).
    #[test]
    fn resolve_edge_stats_max_tris_seeds_the_recommendation_when_absent() {
        assert_eq!(
            resolve_edge_stats_max_tris(None),
            crate::catalog::geometry::recommended_edge_cap()
        );
    }

    #[test]
    fn resolve_edge_stats_max_tris_clamps_a_low_stored_value_to_the_floor() {
        assert_eq!(
            resolve_edge_stats_max_tris(Some(10)),
            crate::catalog::stl_facts::EDGE_STATS_MAX_TRIS
        );
    }

    #[test]
    fn resolve_edge_stats_max_tris_passes_through_a_value_at_or_above_the_floor() {
        let above_floor = u64::from(crate::catalog::stl_facts::EDGE_STATS_MAX_TRIS) + 1_000_000;
        assert_eq!(resolve_edge_stats_max_tris(Some(above_floor)), above_floor as u32);
    }
}
