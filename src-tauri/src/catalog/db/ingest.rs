use crate::error::AppError;
use rusqlite::{params, Connection};

use crate::catalog::{CatalogStats, ExtensionStat, FileRow, FileVariantRow, ModelRow, PackRow};

/// Replace one root's slice of the indexed catalog in one transaction.
/// Other roots' rows are untouched, so huge collections can be indexed one
/// folder at a time. User tags survive; metadata tags are refreshed from
/// the scan.
///
/// Rows with a NULL root predate multi-root support; the scan of whichever
/// root contains them adopts (deletes and re-inserts) them, so migrating an
/// old index is just a rescan. The containment check is path-segment aware:
/// a root of "/lib/a" must not claim "/lib/ab" (mirrors normalize::is_under).
pub fn replace_catalog(
    conn: &mut Connection,
    root: &str,
    files: &[FileRow],
    models: &[ModelRow],
    metadata_tags: &[(String, String)],
    metadata_file_variants: &[FileVariantRow],
    packs: &[PackRow],
) -> Result<(), AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Catalog write failed: {}", e));
    // A picker-supplied "D:\" or a hand-typed trailing slash must scope the
    // same as its bare form, or the same folder scans as two disjoint roots.
    let trimmed = root.trim_end_matches(std::path::MAIN_SEPARATOR);
    let root = if trimmed.is_empty() { root } else { trimmed };
    let sep = std::path::MAIN_SEPARATOR.to_string();
    let tx = conn.transaction().map_err(map_err)?;
    {
        // Preserve known content hashes (hashing is the expensive part of
        // duplicate detection) and file identities across the rebuild
        tx.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS old_hashes AS
                 SELECT path, size_bytes, modified_at, content_hash, file_identity
                 FROM files WHERE content_hash IS NOT NULL OR file_identity IS NOT NULL;",
        )
        .map_err(map_err)?;

        // substr instead of LIKE/GLOB: paths may contain %, _, [ and *
        tx.execute(
            "DELETE FROM files WHERE root = ?1
               OR (root IS NULL AND (dir_path = ?1
                   OR substr(dir_path, 1, length(?1) + length(?2)) = ?1 || ?2))",
            params![root, sep],
        )
        .map_err(map_err)?;
        tx.execute(
            "DELETE FROM models WHERE root = ?1
               OR (root IS NULL AND (dir_path = ?1
                   OR substr(dir_path, 1, length(?1) + length(?2)) = ?1 || ?2))",
            params![root, sep],
        )
        .map_err(map_err)?;
        // Scoped like files/models: another root's metadata tags only come
        // back when THAT root rescans, so this scan must not shed them.
        tx.execute(
            "DELETE FROM model_tags WHERE source = 'metadata'
               AND (dir_path = ?1
                   OR substr(dir_path, 1, length(?1) + length(?2)) = ?1 || ?2)",
            params![root, sep],
        )
        .map_err(map_err)?;

        // Soft-removed folders never re-enter: filter the scan's rows
        // against the ignore list at the door. Tags need no filter of their
        // own — prune_orphans below drops any tag whose model wasn't
        // inserted, and the file_variants import selects FROM files.
        let ignored: Vec<String> = {
            let mut stmt = tx.prepare("SELECT dir_path FROM scan_ignores").map_err(map_err)?;
            let rows = stmt
                .query_map([], |row| row.get(0))
                .map_err(map_err)?
                .collect::<Result<Vec<String>, _>>()
                .map_err(map_err)?;
            rows
        };
        let is_ignored = |p: &str| {
            ignored.iter().any(|ig| {
                p == ig.as_str()
                    || p.strip_prefix(ig.as_str())
                        .is_some_and(|rest| rest.starts_with(std::path::MAIN_SEPARATOR))
            })
        };

        let mut insert_file = tx
            .prepare(
                "INSERT OR REPLACE INTO files
                 (path, dir_path, file_name, extension, size_bytes, modified_at,
                  archive_path, content_hash, root, indexed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, strftime('%s','now'))",
            )
            .map_err(map_err)?;
        for f in files {
            if is_ignored(&f.dir_path) {
                continue;
            }
            insert_file
                .execute(params![
                    f.path,
                    f.dir_path,
                    f.file_name,
                    f.extension,
                    f.size_bytes,
                    f.modified_at,
                    f.archive_path,
                    f.content_hash,
                    root
                ])
                .map_err(map_err)?;
        }
        drop(insert_file);

        // Restore hashes and identities for files that didn't change. Guarded
        // by EXISTS so no-match rows keep their scan-seeded values (pack
        // sidecars arrive with a content_hash) instead of being nulled by the
        // empty subquery. Packed rows never take an old identity: the loose
        // inode it named was deleted when the model was packed.
        tx.execute(
            "UPDATE files SET
                 (content_hash, file_identity) = (
                 SELECT COALESCE(files.content_hash, oh.content_hash),
                        CASE WHEN files.archive_path IS NULL THEN oh.file_identity END
                 FROM old_hashes oh
                 WHERE oh.path = files.path
                   AND oh.size_bytes = files.size_bytes
                   AND oh.modified_at = files.modified_at
             )
             WHERE EXISTS (
                 SELECT 1 FROM old_hashes oh
                 WHERE oh.path = files.path
                   AND oh.size_bytes = files.size_bytes
                   AND oh.modified_at = files.modified_at
             )",
            [],
        )
        .map_err(map_err)?;
        tx.execute("DROP TABLE old_hashes", []).map_err(map_err)?;

        // Packs are derived from disk (pack.json sidecars), so they rebuild
        // with files/models. Scoped by model_dir like the metadata tags —
        // the table has no root column, and needs none: a scan re-reads
        // every sidecar under its root, so the path prefix is exact.
        tx.execute(
            "DELETE FROM packs WHERE model_dir = ?1
               OR substr(model_dir, 1, length(?1) + length(?2)) = ?1 || ?2",
            params![root, sep],
        )
        .map_err(map_err)?;
        let mut insert_pack = tx
            .prepare(
                "INSERT OR REPLACE INTO packs
                 (model_dir, archive_path, archive_size_bytes, archive_checksum, packed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .map_err(map_err)?;
        for p in packs {
            if is_ignored(&p.model_dir) {
                continue;
            }
            insert_pack
                .execute(params![
                    p.model_dir,
                    p.archive_path,
                    p.archive_size_bytes,
                    p.archive_checksum,
                    p.packed_at
                ])
                .map_err(map_err)?;
        }
        drop(insert_pack);

        let mut insert_model = tx
            .prepare(
                "INSERT OR REPLACE INTO models
                 (dir_path, name, description, designer, release_name, preview_path,
                  source, uuid, file_count, total_size_bytes, pose, scale, support_status,
                  release_date, group_name, sculptor, variant, base_round,
                  base_square, root, rotation, dims_mm, part_count, indexed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                  ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, strftime('%s','now'))",
            )
            .map_err(map_err)?;
        for m in models {
            if is_ignored(&m.dir_path) {
                continue;
            }
            insert_model
                .execute(params![
                    m.dir_path,
                    m.name,
                    m.description,
                    m.designer,
                    m.release_name,
                    m.preview_path,
                    m.source,
                    m.uuid,
                    m.file_count,
                    m.total_size_bytes,
                    m.pose,
                    m.scale,
                    m.support_status,
                    m.release_date,
                    m.group_name,
                    m.sculptor,
                    m.variant,
                    m.base_round_mm,
                    m.base_square_mm,
                    root,
                    m.rotation,
                    m.dims_mm,
                    m.part_count
                ])
                .map_err(map_err)?;
        }
        drop(insert_model);

        let mut insert_tag = tx
            .prepare(
                "INSERT OR IGNORE INTO model_tags (dir_path, tag, source)
                 VALUES (?1, ?2, 'metadata')",
            )
            .map_err(map_err)?;
        for (dir_path, tag) in metadata_tags {
            insert_tag
                .execute(params![dir_path, tag])
                .map_err(map_err)?;
        }
        drop(insert_tag);

        prune_orphans(&tx).map_err(map_err)?;
        // Seed file-pose splits carried in model.json (the 3pk read side).
        // OR IGNORE: a user's own assignment (same path PK) always wins, and
        // metadata rows survive the rescan above just like user ones.
        {
            let mut import = tx
                .prepare(
                    "INSERT OR IGNORE INTO file_variants
                         (path, dir_path, variant, pose, support_status)
                     SELECT ?1, dir_path, ?2, ?3, ?4 FROM files WHERE path = ?1",
                )
                .map_err(map_err)?;
            for fv in metadata_file_variants {
                import
                    .execute(params![fv.path, fv.variant, fv.pose, fv.support_status])
                    .map_err(map_err)?;
            }
        }
        rebuild_fts(&tx).map_err(map_err)?;

        // Global stamp feeds the stats footer; the per-root stamp lets the
        // roots UI say which folders have gone stale since their last scan.
        tx.execute(
            "INSERT OR REPLACE INTO meta (key, value)
             VALUES ('last_scan', strftime('%s','now'))",
            [],
        )
        .map_err(map_err)?;
        tx.execute(
            "INSERT OR REPLACE INTO meta (key, value)
             VALUES ('last_scan:' || ?1, strftime('%s','now'))",
            params![root],
        )
        .map_err(map_err)?;
    }
    tx.commit().map_err(map_err)?;
    Ok(())
}

/// Drop rows in the path-keyed side tables whose model or file is no longer
/// indexed. Shared by rescan and root removal — anything that deletes from
/// files/models must sweep these or user curation outlives its subject and
/// silently reattaches if the same path is ever indexed again.
pub(super) fn prune_orphans(tx: &rusqlite::Transaction) -> Result<(), rusqlite::Error> {
    tx.execute(
        "DELETE FROM model_tags
         WHERE dir_path NOT IN (SELECT dir_path FROM models)",
        [],
    )?;
    tx.execute(
        "DELETE FROM model_user_meta
         WHERE dir_path NOT IN (SELECT dir_path FROM models)",
        [],
    )?;
    tx.execute(
        "DELETE FROM file_variants
         WHERE path NOT IN (SELECT path FROM files)",
        [],
    )?;
    tx.execute(
        "DELETE FROM group_renames
         WHERE lower(source_group) NOT IN
             (SELECT DISTINCT lower(COALESCE(group_name, name)) FROM models)",
        [],
    )?;
    Ok(())
}

/// Remove one root's slice from the index — the "remove catalog folder"
/// path. Scoped exactly like replace_catalog (including adoption of legacy
/// NULL-root rows), so removing a folder that predates multi-root cleans up
/// fully. User tags/metadata for the removed models are pruned with them;
/// the durable copy of curation is the model.json sidecars on disk.
pub fn purge_root(conn: &mut Connection, root: &str) -> Result<(), AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Catalog root removal failed: {}", e));
    let trimmed = root.trim_end_matches(std::path::MAIN_SEPARATOR);
    let root = if trimmed.is_empty() { root } else { trimmed };
    let sep = std::path::MAIN_SEPARATOR.to_string();
    let tx = conn.transaction().map_err(map_err)?;
    {
        tx.execute(
            "DELETE FROM files WHERE root = ?1
               OR (root IS NULL AND (dir_path = ?1
                   OR substr(dir_path, 1, length(?1) + length(?2)) = ?1 || ?2))",
            params![root, sep],
        )
        .map_err(map_err)?;
        tx.execute(
            "DELETE FROM models WHERE root = ?1
               OR (root IS NULL AND (dir_path = ?1
                   OR substr(dir_path, 1, length(?1) + length(?2)) = ?1 || ?2))",
            params![root, sep],
        )
        .map_err(map_err)?;
        tx.execute(
            "DELETE FROM packs WHERE model_dir = ?1
               OR substr(model_dir, 1, length(?1) + length(?2)) = ?1 || ?2",
            params![root, sep],
        )
        .map_err(map_err)?;
        prune_orphans(&tx).map_err(map_err)?;
        rebuild_fts(&tx).map_err(map_err)?;
        tx.execute(
            "DELETE FROM meta WHERE key = 'last_scan:' || ?1",
            params![root],
        )
        .map_err(map_err)?;
        // Soft-remove markers under this root go with it: they exist to
        // shape scans of the root, and the root is no longer scanned
        tx.execute(
            "DELETE FROM scan_ignores WHERE dir_path = ?1
               OR substr(dir_path, 1, length(?1) + length(?2)) = ?1 || ?2",
            params![root, sep],
        )
        .map_err(map_err)?;
    }
    tx.commit().map_err(map_err)?;
    Ok(())
}

/// Indexed footprint of one root: (model_count, file_count, total_bytes).
/// Uses the same containment rules as the scoped deletes, so legacy
/// NULL-root rows under the folder are counted as its own.
pub fn root_summary(conn: &Connection, root: &str) -> Result<(u32, u32, i64), AppError> {
    let map_err = |e: rusqlite::Error| AppError::ConfigError(format!("Catalog read failed: {}", e));
    let trimmed = root.trim_end_matches(std::path::MAIN_SEPARATOR);
    let root = if trimmed.is_empty() { root } else { trimmed };
    let sep = std::path::MAIN_SEPARATOR.to_string();
    let models: u32 = conn
        .query_row(
            "SELECT COUNT(*) FROM models WHERE root = ?1
               OR (root IS NULL AND (dir_path = ?1
                   OR substr(dir_path, 1, length(?1) + length(?2)) = ?1 || ?2))",
            params![root, sep],
            |r| r.get(0),
        )
        .map_err(map_err)?;
    let (files, bytes): (u32, i64) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(size_bytes), 0) FROM files WHERE root = ?1
               OR (root IS NULL AND (dir_path = ?1
                   OR substr(dir_path, 1, length(?1) + length(?2)) = ?1 || ?2))",
            params![root, sep],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(map_err)?;
    Ok((models, files, bytes))
}

/// Per-root last-scan times, as (root, epoch) pairs — one row per root that
/// has ever completed a scan into this index.
pub fn root_scan_times(conn: &Connection) -> Result<Vec<(String, i64)>, AppError> {
    let map_err = |e: rusqlite::Error| AppError::ConfigError(format!("Catalog read failed: {}", e));
    let mut stmt = conn
        .prepare(
            "SELECT substr(key, length('last_scan:') + 1), CAST(value AS INTEGER)
             FROM meta WHERE key LIKE 'last_scan:%'",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(map_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_err)?;
    Ok(rows)
}

// The group's display name is folded into the tags text so a search for a
// RENAMED group ("Stone Guardian") still finds its member rows, whose own
// names may say something else entirely ("galeb duhr A").
/// Fold apostrophes and hyphens out of a SQL text expression so a query
/// typed without them ("trappers", "presupported") still matches the
/// indexed value ("Trapper's", "pre-supported"). Must mirror the query-side
/// stripping in `fts_query`. char(8217)/char(8216) are the curly quotes.
fn fts_norm(expr: &str) -> String {
    format!(
        "REPLACE(REPLACE(REPLACE(REPLACE({e}, '''', ''), '-', ''), char(8217), ''), char(8216), '')",
        e = expr
    )
}

/// The INSERT that (re)builds an FTS row. designer + sculptor ride in the
/// free-text `tags` column rather than their own FTS columns, keeping the
/// virtual table shape stable while making both searchable.
fn fts_insert_select() -> String {
    format!(
        "INSERT INTO models_fts (name, description, tags, dir_path)
         SELECT {name}, COALESCE(m.description, ''), {tags}, m.dir_path
         FROM models m
         LEFT JOIN model_user_meta u ON u.dir_path = m.dir_path
         LEFT JOIN group_renames r ON r.source_group = COALESCE(m.group_name, m.name)",
        name = fts_norm("COALESCE(u.custom_name, m.name)"),
        tags = fts_norm(
            "COALESCE((SELECT group_concat(t.tag, ' ') FROM model_tags t
                       WHERE t.dir_path = m.dir_path), '')
                 || ' ' || COALESCE(r.display_name, '')
                 || ' ' || COALESCE(m.group_name, '')
                 || ' ' || COALESCE(u.designer, m.designer, '')
                 || ' ' || COALESCE(u.sculptor, m.sculptor, '')
                 || ' ' || COALESCE(u.release_name, m.release_name, '')
                 || ' ' || COALESCE(u.variant, m.variant, '')"
        ),
    )
}

pub(super) fn rebuild_fts(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute("DELETE FROM models_fts", [])?;
    conn.execute(&fts_insert_select(), [])?;
    Ok(())
}

/// Refresh the FTS row for one model after a tag or user-meta change.
pub(super) fn refresh_fts_row(conn: &Connection, dir_path: &str) -> Result<(), rusqlite::Error> {
    conn.execute("DELETE FROM models_fts WHERE dir_path = ?1", [dir_path])?;
    conn.execute(
        &format!("{} WHERE m.dir_path = ?1", fts_insert_select()),
        [dir_path],
    )?;
    Ok(())
}

pub fn stats(conn: &Connection) -> Result<CatalogStats, AppError> {
    let map_err = |e: rusqlite::Error| AppError::ConfigError(format!("Stats query failed: {}", e));
    let (total_files, total_size): (u32, i64) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(size_bytes), 0) FROM files",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(map_err)?;
    // Hardlinked paths (same file_identity) occupy the disk once, however
    // many names they carry — subtract the extra names so the headline size
    // reports actual disk usage. Only duplicate-scan candidates carry an
    // identity, so this subquery stays small at any library size.
    let shared_savings: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(size_bytes * (n - 1)), 0) FROM (
                 SELECT MAX(size_bytes) AS size_bytes, COUNT(*) AS n FROM files
                 WHERE file_identity IS NOT NULL
                 GROUP BY file_identity HAVING COUNT(*) > 1
             )",
            [],
            |row| row.get(0),
        )
        .map_err(map_err)?;
    let total_size = total_size - shared_savings;
    let total_models: u32 = conn
        .query_row("SELECT COUNT(*) FROM models", [], |row| row.get(0))
        .map_err(map_err)?;
    let last_scan: Option<f64> = conn
        .query_row(
            "SELECT CAST(value AS REAL) FROM meta WHERE key = 'last_scan'",
            [],
            |row| row.get(0),
        )
        .ok();

    let mut stmt = conn
        .prepare(
            "SELECT extension, COUNT(*), SUM(size_bytes) FROM files
             GROUP BY extension ORDER BY SUM(size_bytes) DESC",
        )
        .map_err(map_err)?;
    let extensions = stmt
        .query_map([], |row| {
            Ok(ExtensionStat {
                extension: row.get(0)?,
                file_count: row.get(1)?,
                total_size_bytes: row.get::<_, i64>(2)? as f64,
            })
        })
        .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        .map_err(map_err)?;

    // Compressed-at-rest savings: what packed files would occupy loose vs
    // what their archives actually take on disk
    let (packed_models, packed_archive): (u32, i64) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(archive_size_bytes), 0) FROM packs",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(map_err)?;
    let packed_logical: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(size_bytes), 0) FROM files WHERE archive_path IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .map_err(map_err)?;

    Ok(CatalogStats {
        total_models,
        total_files,
        total_size_bytes: total_size as f64,
        extensions,
        last_scan_epoch: last_scan,
        packed_models,
        packed_logical_bytes: packed_logical as f64,
        packed_archive_bytes: packed_archive as f64,
    })
}

/// Rebuild the FTS index from scratch — the batch-move closer.
pub fn rebuild_search_index(conn: &Connection) -> Result<(), AppError> {
    rebuild_fts(conn)
        .map_err(|e| AppError::ConfigError(format!("Search index rebuild failed: {}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::db::*;
    use crate::catalog::db::test_util::*;

    #[test]
    fn fts_prefix_search_finds_models() {
        let mut conn = test_conn();
        let (files, models, tags) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();

        // prefix match on name
        let page = search(&conn, "new", &[], None, None, None, None, 10, 0, true, None, None).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.entries[0].name, "Giant Newt");
        assert_eq!(page.entries[0].tags, vec!["amphibian"]);

        // tag search through FTS
        let page = search(&conn, "amphib", &[], None, None, None, None, 10, 0, true, None, None).unwrap();
        assert_eq!(page.total, 1);

        // empty query lists everything
        let page = search(&conn, "", &[], None, None, None, None, 10, 0, true, None, None).unwrap();
        assert_eq!(page.total, 2);

        // tag filter
        let page = search(&conn, "", &["amphibian".to_string()], None, None, None, None, 10, 0, true, None, None).unwrap();
        assert_eq!(page.total, 1);

        // no match
        let page = search(&conn, "dragon", &[], None, None, None, None, 10, 0, true, None, None).unwrap();
        assert_eq!(page.total, 0);
    }

    #[test]
    fn soft_removed_folders_stay_gone_across_rescans() {
        let mut conn = test_conn();
        let (files, models, tags) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();

        // Soft remove the newt: rows go, marker stays
        add_scan_ignores(&conn, &["/lib/newt".to_string()]).unwrap();
        remove_models(&mut conn, &["/lib/newt".to_string()]).unwrap();
        assert_eq!(search(&conn, "", &[], None, None, None, None, 10, 0, true, None, None).unwrap().total, 1);

        // The rescan walks the SAME disk state (newt still exists on disk) —
        // the whole point: it must not resurrect what the user removed
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();
        assert_eq!(search(&conn, "newt", &[], None, None, None, None, 10, 0, true, None, None).unwrap().total, 0);
        assert_eq!(search(&conn, "bugbear", &[], None, None, None, None, 10, 0, true, None, None).unwrap().total, 1);
        let file_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(file_count, 1, "ignored dir's files filtered at the door");

        // Unignore + rescan brings it back
        remove_scan_ignores_under(&conn, &["/lib/newt".to_string()]).unwrap();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();
        assert_eq!(search(&conn, "newt", &[], None, None, None, None, 10, 0, true, None, None).unwrap().total, 1);
    }

    #[test]
    fn user_tags_survive_rescan_and_metadata_tags_refresh() {
        let mut conn = test_conn();
        let (files, models, tags) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();

        add_tag(&conn, "/lib/newt", "painted").unwrap();
        // searchable immediately
        assert_eq!(search(&conn, "painted", &[], None, None, None, None, 10, 0, true, None, None).unwrap().total, 1);

        // rescan with metadata tags gone: user tag survives, metadata tag drops
        replace_catalog(&mut conn, "/lib", &files, &models, &[], &[], &[]).unwrap();
        let page = search(&conn, "", &["painted".to_string()], None, None, None, None, 10, 0, true, None, None).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(
            search(&conn, "", &["amphibian".to_string()], None, None, None, None, 10, 0, true, None, None)
                .unwrap()
                .total,
            0
        );

        // a model that disappeared from disk takes its user tags with it
        let (files, models, _) = sample_rows();
        let only_bugbear_files: Vec<_> = files
            .into_iter()
            .filter(|f| f.dir_path == "/lib/bugbear")
            .collect();
        let only_bugbear_models: Vec<_> = models
            .into_iter()
            .filter(|m| m.dir_path == "/lib/bugbear")
            .collect();
        replace_catalog(
            &mut conn,
            "/lib",
            &only_bugbear_files,
            &only_bugbear_models,
            &[],
            &[],
            &[],
        )
        .unwrap();
        let remaining: u32 = conn
            .query_row("SELECT COUNT(*) FROM model_tags", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 0);
    }

    #[test]
    fn scans_replace_only_their_own_root() {
        let mut conn = test_conn();
        let (files, models, tags) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();

        let other_files = vec![file_row("/other/wyvern/wyvern.stl", "/other/wyvern", 10)];
        let other_models = vec![model_row("/other/wyvern", "Wyvern")];
        replace_catalog(&mut conn, "/other", &other_files, &other_models, &[], &[], &[]).unwrap();

        // both roots coexist in one index
        assert_eq!(search(&conn, "", &[], None, None, None, None, 10, 0, true, None, None).unwrap().total, 3);
        assert_eq!(search(&conn, "wyvern", &[], None, None, None, None, 10, 0, true, None, None).unwrap().total, 1);
        assert_eq!(search(&conn, "newt", &[], None, None, None, None, 10, 0, true, None, None).unwrap().total, 1);

        // a root whose scan comes back empty disappears — its sibling doesn't
        replace_catalog(&mut conn, "/other", &[], &[], &[], &[], &[]).unwrap();
        assert_eq!(search(&conn, "wyvern", &[], None, None, None, None, 10, 0, true, None, None).unwrap().total, 0);
        assert_eq!(search(&conn, "newt", &[], None, None, None, None, 10, 0, true, None, None).unwrap().total, 1);
    }

    #[test]
    fn other_roots_tags_and_meta_survive_a_sibling_scan() {
        let mut conn = test_conn();
        let (files, models, tags) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();

        let other_files = vec![file_row("/other/wyvern/wyvern.stl", "/other/wyvern", 10)];
        let other_models = vec![model_row("/other/wyvern", "Wyvern")];
        let other_tags = vec![("/other/wyvern".to_string(), "dragonkin".to_string())];
        replace_catalog(
            &mut conn,
            "/other",
            &other_files,
            &other_models,
            &other_tags,
            &[],
            &[],
        )
        .unwrap();
        add_tag(&conn, "/other/wyvern", "painted").unwrap();

        // /lib rescanning without tags sheds ITS metadata tag only; /other's
        // metadata tag and the user tag both ride out the sibling scan
        replace_catalog(&mut conn, "/lib", &files, &models, &[], &[], &[]).unwrap();
        let by_tag = |tag: &str| {
            search(&conn, "", &[tag.to_string()], None, None, None, None, 10, 0, true, None, None)
                .map(|page| page.total)
                .unwrap()
        };
        assert_eq!(by_tag("amphibian"), 0);
        assert_eq!(by_tag("dragonkin"), 1);
        assert_eq!(by_tag("painted"), 1);
    }

    #[test]
    fn legacy_unrooted_rows_are_adopted_only_by_their_root() {
        let mut conn = test_conn();
        // A pre-multi-root index: rows exist but no row knows its root. One
        // legacy model sits in "/library" — a string-prefix trap for "/lib".
        let (mut files, mut models, _) = sample_rows();
        files.push(file_row("/library/ghoul/ghoul.stl", "/library/ghoul", 10));
        models.push(model_row("/library/ghoul", "Ghoul"));
        replace_catalog(&mut conn, "/lib", &files, &models, &[], &[], &[]).unwrap();
        conn.execute("UPDATE files SET root = NULL", []).unwrap();
        conn.execute("UPDATE models SET root = NULL", []).unwrap();

        // Scan /lib again with the newt gone: the newt's legacy row must be
        // adopted (and thus dropped), the /library one left alone.
        let bug_files: Vec<_> = files
            .iter()
            .filter(|f| f.dir_path == "/lib/bugbear")
            .cloned()
            .collect();
        let bug_models: Vec<_> = models
            .iter()
            .filter(|m| m.dir_path == "/lib/bugbear")
            .cloned()
            .collect();
        replace_catalog(&mut conn, "/lib", &bug_files, &bug_models, &[], &[], &[]).unwrap();

        assert_eq!(search(&conn, "newt", &[], None, None, None, None, 10, 0, true, None, None).unwrap().total, 0);
        assert_eq!(search(&conn, "ghoul", &[], None, None, None, None, 10, 0, true, None, None).unwrap().total, 1);
        let ghoul_root: Option<String> = conn
            .query_row(
                "SELECT root FROM models WHERE dir_path = '/library/ghoul'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ghoul_root, None, "unclaimed legacy rows stay unclaimed");
    }

    #[test]
    fn trailing_separator_scopes_like_the_bare_root() {
        let mut conn = test_conn();
        let (files, models, _) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &[], &[], &[]).unwrap();

        // Same folder, picker-style trailing slash: still one root, so the
        // newt (absent from this scan) must be replaced away, not duplicated.
        let bug_files: Vec<_> = files
            .iter()
            .filter(|f| f.dir_path == "/lib/bugbear")
            .cloned()
            .collect();
        let bug_models: Vec<_> = models
            .iter()
            .filter(|m| m.dir_path == "/lib/bugbear")
            .cloned()
            .collect();
        replace_catalog(&mut conn, "/lib/", &bug_files, &bug_models, &[], &[], &[]).unwrap();
        assert_eq!(search(&conn, "", &[], None, None, None, None, 10, 0, true, None, None).unwrap().total, 1);
        assert_eq!(search(&conn, "newt", &[], None, None, None, None, 10, 0, true, None, None).unwrap().total, 0);
    }

    #[test]
    fn purge_root_removes_the_slice_and_its_curation() {
        let mut conn = test_conn();
        let (files, models, tags) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();
        add_tag(&conn, "/lib/newt", "painted").unwrap();

        let other_files = vec![file_row("/other/wyvern/wyvern.stl", "/other/wyvern", 10)];
        let other_models = vec![model_row("/other/wyvern", "Wyvern")];
        replace_catalog(&mut conn, "/other", &other_files, &other_models, &[], &[], &[]).unwrap();

        purge_root(&mut conn, "/lib").unwrap();

        assert_eq!(search(&conn, "newt", &[], None, None, None, None, 10, 0, true, None, None).unwrap().total, 0);
        assert_eq!(search(&conn, "wyvern", &[], None, None, None, None, 10, 0, true, None, None).unwrap().total, 1);
        let orphaned_tags: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM model_tags WHERE dir_path = '/lib/newt'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphaned_tags, 0);
        // the per-root stamp goes too, so a re-added folder starts fresh
        let stamps = root_scan_times(&conn).unwrap();
        assert_eq!(stamps.len(), 1);
        assert_eq!(stamps[0].0, "/other");
        // /other's footprint is untouched
        let (m, f, _) = root_summary(&conn, "/other").unwrap();
        assert_eq!((m, f), (1, 1));
    }

    #[test]
    fn per_root_scan_times_are_recorded() {
        let mut conn = test_conn();
        let (files, models, _) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &[], &[], &[]).unwrap();
        replace_catalog(&mut conn, "/other", &[], &[], &[], &[], &[]).unwrap();

        let mut roots: Vec<String> = root_scan_times(&conn)
            .unwrap()
            .into_iter()
            .map(|(root, _)| root)
            .collect();
        roots.sort();
        assert_eq!(roots, vec!["/lib".to_string(), "/other".to_string()]);
    }

    #[test]
    fn hashes_survive_rescan_when_file_unchanged() {
        let mut conn = test_conn();
        let (files, models, tags) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();
        store_hash(&conn, "/lib/newt/GiantNewt_v02.stl", "abc123").unwrap();

        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();
        assert_eq!(
            known_hash(&conn, "/lib/newt/GiantNewt_v02.stl"),
            Some("abc123".to_string())
        );

        // changed mtime invalidates the stored hash
        let mut changed = files.clone();
        changed[0].modified_at = 999;
        replace_catalog(&mut conn, "/lib", &changed, &models, &tags, &[], &[]).unwrap();
        assert_eq!(known_hash(&conn, "/lib/newt/GiantNewt_v02.stl"), None);
    }

    #[test]
    fn stats_and_duplicate_candidates() {
        let mut conn = test_conn();
        let (mut files, models, tags) = sample_rows();
        // make the two files the same size -> duplicate candidates
        files[1].size_bytes = 2048;
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();

        let stats = stats(&conn).unwrap();
        assert_eq!(stats.total_files, 2);
        assert_eq!(stats.total_models, 2);
        assert_eq!(stats.total_size_bytes, 4096.0);

        let candidates = duplicate_size_candidates(&conn).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].1.len(), 2);

        store_hash(&conn, &files[0].path, "same").unwrap();
        store_hash(&conn, &files[1].path, "same").unwrap();
        let groups = duplicate_groups(&conn).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].paths.len(), 2);
    }
}
