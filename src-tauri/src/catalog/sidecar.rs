//! Targeted model.json enrichment for machine-derived render metadata:
//! merges measured geometry and the rendered rotation into a sidecar that
//! ALREADY exists, and refuses to create one — a synthetic model.json
//! would flip the folder from heuristic to sidecar authority, which isn't
//! this module's call to make.

use crate::error::AppError;
use std::fs;
use std::path::Path;

/// Merge dims/part_count (+ the rotation the render used) into
/// `<dir>/model.json`, preserving every other key. No-op when the sidecar
/// doesn't exist. Atomic tmp+rename write.
pub fn merge_measured_into_sidecar(
    dir: &Path,
    dims_mm: &str,
    part_count: u32,
    rotation: Option<&str>,
) -> Result<(), AppError> {
    let sidecar = dir.join("model.json");
    if !sidecar.is_file() {
        return Ok(());
    }
    let text = fs::read_to_string(&sidecar)
        .map_err(|e| AppError::IoError(format!("Failed to read {}: {}", sidecar.display(), e)))?;
    let mut value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| AppError::JsonError(format!("Unparseable model.json: {}", e)))?;
    let Some(object) = value.as_object_mut() else {
        return Err(AppError::JsonError(
            "model.json top level is not an object".to_string(),
        ));
    };
    object.insert("dims_mm".to_string(), serde_json::json!(dims_mm));
    object.insert("part_count".to_string(), serde_json::json!(part_count));
    if let Some(rotation) = rotation {
        object.insert("rotation".to_string(), serde_json::json!(rotation));
    }

    let json = serde_json::to_string_pretty(&value)
        .map_err(|e| AppError::JsonError(format!("Failed to encode model.json: {}", e)))?;
    let temp = dir.join(format!(".plinth-sidecar-{}.tmp", std::process::id()));
    fs::write(&temp, json)?;
    fs::remove_file(&sidecar).ok(); // rename-over-existing fails on Windows
    fs::rename(&temp, &sidecar)
        .map_err(|e| AppError::IoError(format!("Failed to update model.json: {}", e)))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_into_existing_sidecar_preserving_other_keys() {
        let dir = std::env::temp_dir().join(format!("stlpack_sidecar_{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("model.json"),
            r#"{"name":"Knight","tags":["hero"],"base_round_mm":"32"}"#,
        )
        .unwrap();

        merge_measured_into_sidecar(&dir, "60.2x35.1x88.7", 3, Some("90,0,0")).unwrap();

        let value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.join("model.json")).unwrap()).unwrap();
        assert_eq!(value["name"], "Knight", "unrelated keys preserved");
        assert_eq!(value["base_round_mm"], "32");
        assert_eq!(value["dims_mm"], "60.2x35.1x88.7");
        assert_eq!(value["part_count"], 3);
        assert_eq!(value["rotation"], "90,0,0");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn never_creates_a_sidecar() {
        let dir = std::env::temp_dir().join(format!("stlpack_sidecar_no_{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();

        merge_measured_into_sidecar(&dir, "10x10x10", 1, None).unwrap();
        assert!(
            !dir.join("model.json").exists(),
            "a folder without a sidecar stays heuristic-scanned"
        );

        fs::remove_dir_all(&dir).ok();
    }
}
