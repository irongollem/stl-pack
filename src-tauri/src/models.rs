use serde::{Deserialize, Serialize};
use specta::Type;
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct Release {
    pub name: String,
    pub date: String,
    pub version: String,
    pub designer: String,
    pub models: Vec<StlModel>,
    pub description: Option<String>,
    pub groups: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct StlModel {
    pub model_name: String,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub images: Vec<String>, // the path of the temporary location of the image during archive creation
    pub model_files: Vec<String>, // the path of the temporary location of the model file during archive creation
    pub group: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct Settings {
    pub scratch_dir: Option<String>,
    pub target_dir: Option<String>,
    pub compression_type: Option<CompressionType>,
    pub chunk_size: Option<u32>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub enum CompressionType {
    SevenZip,
    Zip,
    TarGz,
    TarXz,
}
