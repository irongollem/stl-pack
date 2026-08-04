#![cfg(test)]

use rusqlite::Connection;

use crate::catalog::{FileRow, ModelRow};

use super::schema::init_schema;

pub(super) fn test_conn() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();
    conn
}

pub(super) fn file_row(path: &str, dir_path: &str, size_bytes: i64) -> FileRow {
    FileRow {
        path: path.into(),
        dir_path: dir_path.into(),
        file_name: path.rsplit('/').next().unwrap().into(),
        extension: path.rsplit('.').next().unwrap().into(),
        size_bytes,
        modified_at: 100,
        ..Default::default()
    }
}

pub(super) fn sample_rows() -> (Vec<FileRow>, Vec<ModelRow>, Vec<(String, String)>) {
    let files = vec![
        FileRow {
            path: "/lib/newt/GiantNewt_v02.stl".into(),
            dir_path: "/lib/newt".into(),
            file_name: "GiantNewt_v02.stl".into(),
            extension: "stl".into(),
            size_bytes: 2048,
            modified_at: 100,
            ..Default::default()
        },
        FileRow {
            path: "/lib/bugbear/Bugbear.stl".into(),
            dir_path: "/lib/bugbear".into(),
            file_name: "Bugbear.stl".into(),
            extension: "stl".into(),
            size_bytes: 4096,
            modified_at: 100,
            ..Default::default()
        },
    ];
    let models = vec![
        ModelRow {
            dir_path: "/lib/newt".into(),
            name: "Giant Newt".into(),
            description: Some("A very large newt".into()),
            designer: Some("DTL".into()),
            release_name: Some("Critterfolk".into()),
            preview_path: None,
            source: "metadata".into(),
            uuid: None,
            file_count: 1,
            total_size_bytes: 2048,
            pose: None,
            scale: None,
            support_status: None,
            release_date: None,
            variant: None,
            sculptor: None,
            base_round_mm: None,
            base_square_mm: None,
            group_name: Some("Giant Newt".into()),
            ..Default::default()
        },
        ModelRow {
            dir_path: "/lib/bugbear".into(),
            name: "Bugbear".into(),
            description: None,
            designer: None,
            release_name: None,
            preview_path: None,
            source: "heuristic".into(),
            uuid: None,
            file_count: 1,
            total_size_bytes: 4096,
            pose: None,
            scale: None,
            support_status: None,
            release_date: None,
            variant: None,
            sculptor: None,
            base_round_mm: None,
            base_square_mm: None,
            group_name: Some("Bugbear".into()),
            ..Default::default()
        },
    ];
    let tags = vec![("/lib/newt".to_string(), "amphibian".to_string())];
    (files, models, tags)
}

pub(super) fn model_row(dir_path: &str, name: &str) -> ModelRow {
    ModelRow {
        dir_path: dir_path.into(),
        name: name.into(),
        description: None,
        designer: None,
        release_name: None,
        preview_path: None,
        source: "heuristic".into(),
        uuid: None,
        file_count: 1,
        total_size_bytes: 10,
        pose: None,
        scale: None,
        support_status: None,
        release_date: None,
        variant: None,
        sculptor: None,
        base_round_mm: None,
        base_square_mm: None,
        group_name: Some(name.into()),
        ..Default::default()
    }
}
