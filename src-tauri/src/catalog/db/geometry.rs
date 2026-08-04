use crate::error::AppError;
use rusqlite::{params, Connection};

use crate::catalog::stl_facts::StlFacts;
use crate::catalog::{DuplicateGroup, ModelFileGeometry};

/// Sizes that occur more than once — the free prefilter for duplicate
/// detection.
pub fn duplicate_size_candidates(conn: &Connection) -> Result<Vec<(i64, Vec<String>)>, AppError> {
    let map_err = |e: rusqlite::Error| AppError::ConfigError(format!("Dup query failed: {}", e));
    let mut stmt = conn
        .prepare(
            "SELECT size_bytes FROM files WHERE size_bytes > 0
             GROUP BY size_bytes HAVING COUNT(*) > 1",
        )
        .map_err(map_err)?;
    let sizes: Vec<i64> = stmt
        .query_map([], |row| row.get(0))
        .and_then(|rows| rows.collect())
        .map_err(map_err)?;

    let mut result = Vec::with_capacity(sizes.len());
    let mut path_stmt = conn
        .prepare("SELECT path FROM files WHERE size_bytes = ?1 ORDER BY path")
        .map_err(map_err)?;
    for size in sizes {
        let paths: Vec<String> = path_stmt
            .query_map([size], |row| row.get(0))
            .and_then(|rows| rows.collect())
            .map_err(map_err)?;
        result.push((size, paths));
    }
    Ok(result)
}

pub fn known_hash(conn: &Connection, path: &str) -> Option<String> {
    conn.query_row(
        "SELECT content_hash FROM files WHERE path = ?1",
        [path],
        |row| row.get(0),
    )
    .ok()
    .flatten()
}

pub fn store_hash(conn: &Connection, path: &str, hash: &str) -> Result<(), AppError> {
    conn.execute(
        "UPDATE files SET content_hash = ?2 WHERE path = ?1",
        params![path, hash],
    )
    .map_err(|e| AppError::ConfigError(format!("Failed to store hash: {}", e)))?;
    Ok(())
}

/// Loose (unpacked) STL files a geometry mining pass should consider —
/// archive_path IS NOT NULL rows are excluded because their bytes live
/// inside a model.plinthpack, not loose on disk, so mining has nothing to
/// fs::read (see catalog::geometry's module doc). Each candidate's
/// content_hash rides along so the mining loop can skip a hash it already
/// has (set by an earlier scan/dup-run/pack) straight to the geometry-known
/// check without opening the file.
pub fn stl_geometry_candidates(
    conn: &Connection,
) -> Result<Vec<(String, Option<String>)>, AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Geometry candidate query failed: {}", e));
    let mut stmt = conn
        .prepare("SELECT path, content_hash FROM files WHERE extension = 'stl' AND archive_path IS NULL")
        .map_err(map_err)?;
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        .map_err(map_err)?;
    Ok(rows)
}

/// False (re-mine) only for a row whose open_edges is NULL while its
/// tri_count now fits under `edge_cap` — raising the cap backfills
/// instead of skipping forever.
pub fn geometry_satisfies(
    conn: &Connection,
    content_hash: &str,
    edge_cap: u32,
) -> Result<bool, AppError> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM file_geometry
             WHERE content_hash = ?1 AND (open_edges IS NOT NULL OR tri_count > ?2)",
            params![content_hash, edge_cap],
            |row| row.get(0),
        )
        .map_err(|e| AppError::ConfigError(format!("Geometry lookup failed: {}", e)))?;
    Ok(count > 0)
}

/// Stores bbox extents (max − min per axis), not the raw min/max.
pub fn store_file_geometry(
    conn: &Connection,
    content_hash: &str,
    facts: &StlFacts,
    derived_at: i64,
) -> Result<(), AppError> {
    let x_mm = (facts.max.0 - facts.min.0) as f64;
    let y_mm = (facts.max.1 - facts.min.1) as f64;
    let z_mm = (facts.max.2 - facts.min.2) as f64;
    conn.execute(
        "INSERT INTO file_geometry
             (content_hash, tri_count, x_mm, y_mm, z_mm, volume_mm3, open_edges, derived_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(content_hash) DO UPDATE SET
             tri_count = excluded.tri_count,
             x_mm = excluded.x_mm, y_mm = excluded.y_mm, z_mm = excluded.z_mm,
             volume_mm3 = excluded.volume_mm3,
             open_edges = excluded.open_edges,
             derived_at = excluded.derived_at",
        params![
            content_hash,
            facts.tri_count,
            x_mm,
            y_mm,
            z_mm,
            facts.volume_mm3,
            facts.open_edge_count,
            derived_at
        ],
    )
    .map_err(|e| AppError::ConfigError(format!("Failed to store geometry: {}", e)))?;
    Ok(())
}

/// Inner join on content_hash: un-mined files simply don't appear.
pub fn model_geometry(conn: &Connection, dir_path: &str) -> Result<Vec<ModelFileGeometry>, AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Geometry listing failed: {}", e));
    let mut stmt = conn
        .prepare(
            "SELECT f.file_name, g.tri_count, g.x_mm, g.y_mm, g.z_mm, g.volume_mm3, g.open_edges
             FROM files f JOIN file_geometry g ON g.content_hash = f.content_hash
             WHERE f.dir_path = ?1
             ORDER BY f.file_name COLLATE NOCASE",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map(params![dir_path], |row| {
            Ok(ModelFileGeometry {
                file_name: row.get(0)?,
                tri_count: row.get(1)?,
                x_mm: row.get(2)?,
                y_mm: row.get(3)?,
                z_mm: row.get(4)?,
                volume_mm3: row.get(5)?,
                open_edges: row.get(6)?,
            })
        })
        .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        .map_err(map_err)?;
    Ok(rows)
}

/// Batch-write physical-file identities in one transaction — a duplicate
/// scan refreshes every candidate, and per-row autocommits would turn
/// thousands of cheap stats into thousands of fsyncs.
pub fn store_identities(conn: &Connection, entries: &[(String, String)]) -> Result<(), AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Failed to store identities: {}", e));
    let tx = conn.unchecked_transaction().map_err(map_err)?;
    {
        let mut stmt = tx
            .prepare("UPDATE files SET file_identity = ?2 WHERE path = ?1")
            .map_err(map_err)?;
        for (path, identity) in entries {
            stmt.execute(params![path, identity]).map_err(map_err)?;
        }
    }
    tx.commit().map_err(map_err)?;
    Ok(())
}

/// Post-merge bookkeeping: identity AND modified_at, in one transaction.
/// The mtime matters because replacing a duplicate with a hardlink gives the
/// path the keeper's timestamp — if the index kept the old one, the next
/// rescan's changed-file check would fail and silently drop the stored hash
/// and identity, making the merged group vanish and reappear across scans.
pub fn store_merge_results(
    conn: &Connection,
    entries: &[(String, String, i64)],
) -> Result<(), AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Failed to record merge: {}", e));
    let tx = conn.unchecked_transaction().map_err(map_err)?;
    {
        let mut stmt = tx
            .prepare("UPDATE files SET file_identity = ?2, modified_at = ?3 WHERE path = ?1")
            .map_err(map_err)?;
        for (path, identity, modified_at) in entries {
            stmt.execute(params![path, identity, modified_at])
                .map_err(map_err)?;
        }
    }
    tx.commit().map_err(map_err)?;
    Ok(())
}

/// Assemble confirmed duplicate groups from stored hashes. Paths that are
/// hardlinks of each other share a file_identity and cost the disk only one
/// copy, so reclaimable space is driven by DISTINCT identities, not path
/// count. A missing identity falls back to the path — i.e. it's assumed to
/// be its own copy — so unscanned rows never hide reclaimable bytes.
pub fn duplicate_groups(conn: &Connection) -> Result<Vec<DuplicateGroup>, AppError> {
    let map_err = |e: rusqlite::Error| AppError::ConfigError(format!("Dup grouping failed: {}", e));
    let mut stmt = conn
        .prepare(
            // group_concat skips NULLs, so the CASE gives just the packed
            // subset (NULL when none)
            "SELECT content_hash, size_bytes, group_concat(path, char(31)),
                    COUNT(DISTINCT COALESCE(file_identity, path)),
                    group_concat(CASE WHEN archive_path IS NOT NULL THEN path END, char(31))
             FROM files
             WHERE content_hash IS NOT NULL
             GROUP BY content_hash HAVING COUNT(*) > 1
             ORDER BY size_bytes * (COUNT(DISTINCT COALESCE(file_identity, path)) - 1) DESC,
                      size_bytes DESC",
        )
        .map_err(map_err)?;
    let groups = stmt
        .query_map([], |row| {
            let joined: String = row.get(2)?;
            let packed_joined: Option<String> = row.get(4)?;
            Ok(DuplicateGroup {
                hash: row.get(0)?,
                size_bytes: row.get::<_, i64>(1)? as f64,
                paths: joined.split('\u{1f}').map(String::from).collect(),
                distinct_copies: row.get(3)?,
                packed_paths: packed_joined
                    .map(|p| p.split('\u{1f}').map(String::from).collect())
                    .unwrap_or_default(),
            })
        })
        .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        .map_err(map_err)?;
    Ok(groups)
}
