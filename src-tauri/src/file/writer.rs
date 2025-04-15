use std::{fs, path::PathBuf};

use crate::{
    error::AppError,
    models::{ModelLocation, ModelReference, Release, StlModel},
};

pub fn write_json(json_string: String, path: PathBuf) -> Result<(), AppError> {
    fs::write(path, json_string).map_err(|e| AppError::IoError(e.to_string()))
}

pub fn add_model_to_release_json(release_path: PathBuf, model: &StlModel) -> Result<(), AppError> {
    let release_json_path = release_path.join("release.json");
    let release_json =
        fs::read_to_string(&release_json_path).map_err(|e| AppError::IoError(e.to_string()))?;
    let mut release: Release = serde_json::from_str(&release_json)?;

    let path = if let Some(ref group_name) = model.group {
        if !release.groups.contains(group_name) {
            release.groups.push(group_name.clone());
        }

        format!("{}/{}/model.json", group_name, model.name)
    } else {
        format!("{}/model.json", model.name)
    };
    let model_id = model.id.ok_or_else(|| {
        AppError::InvalidInput("Model ID is missing when adding to release JSON".to_string())
    })?;

    let model_reference = ModelReference {
        id: model_id,
        location: ModelLocation::Local(path),
    };

    release.model_references.push(model_reference);

    write_json(serde_json::to_string_pretty(&release)?, release_json_path)?;
    Ok(())
}
