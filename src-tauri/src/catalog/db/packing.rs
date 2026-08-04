use crate::error::AppError;
use rusqlite::{params, Connection};
use std::path::Path;

/// Flip a model dir's index rows to packed, in place — the pack job calls
/// this per model so no rescan is needed. Only the sidecar's entries are
/// touched, MINUS the paths the pack kept loose because they changed since
/// compression: their rows must keep describing the loose file, or the
/// catalog hides user data behind a "packed" flag until the next rescan.
/// file_identity is cleared: the loose inode it named no longer exists.
/// content_hash is stored BARE (pack::bare_hash) — the dup scanner's format.
pub fn mark_packed(
    conn: &mut Connection,
    model_dir: &str,
    sidecar: &crate::catalog::pack::PackSidecar,
    kept: &[String],
) -> Result<(), AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Catalog pack update failed: {}", e));
    let archive_path = Path::new(model_dir)
        .join(crate::catalog::pack::PACK_ARCHIVE_NAME)
        .to_string_lossy()
        .into_owned();
    let tx = conn.transaction().map_err(map_err)?;
    {
        let mut update = tx
            .prepare(
                "UPDATE files SET archive_path = ?1, content_hash = ?2,
                     file_identity = NULL, size_bytes = ?3, modified_at = ?4
                 WHERE path = ?5",
            )
            .map_err(map_err)?;
        for entry in &sidecar.files {
            let path = crate::catalog::pack::entry_disk_path(Path::new(model_dir), &entry.name)
                .to_string_lossy()
                .into_owned();
            if kept.contains(&path) {
                continue;
            }
            update
                .execute(params![
                    archive_path,
                    crate::catalog::pack::bare_hash(&entry.checksum),
                    entry.size_bytes as i64,
                    entry.modified_at,
                    path
                ])
                .map_err(map_err)?;
        }
    }
    tx.execute(
        "INSERT OR REPLACE INTO packs
             (model_dir, archive_path, archive_size_bytes, archive_checksum, packed_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            model_dir,
            archive_path,
            sidecar.archive_size_bytes as i64,
            sidecar.archive_checksum,
            sidecar.packed_at
        ],
    )
    .map_err(map_err)?;
    tx.commit().map_err(map_err)?;
    Ok(())
}

/// Flip a model dir's index rows back to loose after an unpack. The caller
/// passes fresh (path, size, mtime) stats from the extracted files —
/// content_hash is kept (the bytes are checksum-verified unchanged), and
/// writing the fresh mtime in the same transaction is what stops the next
/// rescan's changed-file check from dropping that hash.
pub fn mark_unpacked(
    conn: &mut Connection,
    model_dir: &str,
    fresh_stats: &[(String, i64, i64)],
) -> Result<(), AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Catalog unpack update failed: {}", e));
    let tx = conn.transaction().map_err(map_err)?;
    {
        let mut update = tx
            .prepare(
                "UPDATE files SET archive_path = NULL, size_bytes = ?1, modified_at = ?2
                 WHERE path = ?3",
            )
            .map_err(map_err)?;
        for (path, size_bytes, modified_at) in fresh_stats {
            update
                .execute(params![size_bytes, modified_at, path])
                .map_err(map_err)?;
        }
    }
    tx.execute("DELETE FROM packs WHERE model_dir = ?1", [model_dir])
        .map_err(map_err)?;
    tx.commit().map_err(map_err)?;
    Ok(())
}

/// Sum of indexed file sizes directly in `dir` — the pack job's progress
/// denominator (packing is per-dir, non-recursive), from the index so no
/// disk walk is needed up front.
pub fn dir_size_bytes(conn: &Connection, dir: &str) -> Result<i64, AppError> {
    let map_err = |e: rusqlite::Error| AppError::ConfigError(format!("Size query failed: {}", e));
    conn.query_row(
        "SELECT COALESCE(SUM(size_bytes), 0) FROM files WHERE dir_path = ?1",
        [dir],
        |row| row.get(0),
    )
    .map_err(map_err)
}

/// Model folders eligible for packing: every model dir that still has at
/// least one loose model file, optionally narrowed to one designer and/or
/// an explicit set of displayed group names (the card checkboxes). This is
/// what lets "pack this whole designer" be one resumable job instead of a
/// drawer visit per model.
pub fn pack_candidate_dirs(
    conn: &Connection,
    designer: Option<&str>,
    groups: &[String],
) -> Result<Vec<String>, AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Pack candidate query failed: {}", e));
    let mut sql = String::from(
        "SELECT DISTINCT m.dir_path FROM models m
         LEFT JOIN model_user_meta u ON u.dir_path = m.dir_path
         LEFT JOIN group_renames r ON r.source_group = COALESCE(m.group_name, m.name)
         WHERE EXISTS (SELECT 1 FROM files f
                       WHERE f.dir_path = m.dir_path AND f.archive_path IS NULL)",
    );
    let mut bound: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    if let Some(name) = designer.map(str::trim).filter(|d| !d.is_empty()) {
        sql.push_str(" AND lower(COALESCE(u.designer, m.designer, '')) = lower(?)");
        bound.push(Box::new(name.to_string()));
    }
    if !groups.is_empty() {
        let placeholders = vec!["lower(?)"; groups.len()].join(", ");
        sql.push_str(&format!(
            " AND lower(COALESCE(r.display_name, m.group_name, m.name)) IN ({})",
            placeholders
        ));
        for group in groups {
            bound.push(Box::new(group.clone()));
        }
    }
    sql.push_str(" ORDER BY m.dir_path");
    let params_ref: Vec<&dyn rusqlite::types::ToSql> = bound.iter().map(|b| b.as_ref()).collect();
    let mut stmt = conn.prepare(&sql).map_err(map_err)?;
    let rows = stmt
        .query_map(params_ref.as_slice(), |row| row.get(0))
        .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        .map_err(map_err)?;
    Ok(rows)
}

/// Every packed model dir. The normalizer and movers consult this to skip
/// what they can't safely reorganize (their index re-keying doesn't rewrite
/// archive_path/packs yet — unpack first).
pub fn packed_model_dirs(conn: &Connection) -> Result<Vec<String>, AppError> {
    let map_err = |e: rusqlite::Error| AppError::ConfigError(format!("Pack lookup failed: {}", e));
    let mut stmt = conn
        .prepare("SELECT model_dir FROM packs")
        .map_err(map_err)?;
    let rows = stmt
        .query_map([], |row| row.get(0))
        .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        .map_err(map_err)?;
    Ok(rows)
}

/// Whether `dir` is, or contains, a packed model dir. substr comparison
/// instead of LIKE so path characters never act as wildcards; both
/// separators checked because the db stores native paths.
pub fn dir_contains_pack(conn: &Connection, dir: &str) -> Result<bool, AppError> {
    let map_err = |e: rusqlite::Error| AppError::ConfigError(format!("Pack lookup failed: {}", e));
    conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM packs
             WHERE model_dir = ?1
                OR substr(model_dir, 1, length(?1) + 1) = ?1 || '/'
                OR substr(model_dir, 1, length(?1) + 1) = ?1 || char(92)
                OR substr(?1, 1, length(model_dir) + 1) = model_dir || '/'
                OR substr(?1, 1, length(model_dir) + 1) = model_dir || char(92)
         )",
        [dir],
        |row| row.get(0),
    )
    .map_err(map_err)
}

/// archive_path per file path, for routing byte-needing actions: a NULL/
/// missing entry means the path is loose on disk (or unknown to the index).
pub fn archive_paths_for(
    conn: &Connection,
    paths: &[String],
) -> Result<std::collections::HashMap<String, String>, AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Archive lookup failed: {}", e));
    let mut stmt = conn
        .prepare("SELECT archive_path FROM files WHERE path = ?1")
        .map_err(map_err)?;
    let mut out = std::collections::HashMap::new();
    for path in paths {
        let archive: Option<Option<String>> =
            stmt.query_row([path], |row| row.get(0)).map(Some).or_else(|e| {
                if e == rusqlite::Error::QueryReturnedNoRows {
                    Ok(None)
                } else {
                    Err(e)
                }
            })
            .map_err(map_err)?;
        if let Some(Some(archive)) = archive {
            out.insert(path.clone(), archive);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::db::*;
    use crate::catalog::db::test_util::*;
    use crate::catalog::PackRow;

    #[test]
    fn pack_marking_flips_flags_stats_and_back() {
        let mut conn = test_conn();
        let (files, models, tags) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();

        let sidecar = crate::catalog::pack::PackSidecar {
            format: crate::catalog::pack::PACK_FORMAT.into(),
            version: crate::catalog::pack::PACK_VERSION,
            generator: "plinth/test".into(),
            archive: crate::catalog::pack::PACK_ARCHIVE_NAME.into(),
            archive_checksum: "blake3:abc".into(),
            archive_size_bytes: 512,
            packed_at: 42,
            files: vec![crate::catalog::pack::PackFileEntry {
                name: "GiantNewt_v02.stl".into(),
                checksum: "blake3:def".into(),
                size_bytes: 2048,
                modified_at: 100,
                stored: true,
            }],
        };
        mark_packed(&mut conn, "/lib/newt", &sidecar, &[]).unwrap();

        // file + member + group all report packed; the other model doesn't
        let listed = model_files(&conn, "/lib/newt", None).unwrap();
        assert!(listed[0].packed);
        let newt = group_members(&conn, "Giant Newt", true).unwrap();
        assert!(newt[0].packed);
        let bugbear = group_members(&conn, "Bugbear", true).unwrap();
        assert!(!bugbear[0].packed);
        // the pack checksum joins duplicate detection without a disk read —
        // stored BARE (the dup scanner's format), not "blake3:"-prefixed
        assert_eq!(
            known_hash(&conn, "/lib/newt/GiantNewt_v02.stl").as_deref(),
            Some("def")
        );

        let s = stats(&conn).unwrap();
        assert_eq!(s.packed_models, 1);
        assert_eq!(s.packed_logical_bytes, 2048.0);
        assert_eq!(s.packed_archive_bytes, 512.0);

        // unpack: flags clear, hash survives (bytes verified unchanged)
        mark_unpacked(
            &mut conn,
            "/lib/newt",
            &[("/lib/newt/GiantNewt_v02.stl".into(), 2048, 200)],
        )
        .unwrap();
        let listed = model_files(&conn, "/lib/newt", None).unwrap();
        assert!(!listed[0].packed);
        assert_eq!(
            known_hash(&conn, "/lib/newt/GiantNewt_v02.stl").as_deref(),
            Some("def")
        );
        assert_eq!(stats(&conn).unwrap().packed_models, 0);

        // a rescan carrying the pack row keeps the seeded hash and does NOT
        // resurrect a stale identity for packed rows (the scanner seeds
        // bare hashes — pack::bare_hash strips the sidecar prefix)
        let mut packed_file = file_row("/lib/newt/GiantNewt_v02.stl", "/lib/newt", 2048);
        packed_file.archive_path = Some("/lib/newt/model.plinthpack".into());
        packed_file.content_hash = Some("def".into());
        // a loose twin the dup scanner hashed (bare hex), same size + bytes
        let mut loose_twin = file_row("/lib/bugbear/Bugbear.stl", "/lib/bugbear", 2048);
        loose_twin.content_hash = Some("def".into());
        let pack_row = PackRow {
            model_dir: "/lib/newt".into(),
            archive_path: "/lib/newt/model.plinthpack".into(),
            archive_size_bytes: 512,
            archive_checksum: Some("blake3:abc".into()),
            packed_at: Some(42),
        };
        replace_catalog(
            &mut conn,
            "/lib",
            &[packed_file, loose_twin],
            &models,
            &[],
            &[],
            &[pack_row],
        )
        .unwrap();
        assert_eq!(
            known_hash(&conn, "/lib/newt/GiantNewt_v02.stl").as_deref(),
            Some("def"),
            "scan-seeded hash survives the old_hashes restore"
        );
        assert_eq!(stats(&conn).unwrap().packed_models, 1);

        // the whole point of bare seeding: a packed copy and a loose twin
        // hashed by the dup scanner (bare hex) land in ONE duplicate group,
        // with the packed path flagged for the UI
        let groups = duplicate_groups(&conn).unwrap();
        assert_eq!(groups.len(), 1, "packed + loose twins group together");
        assert_eq!(groups[0].paths.len(), 2);
        assert_eq!(
            groups[0].packed_paths,
            vec!["/lib/newt/GiantNewt_v02.stl".to_string()]
        );
    }
}
