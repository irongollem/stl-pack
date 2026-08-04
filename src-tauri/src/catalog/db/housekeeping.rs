use crate::error::AppError;
use rusqlite::{params, Connection};

use super::ingest::{prune_orphans, rebuild_fts, refresh_fts_row};

/// Prune file rows after an on-disk delete. Duplicate groups and stats
/// both derive from `files`, so this is what makes a dedup delete visible
/// immediately instead of only after the next full rescan. Per-model
/// counters are recomputed for the affected dirs so the UI stays honest.
pub fn remove_files(conn: &mut Connection, paths: &[String]) -> Result<(), AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Catalog prune failed: {}", e));
    let tx = conn.transaction().map_err(map_err)?;
    {
        let mut affected_dirs: Vec<String> = Vec::new();
        let mut dir_stmt = tx
            .prepare("SELECT dir_path FROM files WHERE path = ?1")
            .map_err(map_err)?;
        let mut delete_stmt = tx
            .prepare("DELETE FROM files WHERE path = ?1")
            .map_err(map_err)?;
        for path in paths {
            if let Ok(dir) = dir_stmt.query_row([path], |row| row.get::<_, String>(0)) {
                if !affected_dirs.contains(&dir) {
                    affected_dirs.push(dir);
                }
            }
            delete_stmt.execute([path]).map_err(map_err)?;
        }
        drop(dir_stmt);
        drop(delete_stmt);

        let mut recount_stmt = tx
            .prepare(
                "UPDATE models SET
                     file_count = (SELECT COUNT(*) FROM files WHERE dir_path = ?1),
                     total_size_bytes =
                         (SELECT COALESCE(SUM(size_bytes), 0) FROM files WHERE dir_path = ?1)
                 WHERE dir_path = ?1",
            )
            .map_err(map_err)?;
        for dir in &affected_dirs {
            recount_stmt.execute([dir]).map_err(map_err)?;
        }
    }
    tx.commit().map_err(map_err)?;
    Ok(())
}

/// Remove whole models from the index — the user-facing "delete model"
/// path. Scoped by dir_path prefix (like purge_root, not like remove_files):
/// the caller may have trashed a folder recursively, so any row filed under
/// a subdirectory of a doomed dir must go with it or it lingers as a ghost
/// pointing at trashed bytes. Sweeps the tables prune_orphans skips
/// (variant_previews, group_covers, packs) explicitly — they're keyed by
/// dir_path/model_dir, not existence-joined. Returns how many model rows
/// actually left the index.
pub fn remove_models(conn: &mut Connection, dirs: &[String]) -> Result<u32, AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Catalog model removal failed: {}", e));
    let sep = std::path::MAIN_SEPARATOR.to_string();
    let tx = conn.transaction().map_err(map_err)?;
    let mut removed: u32 = 0;
    {
        for dir in dirs {
            removed += tx
                .execute(
                    "DELETE FROM models WHERE dir_path = ?1
                       OR substr(dir_path, 1, length(?1) + length(?2)) = ?1 || ?2",
                    params![dir, sep],
                )
                .map_err(map_err)? as u32;
            tx.execute(
                "DELETE FROM files WHERE dir_path = ?1
                   OR substr(dir_path, 1, length(?1) + length(?2)) = ?1 || ?2",
                params![dir, sep],
            )
            .map_err(map_err)?;
            tx.execute(
                "DELETE FROM packs WHERE model_dir = ?1
                   OR substr(model_dir, 1, length(?1) + length(?2)) = ?1 || ?2",
                params![dir, sep],
            )
            .map_err(map_err)?;
            tx.execute(
                "DELETE FROM variant_previews WHERE dir_path = ?1
                   OR substr(dir_path, 1, length(?1) + length(?2)) = ?1 || ?2",
                params![dir, sep],
            )
            .map_err(map_err)?;
            tx.execute(
                "DELETE FROM group_covers WHERE dir_path = ?1
                   OR substr(dir_path, 1, length(?1) + length(?2)) = ?1 || ?2",
                params![dir, sep],
            )
            .map_err(map_err)?;
        }
        prune_orphans(&tx).map_err(map_err)?;
        rebuild_fts(&tx).map_err(map_err)?;
    }
    tx.commit().map_err(map_err)?;
    Ok(removed)
}

/// Every indexed model dir at or under `dir` — the consolidation check for
/// disk deletion asks "does this parent shelter any model NOT being
/// deleted?" before daring to trash the whole parent.
pub fn model_dirs_under(conn: &Connection, dir: &str) -> Result<Vec<String>, AppError> {
    let map_err = |e: rusqlite::Error| AppError::ConfigError(format!("Catalog read failed: {}", e));
    let sep = std::path::MAIN_SEPARATOR.to_string();
    let mut stmt = conn
        .prepare(
            "SELECT dir_path FROM models WHERE dir_path = ?1
               OR substr(dir_path, 1, length(?1) + length(?2)) = ?1 || ?2",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map(params![dir, sep], |row| row.get(0))
        .map_err(map_err)?
        .collect::<Result<Vec<String>, _>>()
        .map_err(map_err)?;
    Ok(rows)
}

/// The hash inputs for a model's app-data preview files: the dir_path itself
/// plus every variant_key filed under it. persist_preview names its copies
/// DefaultHasher(variant_key ?? dir_path) + timestamp, so deleting a model
/// must sweep by the same keys or the copies leak in app_data/previews
/// forever — nothing else ever prunes that folder.
pub fn preview_sweep_keys(conn: &Connection, dirs: &[String]) -> Result<Vec<String>, AppError> {
    let map_err = |e: rusqlite::Error| AppError::ConfigError(format!("Catalog read failed: {}", e));
    let sep = std::path::MAIN_SEPARATOR.to_string();
    let mut keys: Vec<String> = dirs.to_vec();
    let mut stmt = conn
        .prepare(
            "SELECT variant_key FROM variant_previews WHERE dir_path = ?1
               OR substr(dir_path, 1, length(?1) + length(?2)) = ?1 || ?2",
        )
        .map_err(map_err)?;
    for dir in dirs {
        let variant_keys = stmt
            .query_map(params![dir, sep], |row| row.get::<_, String>(0))
            .map_err(map_err)?
            .collect::<Result<Vec<String>, _>>()
            .map_err(map_err)?;
        keys.extend(variant_keys);
    }
    Ok(keys)
}

/// Indexed footprint of a set of model dirs (file_count, total_bytes),
/// prefix-scoped like the deletion it previews so the confirmation dialog
/// describes exactly what remove_models will take.
pub fn dirs_summary(conn: &Connection, dirs: &[String]) -> Result<(u32, i64), AppError> {
    let map_err = |e: rusqlite::Error| AppError::ConfigError(format!("Catalog read failed: {}", e));
    let sep = std::path::MAIN_SEPARATOR.to_string();
    let mut files: u32 = 0;
    let mut bytes: i64 = 0;
    let mut stmt = conn
        .prepare(
            "SELECT COUNT(*), COALESCE(SUM(size_bytes), 0) FROM files WHERE dir_path = ?1
               OR substr(dir_path, 1, length(?1) + length(?2)) = ?1 || ?2",
        )
        .map_err(map_err)?;
    for dir in dirs {
        let (f, b): (u32, i64) = stmt
            .query_row(params![dir, sep], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(map_err)?;
        files += f;
        bytes += b;
    }
    Ok((files, bytes))
}

/// Mark folders as soft-removed: future scans skip them (see the filter in
/// replace_catalog). INSERT OR REPLACE so re-removing refreshes the stamp.
pub fn add_scan_ignores(conn: &Connection, dirs: &[String]) -> Result<(), AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Ignore-list write failed: {}", e));
    let mut stmt = conn
        .prepare(
            "INSERT OR REPLACE INTO scan_ignores (dir_path, ignored_at)
             VALUES (?1, strftime('%s','now'))",
        )
        .map_err(map_err)?;
    for dir in dirs {
        stmt.execute([dir]).map_err(map_err)?;
    }
    Ok(())
}

/// Drop soft-remove markers at or under the given dirs. Used when the
/// folders are truly deleted from disk (nothing left to ignore) and by the
/// Settings "unignore" action (dir passed exactly).
pub fn remove_scan_ignores_under(conn: &Connection, dirs: &[String]) -> Result<(), AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Ignore-list prune failed: {}", e));
    let sep = std::path::MAIN_SEPARATOR.to_string();
    let mut stmt = conn
        .prepare(
            "DELETE FROM scan_ignores WHERE dir_path = ?1
               OR substr(dir_path, 1, length(?1) + length(?2)) = ?1 || ?2",
        )
        .map_err(map_err)?;
    for dir in dirs {
        stmt.execute(params![dir, sep]).map_err(map_err)?;
    }
    Ok(())
}

/// The soft-remove list, newest first, as (dir_path, ignored_at) pairs.
pub fn list_scan_ignores(conn: &Connection) -> Result<Vec<(String, i64)>, AppError> {
    let map_err = |e: rusqlite::Error| AppError::ConfigError(format!("Catalog read failed: {}", e));
    let mut stmt = conn
        .prepare("SELECT dir_path, ignored_at FROM scan_ignores ORDER BY ignored_at DESC, dir_path")
        .map_err(map_err)?;
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(map_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_err)?;
    Ok(rows)
}

/// Every distinct root the index knows about. Disk-side deletion uses these
/// as hard ceilings: folder consolidation may climb toward a root but never
/// reach it, so "delete the last model in a catalog" can never trash the
/// catalog folder itself.
pub fn known_roots(conn: &Connection) -> Result<Vec<String>, AppError> {
    let map_err = |e: rusqlite::Error| AppError::ConfigError(format!("Catalog read failed: {}", e));
    let mut stmt = conn
        .prepare("SELECT DISTINCT root FROM models WHERE root IS NOT NULL")
        .map_err(map_err)?;
    let rows = stmt
        .query_map([], |row| row.get(0))
        .map_err(map_err)?
        .collect::<Result<Vec<String>, _>>()
        .map_err(map_err)?;
    Ok(rows)
}

/// Repoint every indexed path after a model directory moves on disk.
/// model_tags is keyed by dir_path, and replace_catalog deletes tags whose
/// dir_path no longer matches a model — so skipping this doesn't just leave
/// the catalog stale, it silently loses user tags on the next rescan.
pub fn move_model(conn: &mut Connection, from: &str, to: &str) -> Result<(), AppError> {
    let map_err = |e: rusqlite::Error| AppError::ConfigError(format!("Catalog move failed: {}", e));
    let tx = conn.transaction().map_err(map_err)?;
    {
        // substr comparison instead of LIKE: paths may contain % or _
        tx.execute(
            "UPDATE models SET
                 preview_path = CASE
                     WHEN substr(preview_path, 1, length(?1) + 1) = ?1 || '/'
                     THEN ?2 || substr(preview_path, length(?1) + 1)
                     ELSE preview_path END,
                 dir_path = ?2
             WHERE dir_path = ?1",
            params![from, to],
        )
        .map_err(map_err)?;
        tx.execute(
            "UPDATE files SET
                 path = ?2 || substr(path, length(?1) + 1),
                 dir_path = ?2
             WHERE dir_path = ?1",
            params![from, to],
        )
        .map_err(map_err)?;
        // OR IGNORE + sweep: if the destination somehow already carries the
        // same tag, the PK collision shouldn't abort the whole move
        tx.execute(
            "UPDATE OR IGNORE model_tags SET dir_path = ?2 WHERE dir_path = ?1",
            params![from, to],
        )
        .map_err(map_err)?;
        tx.execute("DELETE FROM model_tags WHERE dir_path = ?1", [from])
            .map_err(map_err)?;
        tx.execute(
            "UPDATE OR IGNORE model_user_meta SET dir_path = ?2 WHERE dir_path = ?1",
            params![from, to],
        )
        .map_err(map_err)?;
        tx.execute("DELETE FROM model_user_meta WHERE dir_path = ?1", [from])
            .map_err(map_err)?;

        tx.execute("DELETE FROM models_fts WHERE dir_path = ?1", [from])
            .map_err(map_err)?;
        refresh_fts_row(&tx, to).map_err(map_err)?;
    }
    tx.commit().map_err(map_err)?;
    Ok(())
}

/// Apply the GROUP-level facts — designer, sculptor, release name/date —
/// to every other member of the group `dir_path` belongs to. A release is
/// a property of the MODEL, not of one build/pose folder: editing it in
/// the drawer must never leave sibling members claiming something else
/// (or nothing). Only Some values propagate; a member's existing override
/// is never cleared from here. Returns how many siblings were touched.
pub fn propagate_group_meta(
    conn: &Connection,
    dir_path: &str,
    designer: Option<&str>,
    sculptor: Option<&str>,
    release_name: Option<&str>,
    release_date: Option<&str>,
) -> Result<u32, AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Group meta propagation failed: {}", e));
    let mut stmt = conn
        .prepare(
            "SELECT m.dir_path FROM models m
             LEFT JOIN group_renames r ON r.source_group = COALESCE(m.group_name, m.name)
             WHERE lower(COALESCE(r.display_name, m.group_name, m.name)) =
                   (SELECT lower(COALESCE(r2.display_name, m2.group_name, m2.name))
                    FROM models m2
                    LEFT JOIN group_renames r2
                        ON r2.source_group = COALESCE(m2.group_name, m2.name)
                    WHERE m2.dir_path = ?1)
               AND m.dir_path <> ?1",
        )
        .map_err(map_err)?;
    let siblings: Vec<String> = stmt
        .query_map([dir_path], |row| row.get(0))
        .and_then(|rows| rows.collect())
        .map_err(map_err)?;

    for sibling in &siblings {
        conn.execute(
            "INSERT INTO model_user_meta
                 (dir_path, designer, sculptor, release_name, release_date)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(dir_path) DO UPDATE SET
                 designer     = COALESCE(?2, model_user_meta.designer),
                 sculptor     = COALESCE(?3, model_user_meta.sculptor),
                 release_name = COALESCE(?4, model_user_meta.release_name),
                 release_date = COALESCE(?5, model_user_meta.release_date)",
            params![sibling, designer, sculptor, release_name, release_date],
        )
        .map_err(map_err)?;
        // designer and release feed the FTS text — keep search in step
        refresh_fts_row(conn, sibling).map_err(map_err)?;
    }
    Ok(siblings.len() as u32)
}

/// Repoint every indexed path under `from` (a directory) to live under
/// `to` — the normalizer's whole-tree cousin of move_model. Covers the
/// tables move_model predates: file_variants, variant_previews (whose
/// variant_key embeds the dir path ahead of a \u{1f} separator) and
/// group_covers. PK columns update OR IGNORE + sweep so a collision can't
/// abort the batch. FTS rows for moved dirs are dropped here and rebuilt
/// once at finalize — per-row refresh during a thousand-move batch would
/// be pure waste.
pub fn move_tree_index(conn: &mut Connection, from: &str, to: &str) -> Result<(), AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Catalog tree move failed: {}", e));
    let sep = std::path::MAIN_SEPARATOR.to_string();
    let tx = conn.transaction().map_err(map_err)?;
    {
        // (table, column, part_of_primary_key)
        let columns: &[(&str, &str, bool)] = &[
            ("files", "path", true),
            ("files", "dir_path", false),
            ("models", "dir_path", true),
            ("models", "preview_path", false),
            ("model_tags", "dir_path", true),
            ("model_user_meta", "dir_path", true),
            ("model_user_meta", "preview_path", false),
            ("file_variants", "path", true),
            ("file_variants", "dir_path", false),
            ("variant_previews", "variant_key", true),
            ("variant_previews", "dir_path", false),
            ("variant_previews", "preview_path", false),
            ("group_covers", "dir_path", false),
            ("group_covers", "variant_key", false),
        ];
        for (table, column, is_pk) in columns {
            let verb = if *is_pk { "UPDATE OR IGNORE" } else { "UPDATE" };
            // substr comparison instead of LIKE: paths may contain % or _.
            // char(31) is the variant_key separator — a dir prefix can be
            // followed by either a path separator or that marker.
            let predicate = format!(
                "{c} = ?1 OR substr({c}, 1, length(?1) + 1) = ?1 || ?3
                       OR substr({c}, 1, length(?1) + 1) = ?1 || char(31)",
                c = column
            );
            tx.execute(
                &format!(
                    "{verb} {table} SET {c} = ?2 || substr({c}, length(?1) + 1) WHERE {p}",
                    verb = verb,
                    table = table,
                    c = column,
                    p = predicate
                ),
                params![from, to, sep],
            )
            .map_err(map_err)?;
            if *is_pk {
                // whatever still matches collided with an existing row
                // (?2 is unused by the predicate but keeps the indexes aligned)
                tx.execute(
                    &format!("DELETE FROM {table} WHERE {p}", table = table, p = predicate),
                    params![from, to, sep],
                )
                .map_err(map_err)?;
            }
        }
        tx.execute(
            "DELETE FROM models_fts
             WHERE dir_path = ?1 OR substr(dir_path, 1, length(?1) + 1) = ?1 || ?2",
            params![from, sep],
        )
        .map_err(map_err)?;
        // A dir move can cross catalog-folder boundaries (staging mode
        // drains a raw folder's models into the primary) — the moved
        // rows' root stamp is now stale, and this helper has no notion of
        // configured catalog roots to recompute it. Clear it instead: the
        // NULL-adoption fallback the scoped scan/purge queries already
        // carry (see replace_catalog) treats NULL as "claimed by whichever
        // folder's prefix matches", so the rows stay correctly discoverable
        // and counted immediately, and get re-stamped by the next scan of
        // wherever they now live.
        tx.execute(
            "UPDATE files SET root = NULL
             WHERE dir_path = ?1 OR substr(dir_path, 1, length(?1) + 1) = ?1 || ?2",
            params![to, sep],
        )
        .map_err(map_err)?;
        tx.execute(
            "UPDATE models SET root = NULL
             WHERE dir_path = ?1 OR substr(dir_path, 1, length(?1) + 1) = ?1 || ?2",
            params![to, sep],
        )
        .map_err(map_err)?;
    }
    tx.commit().map_err(map_err)
}

/// Repoint one file's index rows after a per-file move/rename.
pub fn move_file_index(conn: &mut Connection, from: &str, to: &str) -> Result<(), AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Catalog file move failed: {}", e));
    let to_path = std::path::Path::new(to);
    let dir = to_path
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = to_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tx = conn.transaction().map_err(map_err)?;
    {
        tx.execute(
            "UPDATE OR IGNORE files SET path = ?2, dir_path = ?3, file_name = ?4, root = NULL WHERE path = ?1",
            params![from, to, dir, name],
        )
        .map_err(map_err)?;
        tx.execute("DELETE FROM files WHERE path = ?1", [from])
            .map_err(map_err)?;
        tx.execute(
            "UPDATE OR IGNORE file_variants SET path = ?2, dir_path = ?3 WHERE path = ?1",
            params![from, to, dir],
        )
        .map_err(map_err)?;
        tx.execute("DELETE FROM file_variants WHERE path = ?1", [from])
            .map_err(map_err)?;
    }
    tx.commit().map_err(map_err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::db::*;
    use crate::catalog::db::test_util::*;
    use crate::catalog::ModelRow;

    #[test]
    fn remove_models_takes_nested_rows_and_side_tables() {
        let mut conn = test_conn();
        let (mut files, mut models, tags) = sample_rows();
        // A file filed under a SUBFOLDER of the doomed dir: deletion is
        // prefix-scoped because the disk delete is recursive — an exact-match
        // delete would leave this row pointing at trashed bytes.
        files.push(file_row("/lib/newt/supported/GiantNewt_sup.stl", "/lib/newt/supported", 1024));
        models.push(ModelRow {
            dir_path: "/lib/newt/supported".into(),
            name: "Giant Newt".into(),
            source: "heuristic".into(),
            file_count: 1,
            total_size_bytes: 1024,
            group_name: Some("Giant Newt".into()),
            ..Default::default()
        });
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();
        // The side tables prune_orphans does NOT sweep — remove_models must
        // take these explicitly or curation ghosts survive the delete
        conn.execute(
            "INSERT INTO variant_previews (variant_key, dir_path, preview_path)
             VALUES ('/lib/newt\u{241F}sword\u{241F}brave', '/lib/newt', '/previews/abc.png')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO group_covers (group_name, dir_path, variant_key)
             VALUES ('Giant Newt', '/lib/newt', NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO packs (model_dir, archive_path, archive_size_bytes)
             VALUES ('/lib/newt', '/lib/newt/model.plinthpack', 512)",
            [],
        )
        .unwrap();

        let sweep_keys = preview_sweep_keys(&conn, &["/lib/newt".to_string()]).unwrap();
        assert!(sweep_keys.contains(&"/lib/newt".to_string()));
        assert!(sweep_keys.contains(&"/lib/newt\u{241F}sword\u{241F}brave".to_string()));

        let removed = remove_models(&mut conn, &["/lib/newt".to_string()]).unwrap();
        assert_eq!(removed, 2, "the nested supported/ model row goes too");

        let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(count("SELECT COUNT(*) FROM models"), 1);
        assert_eq!(count("SELECT COUNT(*) FROM files"), 1);
        assert_eq!(count("SELECT COUNT(*) FROM model_tags"), 0);
        assert_eq!(count("SELECT COUNT(*) FROM variant_previews"), 0);
        assert_eq!(count("SELECT COUNT(*) FROM group_covers"), 0);
        assert_eq!(count("SELECT COUNT(*) FROM packs"), 0);
        // FTS forgot the deleted model but still finds the survivor
        assert_eq!(search(&conn, "newt", &[], None, None, None, None, 10, 0, true, None, None).unwrap().total, 0);
        assert_eq!(search(&conn, "bugbear", &[], None, None, None, None, 10, 0, true, None, None).unwrap().total, 1);

        let (file_count, bytes) = dirs_summary(&conn, &["/lib/bugbear".to_string()]).unwrap();
        assert_eq!((file_count, bytes), (1, 4096));
    }

    #[test]
    fn cross_root_dir_move_unclaims_the_row_instead_of_stranding_it() {
        let mut conn = test_conn();
        let (files, models, _) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &[], &[], &[]).unwrap();

        // stage "newt" from /lib into a second folder, exactly what the
        // normalizer's wholesale dir move does for a primary/staging target
        move_tree_index(&mut conn, "/lib/newt", "/primary/DTL/Giant Newt").unwrap();

        // the row is unclaimed (NULL), not left claiming a root it no
        // longer lives under — the prefix fallback still finds it under
        // its NEW location...
        let root: Option<String> = conn
            .query_row(
                "SELECT root FROM models WHERE dir_path = '/primary/DTL/Giant Newt'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(root, None);
        let (m, f, _) = root_summary(&conn, "/primary").unwrap();
        assert_eq!((m, f), (1, 1), "counted under its new folder pre-rescan");
        let (m, f, _) = root_summary(&conn, "/lib").unwrap();
        assert_eq!(f, 1, "bugbear only — newt no longer attributed to /lib");
        let _ = m;

        // ...and critically: rescanning the OLD folder (now missing the
        // moved model on disk) must not delete the staged row. Before the
        // root=NULL fix this failed — the row still said root='/lib' and
        // the scoped delete caught it even though it had moved away.
        let bugbear_only: Vec<_> = files
            .iter()
            .filter(|f| f.dir_path == "/lib/bugbear")
            .cloned()
            .collect();
        let bugbear_model: Vec<_> = models
            .iter()
            .filter(|m| m.dir_path == "/lib/bugbear")
            .cloned()
            .collect();
        replace_catalog(&mut conn, "/lib", &bugbear_only, &bugbear_model, &[], &[], &[]).unwrap();
        assert_eq!(
            search(&conn, "newt", &[], None, None, None, None, 10, 0, true, None, None).unwrap().total,
            1,
            "staged model must survive a rescan of the folder it moved OUT of"
        );
    }

    #[test]
    fn cross_root_file_move_unclaims_the_row() {
        let mut conn = test_conn();
        let (files, models, _) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &[], &[], &[]).unwrap();

        move_file_index(
            &mut conn,
            "/lib/newt/GiantNewt_v02.stl",
            "/primary/DTL/Giant Newt/GiantNewt_v02.stl",
        )
        .unwrap();

        let root: Option<String> = conn
            .query_row(
                "SELECT root FROM files WHERE path = '/primary/DTL/Giant Newt/GiantNewt_v02.stl'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(root, None);
    }

    #[test]
    fn remove_files_prunes_dups_and_recounts_models() {
        let mut conn = test_conn();
        let (mut files, models, tags) = sample_rows();
        // two identical-content files -> one duplicate group
        files[1].size_bytes = 2048;
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();
        store_hash(&conn, &files[0].path, "same").unwrap();
        store_hash(&conn, &files[1].path, "same").unwrap();
        assert_eq!(duplicate_groups(&conn).unwrap().len(), 1);

        remove_files(&mut conn, &[files[1].path.clone()]).unwrap();

        // group dissolves without a rescan, and the model's counters follow
        assert!(duplicate_groups(&conn).unwrap().is_empty());
        let (count, size): (u32, i64) = conn
            .query_row(
                "SELECT file_count, total_size_bytes FROM models WHERE dir_path = '/lib/bugbear'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(count, 0);
        assert_eq!(size, 0);
    }

    #[test]
    fn move_model_repoints_index_and_keeps_tags_through_rescan() {
        let mut conn = test_conn();
        let (files, models, tags) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();
        add_tag(&conn, "/lib/newt", "painted").unwrap();

        move_model(&mut conn, "/lib/newt", "/lib/amphibians/newt").unwrap();

        // model, files and search index all follow the new path
        let page = search(&conn, "newt", &[], None, None, None, None, 10, 0, true, None, None).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.entries[0].dir_path, "/lib/amphibians/newt");
        assert!(page.entries[0].tags.contains(&"painted".to_string()));
        let moved_file: String = conn
            .query_row(
                "SELECT path FROM files WHERE dir_path = '/lib/amphibians/newt'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(moved_file, "/lib/amphibians/newt/GiantNewt_v02.stl");

        // the regression this guards: a rescan reflecting the new location
        // must not drop the user tag (model_tags is keyed by dir_path)
        let (mut files, mut models, _) = sample_rows();
        files[0].path = "/lib/amphibians/newt/GiantNewt_v02.stl".into();
        files[0].dir_path = "/lib/amphibians/newt".into();
        models[0].dir_path = "/lib/amphibians/newt".into();
        replace_catalog(&mut conn, "/lib", &files, &models, &[], &[], &[]).unwrap();
        assert_eq!(
            search(&conn, "", &["painted".to_string()], None, None, None, None, 10, 0, true, None, None)
                .unwrap()
                .total,
            1
        );
    }

    #[test]
    fn group_meta_propagates_to_every_member() {
        // Release/designer are facts about the MODEL: editing them on the
        // selected member must reach every sibling in the group — poses
        // and variants showing an empty release beside a filled-in primary
        // was the drawer lying about its own model.
        let mut conn = test_conn();
        let member = |dir: &str, group: &str| ModelRow {
            dir_path: dir.into(),
            name: group.into(),
            description: None,
            designer: None,
            release_name: None,
            preview_path: None,
            source: "metadata".into(),
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
            group_name: Some(group.into()),
            ..Default::default()
        };
        let models = vec![
            member("/lib/LK/Supported", "Little Knights"),
            member("/lib/LK/Unsupported", "Little Knights"),
            member("/lib/Peryton", "Peryton"),
        ];
        replace_catalog(&mut conn, "/lib", &[], &models, &[], &[], &[]).unwrap();
        // the sibling already carries a sculptor override — None fields
        // must never clobber it
        update_model_user_meta(
            &conn,
            "/lib/LK/Unsupported",
            None,
            None,
            None,
            None,
            None,
            None,
            Some("A. Artist".into()),
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let touched = propagate_group_meta(
            &conn,
            "/lib/LK/Supported",
            Some("Dragon Trapper's Lodge"),
            None,
            Some("Order of the Unicorn"),
            Some("2026-05"),
        )
        .unwrap();
        assert_eq!(touched, 1, "one sibling in the group");

        let (designer, release, date, sculptor): (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT designer, release_name, release_date, sculptor
                 FROM model_user_meta WHERE dir_path = '/lib/LK/Unsupported'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(designer.as_deref(), Some("Dragon Trapper's Lodge"));
        assert_eq!(release.as_deref(), Some("Order of the Unicorn"));
        assert_eq!(date.as_deref(), Some("2026-05"));
        assert_eq!(sculptor.as_deref(), Some("A. Artist"), "None must not clobber");

        // the foreign group is untouched
        let foreign: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM model_user_meta WHERE dir_path = '/lib/Peryton'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(foreign, 0);
    }

    #[test]
    fn user_meta_follows_a_model_move() {
        let mut conn = test_conn();
        let (files, models, tags) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();
        update_model_user_meta(
            &conn,
            "/lib/newt",
            Some("Shiny Newt".into()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

        move_model(&mut conn, "/lib/newt", "/lib/amphibians/newt").unwrap();

        let page = search(&conn, "shiny", &[], None, None, None, None, 10, 0, true, None, None).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.entries[0].dir_path, "/lib/amphibians/newt");
    }
}
