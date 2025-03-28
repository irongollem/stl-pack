use serde::{Deserialize, Serialize};
use specta::Type;
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct ReleaseMetaData {
    pub release_name: String,
    pub release_date: String,
    pub version: String,
    pub designer: String,
    pub models: Vec<StlModel>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct StlModel {
    pub model_name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub images: Vec<String>, // the path of the temporary location of the image during archive creation
    pub model_files: Vec<String>, // the path of the temporary location of the model file during archive creation
}

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct Settings {
    pub scratch_dir: Option<String>,
    pub target_dir: Option<String>,
    pub compression_type: Option<CompressionType>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub enum CompressionType {
    // SevenZip,
    Zip,
    Tar,
    TarGz,
    TarBz2,
}
