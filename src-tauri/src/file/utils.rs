use std::path::{Path, PathBuf};

use crate::error::AppError;

use super::compressors;

pub fn clean_name(name: &str) -> String {
    name.trim().to_lowercase().replace(" ", "_")
}

/// First non-existing sibling of `path`: "cut.stl" -> "cut-1.stl" ->
/// "cut-2.stl" ... (or "cut" -> "cut-1" with no extension at all). Shared by
/// render/commands.rs (render outputs, always .png) and
/// basecutter/commands.rs::export_cuts (catalog exports, always .stl) so a
/// re-run into an already-populated destination never clobbers an earlier
/// file — it gets a -N suffix instead.
pub(crate) fn unique_path(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let extension = path.extension().map(|e| e.to_string_lossy().into_owned());
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    for n in 1.. {
        let candidate = match &extension {
            Some(ext) => parent.join(format!("{stem}-{n}.{ext}")),
            None => parent.join(format!("{stem}-{n}")),
        };
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("ran out of integers before file names")
}

pub fn calculate_total_size(
    group_and_model_dirs: &[PathBuf],
    files_for_3pk: &[PathBuf],
    files_for_zip: &[PathBuf],
) -> Result<(u32, u32), AppError> {
    let (group_and_model_size, group_and_model_files) =
        compressors::determine_dir_size_kb(group_and_model_dirs)?;
    let (files_for_3pk_size, files_for_3pk_count) =
        compressors::determine_dir_size_kb(files_for_3pk)?;
    let (files_for_zip_size, files_for_zip_count) =
        compressors::determine_dir_size_kb(files_for_zip)?;

    let total_size = group_and_model_size + files_for_3pk_size + files_for_zip_size;
    let total_files = group_and_model_files + files_for_3pk_count + files_for_zip_count;

    Ok((total_size, total_files))
}

#[cfg(test)]
mod unique_path_tests {
    use super::*;

    struct TempRoot(PathBuf);
    impl TempRoot {
        fn new(label: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "stlpack_file_utils_unique_path_{}_{}",
                label,
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempRoot {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn returns_the_path_unchanged_when_nothing_is_there() {
        let dir = TempRoot::new("free");
        let candidate = dir.path().join("render.png");
        assert_eq!(unique_path(candidate.clone()), candidate);
    }

    #[test]
    fn suffixes_before_the_extension_on_collision() {
        let dir = TempRoot::new("collide");
        let taken = dir.path().join("render.png");
        std::fs::write(&taken, b"x").unwrap();
        assert_eq!(unique_path(taken), dir.path().join("render-1.png"));
    }

    #[test]
    fn keeps_suffixing_past_multiple_collisions() {
        let dir = TempRoot::new("multi");
        std::fs::write(dir.path().join("cut.stl"), b"x").unwrap();
        std::fs::write(dir.path().join("cut-1.stl"), b"x").unwrap();
        assert_eq!(
            unique_path(dir.path().join("cut.stl")),
            dir.path().join("cut-2.stl")
        );
    }

    #[test]
    fn handles_a_path_with_no_extension() {
        let dir = TempRoot::new("noext");
        std::fs::write(dir.path().join("cut"), b"x").unwrap();
        assert_eq!(unique_path(dir.path().join("cut")), dir.path().join("cut-1"));
    }
}
