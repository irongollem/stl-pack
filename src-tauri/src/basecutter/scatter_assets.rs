//! Scatter asset sources: the bundled (curated, embedded) set and the
//! user-library (scanned folder) set — docs/SCATTER.md "Bundled assets" /
//! "User library". Both return `ScatterAsset` rows; the bundled set is a
//! fixed, embedded table, the user-library set is measured fresh off disk
//! on every scan, no caching (a user editing their folder expects the next
//! scan to see it).

use crate::basecutter::scatter::{ScatterAsset, ScatterAssetSource};
use crate::basecutter::stl_bbox::parse_binary_stl_bbox;
use crate::error::AppError;
use std::path::{Path, PathBuf};
use tauri::AppHandle;

/// One curated bundled asset, transcribed from
/// `resources/scatter/manifest.json` (see docs/SCATTER-ASSETS.md
/// "Curation verdict"). `footprint_mm` is `max(x, y)` of the manifest's
/// canonical footprint, matching `StlBBox::footprint_mm`'s convention —
/// the manifest-drift test below asserts this table matches the shipped
/// manifest.json byte-for-byte.
struct BundledAsset {
    id: &'static str,
    label: &'static str,
    filename: &'static str,
    footprint_mm: f64,
    height_mm: f64,
    // Per-piece provenance, checked against manifest.json by the
    // manifest-drift test; not yet surfaced through any command.
    #[allow(dead_code)]
    license: &'static str,
    // A muted, tabletop-realistic sRGB hex a placed instance paints its
    // "Col" corner attribute with, before per-piece brightness jitter.
    // Picked per asset id, not one flat color for the whole table — not
    // in manifest.json, so not drift-checked against it, but checked
    // non-empty and `#rrggbb`-shaped by this file's own tests.
    color: &'static str,
    bytes: &'static [u8],
}

const SMITHSONIAN_LICENSE: &str =
    "CC0 1.0 (Smithsonian Open Access, machine-tagged \"metadata_usage.access\": \"CC0\")";

// The one non-CC0 admission in the bundle: the Printables organic-leaf set.
// CC BY-SA carries a share-alike obligation that CC0 does not (a base a
// user decorates with these inherits the license); admitted deliberately
// — see docs/SCATTER-ASSETS.md.
const LEAF_LICENSE: &str = "CC BY-SA 4.0 (Printables)";

/// The bundled set — see this module's doc comment and `BundledAsset`'s.
/// Order is cosmetic (drives the frontend's default listing order); the
/// manifest-drift test checks set membership, not order.
const BUNDLED_ASSETS: &[BundledAsset] = &[
    BundledAsset {
        id: "skull-hesperocyon",
        label: "Hesperocyon skull",
        filename: "skull-hesperocyon.stl",
        footprint_mm: 7.0,
        height_mm: 2.704,
        license: SMITHSONIAN_LICENSE,
        color: "#cfc6b0",
        bytes: include_bytes!("../../resources/scatter/skull-hesperocyon.stl"),
    },
    BundledAsset {
        id: "skull-pseudocynodictis",
        label: "Pseudocynodictis skull",
        filename: "skull-pseudocynodictis.stl",
        footprint_mm: 6.0,
        height_mm: 2.284,
        license: SMITHSONIAN_LICENSE,
        color: "#cfc6b0",
        bytes: include_bytes!("../../resources/scatter/skull-pseudocynodictis.stl"),
    },
    BundledAsset {
        id: "skull-leptophoca-seal",
        label: "Leptophoca seal skull",
        filename: "skull-leptophoca-seal.stl",
        footprint_mm: 8.0,
        height_mm: 4.411,
        license: SMITHSONIAN_LICENSE,
        // Marine-mammal bone weathers greyer than a land skull's warmer
        // ivory tones — a small distinguishing nudge off the shared
        // bone/skull base color rather than a flat reuse of it.
        color: "#c4c2b8",
        bytes: include_bytes!("../../resources/scatter/skull-leptophoca-seal.stl"),
    },
    BundledAsset {
        id: "skull-deer",
        label: "White-tailed deer skull",
        filename: "skull-deer.stl",
        footprint_mm: 9.0,
        height_mm: 3.846,
        license: SMITHSONIAN_LICENSE,
        color: "#d6c9a8",
        bytes: include_bytes!("../../resources/scatter/skull-deer.stl"),
    },
    BundledAsset {
        id: "skull-diplocaulus",
        label: "Diplocaulus (boomerang-head) skull",
        filename: "skull-diplocaulus.stl",
        footprint_mm: 11.0,
        height_mm: 1.362,
        license: SMITHSONIAN_LICENSE,
        color: "#c9c2ab",
        bytes: include_bytes!("../../resources/scatter/skull-diplocaulus.stl"),
    },
    BundledAsset {
        id: "bone-deer-mandible",
        label: "Deer mandible",
        filename: "bone-deer-mandible.stl",
        footprint_mm: 9.0,
        height_mm: 4.82,
        license: SMITHSONIAN_LICENSE,
        color: "#cfc6b0",
        bytes: include_bytes!("../../resources/scatter/bone-deer-mandible.stl"),
    },
    BundledAsset {
        id: "bone-deer-forelimb",
        label: "Deer forelimb bone",
        filename: "bone-deer-forelimb.stl",
        footprint_mm: 12.0,
        height_mm: 1.284,
        license: SMITHSONIAN_LICENSE,
        color: "#cfc6b0",
        bytes: include_bytes!("../../resources/scatter/bone-deer-forelimb.stl"),
    },
    BundledAsset {
        id: "bone-pilot-whale-mandible",
        label: "Pilot whale mandible",
        filename: "bone-pilot-whale-mandible.stl",
        footprint_mm: 16.0,
        height_mm: 4.699,
        license: SMITHSONIAN_LICENSE,
        // The "statement piece" (docs/SCATTER.md "Scale anchor") — big,
        // weathered whale bone reads greyer/more porous than a small land
        // mammal's skull.
        color: "#b8b5ab",
        bytes: include_bytes!("../../resources/scatter/bone-pilot-whale-mandible.stl"),
    },
    BundledAsset {
        id: "mushroom",
        label: "Mushroom",
        filename: "mushroom.stl",
        footprint_mm: 6.369,
        height_mm: 6.0,
        license: "CC0 (as recorded in opengameart-mushroom/LICENSE.txt)",
        color: "#99573f",
        bytes: include_bytes!("../../resources/scatter/mushroom.stl"),
    },
    // Organic-leaf set — decimated to ~1500 tris and normalized to a ~5mm
    // footprint with a 1.2mm thickness and a baked-in curl (the source models
    // are ~90mm print-scale; thickness is set deliberately chunky so the leaf
    // prints and sits proud of the scatter's 0.4mm stitch-sink instead of
    // vanishing into it). See tools/curate_leaves.py. CC BY-SA 4.0.
    BundledAsset {
        id: "leaf-maple",
        label: "Maple leaf",
        filename: "leaf-maple.stl",
        footprint_mm: 4.9663,
        height_mm: 1.6860,
        license: LEAF_LICENSE,
        // Fallen maple litter skews reddish-brown rather than the flatter
        // green-brown of the rest of the leaf set.
        color: "#8a5a34",
        bytes: include_bytes!("../../resources/scatter/leaf-maple.stl"),
    },
    BundledAsset {
        id: "leaf-apple",
        label: "Apple leaf",
        filename: "leaf-apple.stl",
        footprint_mm: 4.9696,
        height_mm: 1.4742,
        license: LEAF_LICENSE,
        color: "#6a7a3e",
        bytes: include_bytes!("../../resources/scatter/leaf-apple.stl"),
    },
    BundledAsset {
        id: "leaf-cherry",
        label: "Cherry leaf",
        filename: "leaf-cherry.stl",
        footprint_mm: 4.9996,
        height_mm: 1.4852,
        license: LEAF_LICENSE,
        color: "#7c6a35",
        bytes: include_bytes!("../../resources/scatter/leaf-cherry.stl"),
    },
    BundledAsset {
        id: "leaf-oak",
        label: "Oak leaf",
        filename: "leaf-oak.stl",
        footprint_mm: 4.9992,
        height_mm: 1.5903,
        license: LEAF_LICENSE,
        color: "#6e5a35",
        bytes: include_bytes!("../../resources/scatter/leaf-oak.stl"),
    },
    BundledAsset {
        id: "leaf-hazel",
        label: "Hazel leaf",
        filename: "leaf-hazel.stl",
        footprint_mm: 4.9889,
        height_mm: 1.5061,
        license: LEAF_LICENSE,
        color: "#6a7a3e",
        bytes: include_bytes!("../../resources/scatter/leaf-hazel.stl"),
    },
    BundledAsset {
        id: "forest-branch-scan",
        label: "Broken forest branch",
        filename: "forest-branch-scan.stl",
        footprint_mm: 10.920,
        height_mm: 5.483,
        license: "CC0 (Poly Haven)",
        color: "#6e553a",
        bytes: include_bytes!("../../resources/scatter/forest-branch-scan.stl"),
    },
    BundledAsset {
        id: "forest-log-scan",
        label: "Fallen forest log",
        filename: "forest-log-scan.stl",
        footprint_mm: 15.890,
        height_mm: 1.454,
        license: "CC0 (Poly Haven)",
        // A whole fallen log reads darker/more weathered than a broken
        // branch fragment.
        color: "#5c4632",
        bytes: include_bytes!("../../resources/scatter/forest-log-scan.stl"),
    },
];

/// The curated manifest, embedded verbatim for the drift test below —
/// deliberately NOT read from `resources/scatter/manifest.json` at
/// runtime by anything else. The manifest lives in the repo for
/// provenance/the drift test only; BUNDLED_ASSETS above is what the app
/// actually serves.
#[cfg(test)]
const MANIFEST_JSON: &str = include_str!("../../resources/scatter/manifest.json");

/// The curated CREDITS.md, embedded verbatim (docs/SCATTER.md "Bundled
/// assets": "listed in an in-app credits panel + CREDITS file when
/// attribution is owed"). Every bundled piece is CC0 (nothing legally
/// owed) EXCEPT the CC BY-SA 4.0 leaf set, whose attribution the file
/// carries — see the file itself. Exposed as its own command rather than folded into
/// `ScatterAsset` since it's one shared document, not a per-asset field.
#[tauri::command]
#[specta::specta]
pub fn get_scatter_credits() -> String {
    include_str!("../../resources/scatter/CREDITS.md").to_string()
}

fn bundled_asset_by_id(id: &str) -> Option<&'static BundledAsset> {
    BUNDLED_ASSETS.iter().find(|a| a.id == id)
}

/// Test-only accessor: a bundled asset's raw embedded bytes, so a test that
/// can't build a real `AppHandle` (see `resolve_asset_path`'s doc comment)
/// can still write the SAME bytes production would materialize to its own
/// scratch dir and drive the rest of the pipeline exactly as it would run
/// for real.
#[cfg(test)]
pub(crate) fn bundled_asset_bytes_for_test(id: &str) -> Option<&'static [u8]> {
    bundled_asset_by_id(id).map(|a| a.bytes)
}

/// Materialize and return every bundled asset (docs/SCATTER.md "Bundled
/// assets"). Lazily materializes each STL under `scatter/<filename>` in
/// the app cache dir on every call, same always-overwrite discipline as
/// the embedded scripts (`render::engine::materialize_embedded_asset`).
pub fn get_bundled_assets(app_handle: &AppHandle) -> Result<Vec<ScatterAsset>, AppError> {
    BUNDLED_ASSETS
        .iter()
        .map(|asset| {
            let path = crate::render::engine::materialize_embedded_asset(
                app_handle,
                &format!("scatter/{}", asset.filename),
                asset.bytes,
            )?;
            Ok(ScatterAsset {
                id: asset.id.to_string(),
                label: asset.label.to_string(),
                source: ScatterAssetSource::Bundled,
                path: path.to_string_lossy().into_owned(),
                footprint_mm: asset.footprint_mm,
                height_mm: asset.height_mm,
                color: asset.color.to_string(),
                warning: None, // curated + normalized at curation time — never warns
            })
        })
        .collect()
}

/// Neutral fallback for every user-library asset — no curation pass has
/// looked at a user's own folder, so there's no per-piece color to pick
/// the way `BUNDLED_ASSETS` does. Mirrors scatter_landscape.py's own
/// `DEFAULT_ASSET_COLOR`, kept in sync by inspection (each side owns its
/// own copy — no shared import between Rust and Python).
const DEFAULT_USER_ASSET_COLOR: &str = "#9a9a9a";

/// The color a placed instance of asset `id` should paint with — the
/// `asset_colors` counterpart to `resolve_asset_path`'s `asset_paths`
/// entry, but pure (no `AppHandle`, no materialization): a bundled id's
/// color is a compile-time constant, and a user-library id has no
/// curated color to look up at all, so both branches resolve without
/// touching disk. Never fails — an unknown id (already caught by
/// `resolve_asset_path` before this would ever run for it) falls back to
/// the same neutral grey a real user-library asset gets.
pub fn resolve_asset_color(id: &str) -> String {
    bundled_asset_by_id(id)
        .map(|a| a.color.to_string())
        .unwrap_or_else(|| DEFAULT_USER_ASSET_COLOR.to_string())
}

/// Resolve one `Asset { id }` piece to an absolute file path — bundled
/// first, then the configured user-library folder — for injection into a
/// scatter job's `asset_paths` (see docs/SCATTER.md "Asset source").
/// Returns `NotFoundError` for an unknown id or a user-library id whose
/// file no longer exists, so `start_scatter` fails clearly BEFORE
/// spawning Blender rather than forwarding a dangling id into the job
/// JSON.
pub fn resolve_asset_path(app_handle: &AppHandle, id: &str) -> Result<PathBuf, AppError> {
    if let Some(asset) = bundled_asset_by_id(id) {
        return crate::render::engine::materialize_embedded_asset(
            app_handle,
            &format!("scatter/{}", asset.filename),
            asset.bytes,
        );
    }

    let library_dir = crate::settings::SETTINGS_CACHE
        .lock()
        .ok()
        .and_then(|settings| settings.scatter_library_dir.clone());

    if let Some(dir) = library_dir {
        if let Some(path) = find_stl_by_stem(Path::new(&dir), id) {
            return Ok(path);
        }
    }

    Err(AppError::NotFoundError(format!(
        "unknown scatter asset id: {id} (not in the bundled set, and not found in the \
         configured user library folder)"
    )))
}

/// Non-recursive search of `dir` for a `*.stl` whose file stem matches `id`
/// exactly — the same identity a user-library scan handed out as `id` in
/// the first place (see `scan_scatter_library` below), so a round trip
/// (scan -> pick -> scatter) always resolves.
fn find_stl_by_stem(dir: &Path, id: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if !has_stl_extension(&path) {
            continue;
        }
        if path.file_stem().and_then(|s| s.to_str()) == Some(id) {
            return Some(path);
        }
    }
    None
}

fn has_stl_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("stl"))
}

/// A scatter piece is not a mini: a user-library STL whose measured
/// footprint clears this many millimetres is flagged, not blocked. Chosen
/// well above the bundled set's own largest legitimate piece (the
/// whale-mandible "statement piece" at 16mm) and comfortably below a
/// typical 28-32mm-heroic miniature's footprint (a round base alone
/// starts at 25mm) — headroom on both sides so neither the biggest
/// legitimate scatter piece nor a big rock/skull false-positives, while
/// an actual mini dropped in the folder by habit reliably clears it.
const MINI_FOOTPRINT_WARNING_MM: f64 = 40.0;

fn mini_footprint_warning(footprint_mm: f64) -> Option<String> {
    if footprint_mm > MINI_FOOTPRINT_WARNING_MM {
        Some(format!(
            "footprint {footprint_mm:.1}mm exceeds {MINI_FOOTPRINT_WARNING_MM:.0}mm at scale 1 — \
             this looks like a miniature, not scatter debris"
        ))
    } else {
        None
    }
}

fn unparseable_stl_warning(reason: &str) -> String {
    format!("could not read as a binary STL: {reason}")
}

/// Scan `dir` (non-recursive) for `*.stl` files and measure each one's
/// bounding box in pure Rust — no Blender (docs/SCATTER.md "User library").
/// A file that fails to parse is not dropped from the result: it's returned
/// with zeroed dims and a `warning` explaining why, so the UI can still
/// list it (and the user can see WHY it's unusable) instead of a silent
/// gap between "files in the folder" and "pieces offered".
#[tauri::command]
#[specta::specta]
pub fn scan_scatter_library(dir: String) -> Result<Vec<ScatterAsset>, AppError> {
    scan_scatter_library_dir(Path::new(&dir))
}

fn scan_scatter_library_dir(dir: &Path) -> Result<Vec<ScatterAsset>, AppError> {
    if !dir.is_dir() {
        return Err(AppError::NotFoundError(format!(
            "not a directory: {}",
            dir.display()
        )));
    }

    let entries = std::fs::read_dir(dir)
        .map_err(|e| AppError::IoError(format!("Failed to read {}: {}", dir.display(), e)))?;

    let mut assets: Vec<ScatterAsset> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_file() || !has_stl_extension(&path) {
                return None;
            }
            Some(scan_one_stl(&path))
        })
        .collect();

    assets.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(assets)
}

fn scan_one_stl(path: &Path) -> ScatterAsset {
    let id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let label = id.clone();
    let path_string = path.to_string_lossy().into_owned();

    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            return ScatterAsset {
                id,
                label,
                source: ScatterAssetSource::User,
                path: path_string,
                footprint_mm: 0.0,
                height_mm: 0.0,
                color: DEFAULT_USER_ASSET_COLOR.to_string(),
                warning: Some(unparseable_stl_warning(&format!("could not read file: {e}"))),
            };
        }
    };

    match parse_binary_stl_bbox(&bytes) {
        Ok(bbox) => {
            let footprint_mm = bbox.footprint_mm();
            ScatterAsset {
                id,
                label,
                source: ScatterAssetSource::User,
                path: path_string,
                footprint_mm,
                height_mm: bbox.height_mm(),
                color: DEFAULT_USER_ASSET_COLOR.to_string(),
                warning: mini_footprint_warning(footprint_mm),
            }
        }
        Err(reason) => ScatterAsset {
            id,
            label,
            source: ScatterAssetSource::User,
            path: path_string,
            footprint_mm: 0.0,
            height_mm: 0.0,
            color: DEFAULT_USER_ASSET_COLOR.to_string(),
            warning: Some(unparseable_stl_warning(&reason)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// Re-parse the ACTUAL manifest.json shipped in resources/scatter/
    /// (not a copy of these numbers) and assert BUNDLED_ASSETS matches it
    /// field-for-field. A future curation change that updates
    /// manifest.json without updating this table fails this test instead
    /// of silently shipping a stale const.
    #[test]
    fn bundled_assets_table_matches_the_shipped_manifest() {
        let manifest: Value = serde_json::from_str(MANIFEST_JSON).expect("manifest.json parses");
        let manifest_assets = manifest["assets"].as_array().expect("assets is an array");

        assert_eq!(
            BUNDLED_ASSETS.len(),
            manifest_assets.len(),
            "BUNDLED_ASSETS and manifest.json's assets[] must have the same piece count"
        );

        for entry in manifest_assets {
            let id = entry["id"].as_str().expect("id is a string");
            let table_entry = bundled_asset_by_id(id)
                .unwrap_or_else(|| panic!("manifest id '{id}' missing from BUNDLED_ASSETS"));

            assert_eq!(table_entry.label, entry["label"].as_str().unwrap());
            assert_eq!(table_entry.filename, entry["source_file"].as_str().unwrap());
            assert_eq!(table_entry.license, entry["license"].as_str().unwrap());

            let x = entry["canonical"]["footprint_mm"]["x"].as_f64().unwrap();
            let y = entry["canonical"]["footprint_mm"]["y"].as_f64().unwrap();
            assert_eq!(table_entry.footprint_mm, x.max(y), "footprint_mm for {id}");

            let height = entry["canonical"]["height_mm"].as_f64().unwrap();
            assert_eq!(table_entry.height_mm, height, "height_mm for {id}");
        }
    }

    /// Every embedded STL's OWN measured bbox (via the same pure parser
    /// scan_scatter_library uses) must match the table's transcribed
    /// footprint/height — catches a copy-paste id/file mismatch or a
    /// manifest number that was never actually re-measured against the
    /// file that shipped.
    #[test]
    fn bundled_assets_measured_bbox_matches_the_table() {
        for asset in BUNDLED_ASSETS {
            let bbox = parse_binary_stl_bbox(asset.bytes)
                .unwrap_or_else(|e| panic!("{} failed to parse: {e}", asset.id));
            assert!(
                (bbox.footprint_mm() - asset.footprint_mm).abs() < 0.01,
                "{}: measured footprint {} vs table {}",
                asset.id,
                bbox.footprint_mm(),
                asset.footprint_mm
            );
            assert!(
                (bbox.height_mm() - asset.height_mm).abs() < 0.01,
                "{}: measured height {} vs table {}",
                asset.id,
                bbox.height_mm(),
                asset.height_mm
            );
        }
    }

    #[test]
    fn bundled_assets_manifold_and_license_allowed_per_manifest() {
        let manifest: Value = serde_json::from_str(MANIFEST_JSON).unwrap();
        for entry in manifest["assets"].as_array().unwrap() {
            // CC0 is the default admission bar (docs/SCATTER-ASSETS.md); CC
            // BY-SA 4.0 is the one deliberate exception (the Printables leaf
            // set — see LEAF_LICENSE). Anything else is not cleared to ship.
            let license = entry["license"].as_str().unwrap();
            assert!(
                license.contains("CC0") || license.contains("CC BY-SA 4.0"),
                "bundled piece '{}' has un-cleared license '{}' — only CC0 or CC BY-SA 4.0 ship",
                entry["id"].as_str().unwrap(),
                license
            );
            assert!(entry["manifold"].as_bool().unwrap());
            assert!(entry["tris"].as_u64().unwrap() <= 15_000);
        }
    }

    /// Every bundled asset must carry a real `#rrggbb` color — a
    /// missing/malformed entry would silently paint a piece black or
    /// crash scatter_landscape.py's `_hex_to_rgb01` on a bad hex string.
    #[test]
    fn every_bundled_asset_has_a_well_formed_hex_color() {
        for asset in BUNDLED_ASSETS {
            assert_eq!(
                asset.color.len(),
                7,
                "{}: color {:?} must be '#rrggbb' (7 chars)",
                asset.id,
                asset.color
            );
            assert!(
                asset.color.starts_with('#'),
                "{}: color {:?} must start with '#'",
                asset.id,
                asset.color
            );
            assert!(
                asset.color[1..].chars().all(|c| c.is_ascii_hexdigit()),
                "{}: color {:?} must be hex digits after '#'",
                asset.id,
                asset.color
            );
        }
    }

    #[test]
    fn resolve_asset_color_returns_the_bundled_color_and_defaults_for_unknown_ids() {
        assert_eq!(resolve_asset_color("skull-hesperocyon"), "#cfc6b0");
        assert_eq!(resolve_asset_color("mushroom"), "#99573f");
        assert_eq!(resolve_asset_color("not-a-bundled-id"), DEFAULT_USER_ASSET_COLOR);
    }

    #[test]
    fn scan_scatter_library_dir_defaults_user_assets_to_the_neutral_color() {
        let dir = std::env::temp_dir().join(format!("stlpack_scatter_scan_color_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = build_binary_stl(&[[(0.0, 0.0, 0.0), (5.0, 0.0, 0.0), (0.0, 5.0, 3.0)]]);
        std::fs::write(dir.join("piece.stl"), &good).unwrap();

        let result = scan_scatter_library_dir(&dir).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].color, DEFAULT_USER_ASSET_COLOR);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `resolve_asset_path`'s bundled branch needs a real `AppHandle`, so
    /// it isn't unit-tested directly; `find_stl_by_stem` is the pure logic
    /// its user-library fallback delegates to, and is covered here.
    #[test]
    fn find_stl_by_stem_is_non_recursive_and_extension_gated() {
        let dir = std::env::temp_dir().join(format!("stlpack_scatter_findstem_{}", std::process::id()));
        let sub = dir.join("subdir");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(dir.join("piece.stl"), b"x").unwrap();
        std::fs::write(dir.join("piece.txt"), b"x").unwrap();
        std::fs::write(sub.join("nested.stl"), b"x").unwrap();

        assert!(find_stl_by_stem(&dir, "piece").is_some());
        assert!(find_stl_by_stem(&dir, "nested").is_none()); // not recursive
        assert!(find_stl_by_stem(&dir, "piece_txt").is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Hand-builds a tiny well-formed binary STL, same recipe as
    /// stl_bbox's own test helper (kept local rather than shared).
    fn build_binary_stl(triangles: &[[(f32, f32, f32); 3]]) -> Vec<u8> {
        let mut bytes = vec![0u8; 80];
        bytes.extend_from_slice(&(triangles.len() as u32).to_le_bytes());
        for tri in triangles {
            bytes.extend_from_slice(&[0u8; 12]);
            for &(x, y, z) in tri {
                bytes.extend_from_slice(&x.to_le_bytes());
                bytes.extend_from_slice(&y.to_le_bytes());
                bytes.extend_from_slice(&z.to_le_bytes());
            }
            bytes.extend_from_slice(&[0u8; 2]);
        }
        bytes
    }

    #[test]
    fn scan_scatter_library_dir_reports_valid_pieces_without_warning() {
        let dir = std::env::temp_dir().join(format!("stlpack_scatter_scan_ok_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // A small pebble-scale piece: 6mm footprint, well under the warning gate.
        let small = build_binary_stl(&[[(0.0, 0.0, 0.0), (6.0, 0.0, 0.0), (0.0, 4.0, 2.0)]]);
        std::fs::write(dir.join("small-rock.stl"), &small).unwrap();

        let result = scan_scatter_library_dir(&dir).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "small-rock");
        assert_eq!(result[0].source, ScatterAssetSource::User);
        assert_eq!(result[0].footprint_mm, 6.0);
        assert_eq!(result[0].height_mm, 2.0);
        assert!(result[0].warning.is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scan_scatter_library_dir_warns_on_a_mini_scale_footprint() {
        let dir = std::env::temp_dir().join(format!("stlpack_scatter_scan_warn_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // 50mm footprint — clears MINI_FOOTPRINT_WARNING_MM (40mm).
        let big = build_binary_stl(&[[(0.0, 0.0, 0.0), (50.0, 0.0, 0.0), (0.0, 20.0, 30.0)]]);
        std::fs::write(dir.join("suspiciously-large.stl"), &big).unwrap();

        let result = scan_scatter_library_dir(&dir).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].footprint_mm, 50.0);
        let warning = result[0].warning.as_ref().expect("expected a mini-scale warning");
        assert!(warning.contains("40"));
        assert!(warning.contains("miniature"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scan_scatter_library_dir_flags_unparseable_files_instead_of_failing() {
        let dir = std::env::temp_dir().join(format!("stlpack_scatter_scan_bad_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(dir.join("not-really-stl.stl"), b"this is not binary STL data at all").unwrap();
        let good = build_binary_stl(&[[(0.0, 0.0, 0.0), (5.0, 0.0, 0.0), (0.0, 5.0, 3.0)]]);
        std::fs::write(dir.join("good-piece.stl"), &good).unwrap();

        let result = scan_scatter_library_dir(&dir).unwrap();
        assert_eq!(result.len(), 2, "the scan must not fail or drop the bad file");

        let bad = result.iter().find(|a| a.id == "not-really-stl").unwrap();
        assert!(bad.warning.is_some());
        assert_eq!(bad.footprint_mm, 0.0);

        let good = result.iter().find(|a| a.id == "good-piece").unwrap();
        assert!(good.warning.is_none());
        assert_eq!(good.footprint_mm, 5.0);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scan_scatter_library_dir_is_non_recursive_and_ignores_non_stl_files() {
        let dir = std::env::temp_dir().join(format!("stlpack_scatter_scan_nonrec_{}", std::process::id()));
        let sub = dir.join("nested");
        std::fs::create_dir_all(&sub).unwrap();

        let good = build_binary_stl(&[[(0.0, 0.0, 0.0), (3.0, 0.0, 0.0), (0.0, 3.0, 1.0)]]);
        std::fs::write(dir.join("top-level.stl"), &good).unwrap();
        std::fs::write(dir.join("readme.txt"), b"not an stl").unwrap();
        std::fs::write(sub.join("nested.stl"), &good).unwrap();

        let result = scan_scatter_library_dir(&dir).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "top-level");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scan_scatter_library_dir_rejects_a_missing_directory() {
        let dir = std::env::temp_dir().join("stlpack_scatter_scan_definitely_missing");
        std::fs::remove_dir_all(&dir).ok();
        assert!(matches!(
            scan_scatter_library_dir(&dir),
            Err(AppError::NotFoundError(_))
        ));
    }
}
