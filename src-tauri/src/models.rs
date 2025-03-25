use serde::{ Serialize, Deserialize };

// #[derive(Serialize, Deserialize, Debug)]
// pub struct FileMeta {
//     pub name: String,
//     pub path: String,
// }

#[derive(Debug, Serialize, Deserialize)]
pub struct StlModel {
    pub model_name: String,
    pub release_date: String,
    pub designer: String,
    pub release: String,
    pub description: String,
    pub tags: Vec<String>,
    pub pictures: Vec<String>,
    pub model_files: Vec<String>,
}