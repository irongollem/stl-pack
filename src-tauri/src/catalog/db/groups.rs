use crate::error::AppError;
use rusqlite::{params, Connection};
use std::path::Path;

use crate::catalog::{CatalogEntry, CatalogFile, FileVariant, GroupOrigin};

use super::ingest::{rebuild_fts, refresh_fts_row};
use super::meta::{require_model, set_model_preview, update_model_facets};
use super::search::{entry_select_sql, map_entry_row, NSFW_EFFECTIVE_SQL};

/// Separator inside a variant_key. The unit separator can't occur in a path,
/// so a key never collides with a real directory. Format is
/// `dir\u{1f}variant\u{1f}pose`; empty variant AND pose = the residual pool.
const VARIANT_SEP: char = '\u{1f}';

/// Build a member's variant_key. Empty facet strings encode "no variant"/
/// "no pose"; both empty is the residual/unassigned member.
fn variant_key(dir_path: &str, variant: &str, pose: &str) -> String {
    format!("{dir_path}{VARIANT_SEP}{variant}{VARIANT_SEP}{pose}")
}

/// Recover (variant, pose) from a variant_key. dir_path is the authority for
/// which folder, so the leading segment is ignored; the last two fields are
/// the facets (either may be "" for unset).
fn parse_variant_key(key: &str) -> (&str, &str) {
    let mut fields = key.rsplit(VARIANT_SEP);
    let pose = fields.next().unwrap_or("");
    let variant = fields.next().unwrap_or("");
    (variant, pose)
}

/// path -> size for a dir's indexed files (model files only; images aren't
/// indexed). Used to recompute per-pose counts and sizes after a split.
fn file_sizes_for_dir(
    conn: &Connection,
    dir_path: &str,
) -> Result<std::collections::HashMap<String, i64>, AppError> {
    let map = |e: rusqlite::Error| AppError::ConfigError(format!("File size lookup failed: {}", e));
    let mut stmt = conn
        .prepare("SELECT path, size_bytes FROM files WHERE dir_path = ?1")
        .map_err(map)?;
    let rows = stmt
        .query_map([dir_path], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(map)?;
    rows.collect::<Result<_, _>>().map_err(map)
}

/// Fan a folder that carries file→pose assignments into one member per
/// assigned pose, plus a residual member for any still-unassigned model
/// files. Counts and sizes are recomputed per bucket from the files table.
/// Folders with no assignments pass through untouched, so nothing regresses
/// for the folder-per-model libraries. Ordered supported-before-unsupported
/// then by pose, matching the whole-folder member ordering.
fn expand_file_variants(
    conn: &Connection,
    entries: Vec<CatalogEntry>,
) -> Result<Vec<CatalogEntry>, AppError> {
    use std::collections::{BTreeMap, HashSet};
    let mut out = Vec::new();
    for entry in entries {
        let assigned: Vec<FileVariant> = get_file_variants(conn, &entry.dir_path)?
            .into_iter()
            .filter(|v| v.pose.as_deref().is_some_and(|p| !p.is_empty()))
            .collect();
        if assigned.is_empty() {
            out.push(entry);
            continue;
        }
        let sizes = file_sizes_for_dir(conn, &entry.dir_path)?;
        // Per-variant preview overrides for this folder, keyed by variant_key.
        // A member with its own render beats the folder-level preview it would
        // otherwise inherit from `entry` below.
        let previews = get_variant_previews(conn, &entry.dir_path)?;
        // (support, variant, pose) -> file paths; BTreeMap for a stable order
        let mut buckets: BTreeMap<(Option<String>, String, String), Vec<String>> = BTreeMap::new();
        let mut claimed: HashSet<String> = HashSet::new();
        for v in assigned {
            // A pose-only assignment inherits the FOLDER's variant: the
            // canonical leaf .../Supported/Great Swords fans into pose
            // members that must stay inside the Great Swords tab — using
            // only the file-level value collapsed every pose member into
            // a variantless pool and the variant tier vanished. A file
            // value that only differs from the folder's by CASE adopts the
            // folder's spelling — legacy rows predate the Title Case
            // convention and must not fork a second bucket.
            let variant = v
                .variant
                .filter(|s| !s.is_empty())
                .map(|s| {
                    match entry.variant.as_deref() {
                        Some(ev) if ev.eq_ignore_ascii_case(&s) => ev.to_string(),
                        _ => s,
                    }
                })
                .or_else(|| entry.variant.clone())
                .unwrap_or_default();
            let pose = v.pose.unwrap_or_default();
            claimed.insert(v.path.clone());
            buckets
                .entry((v.support_status, variant, pose))
                .or_default()
                .push(v.path);
        }
        for ((support, variant, pose), paths) in buckets {
            let bytes: i64 = paths.iter().filter_map(|p| sizes.get(p)).sum();
            // label reads "mob sword 2" — base plus whichever facets are
            // set, skipping a variant the whole folder already carries
            // (every pose member repeating it would just be noise)
            let mut label = entry.name.clone();
            for facet in [&variant, &pose] {
                let repeats_folder = entry
                    .variant
                    .as_deref()
                    .is_some_and(|ev| ev.eq_ignore_ascii_case(facet));
                if !facet.is_empty() && !repeats_folder {
                    label.push(' ');
                    label.push_str(facet);
                }
            }
            let key = variant_key(&entry.dir_path, &variant, &pose);
            let preview_path = previews
                .get(&key)
                .cloned()
                .or_else(|| entry.preview_path.clone());
            out.push(CatalogEntry {
                name: label,
                variant: (!variant.is_empty()).then(|| variant.clone()),
                pose: (!pose.is_empty()).then(|| pose.clone()),
                support_status: support.or_else(|| entry.support_status.clone()),
                file_count: paths.len() as u32,
                total_size_bytes: bytes as f64,
                preview_path,
                variant_key: Some(key),
                ..entry.clone()
            });
        }
        // Whatever the user hasn't sorted yet stays visible as a residual
        // member so no file silently vanishes from the folder.
        let residual: Vec<&String> = sizes.keys().filter(|p| !claimed.contains(*p)).collect();
        if !residual.is_empty() {
            let bytes: i64 = residual.iter().filter_map(|p| sizes.get(*p)).sum();
            let key = variant_key(&entry.dir_path, "", "");
            let preview_path = previews
                .get(&key)
                .cloned()
                .or_else(|| entry.preview_path.clone());
            out.push(CatalogEntry {
                name: format!("{} (unassigned)", entry.name),
                // keep the folder's variant — the leftovers still live in
                // that variant's folder, only their pose is unknown
                variant: entry.variant.clone(),
                pose: None,
                file_count: residual.len() as u32,
                total_size_bytes: bytes as f64,
                preview_path,
                variant_key: Some(key),
                ..entry
            });
        }
    }
    Ok(out)
}

/// All variants of one logical model, ordered for the drawer: support
/// status first (alphabetical puts supported before unsupported, unknowns
/// last), then pose. `include_nsfw = false` drops any member individually
/// flagged 18+ (see NSFW_EFFECTIVE_SQL) — data ops (pack/render/move/delete
/// scope resolution) must pass true so a hidden member is never silently
/// skipped by an action the user actually asked for.
pub fn group_members(
    conn: &Connection,
    group_name: &str,
    include_nsfw: bool,
) -> Result<Vec<CatalogEntry>, AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Group member query failed: {}", e));
    let where_sql = if include_nsfw {
        "LEFT JOIN group_renames r ON r.source_group = COALESCE(m.group_name, m.name)
         WHERE lower(COALESCE(r.display_name, m.group_name, m.name)) = lower(?)"
            .to_string()
    } else {
        format!(
            "LEFT JOIN group_renames r ON r.source_group = COALESCE(m.group_name, m.name)
             WHERE lower(COALESCE(r.display_name, m.group_name, m.name)) = lower(?)
               AND {} = 0",
            NSFW_EFFECTIVE_SQL
        )
    };
    let sql = entry_select_sql(
        "",
        &where_sql,
        "ORDER BY NULLIF(COALESCE(u.support_status, m.support_status), '') IS NULL,
                  NULLIF(COALESCE(u.support_status, m.support_status), ''),
                  NULLIF(COALESCE(u.pose, m.pose), '') IS NULL,
                  NULLIF(COALESCE(u.pose, m.pose), ''),
                  m.dir_path",
    );
    let mut stmt = conn.prepare(&sql).map_err(map_err)?;
    let entries = stmt
        .query_map([group_name], map_entry_row)
        .map_err(map_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_err)?;
    // A curated dump folder becomes several pose members here; untouched
    // folders pass straight through.
    expand_file_variants(conn, entries)
}

/// Map every scanner-level source group currently SHOWN as `group_name`
/// to display as `new_name`. Returns how many mappings were written.
fn upsert_group_rename(
    conn: &Connection,
    group_name: &str,
    new_name: &str,
) -> Result<usize, rusqlite::Error> {
    conn.execute(
        "INSERT INTO group_renames (source_group, display_name)
         SELECT DISTINCT COALESCE(m.group_name, m.name), ?2
         FROM models m
         LEFT JOIN group_renames r
             ON r.source_group = COALESCE(m.group_name, m.name)
         WHERE lower(COALESCE(r.display_name, m.group_name, m.name)) = lower(?1)
         ON CONFLICT(source_group) DO UPDATE SET display_name = excluded.display_name",
        params![group_name, new_name],
    )
}

/// Rename the group shown as `group_name` to `new_name` — stored against
/// the scanner-level source groups so it survives rescans. An empty
/// new_name clears the override(s), reverting to the folder-derived name.
/// Renaming a group to another group's name merges the two.
pub fn rename_group(conn: &Connection, group_name: &str, new_name: &str) -> Result<(), AppError> {
    let map_err = |e: rusqlite::Error| AppError::ConfigError(format!("Group rename failed: {}", e));
    let new_name = new_name.trim();
    if new_name.is_empty() {
        conn.execute(
            "DELETE FROM group_renames
             WHERE lower(display_name) = lower(?1) OR lower(source_group) = lower(?1)",
            [group_name],
        )
        .map_err(map_err)?;
    } else {
        let changed = upsert_group_rename(conn, group_name, new_name).map_err(map_err)?;
        if changed == 0 {
            return Err(AppError::NotFoundError(format!(
                "No catalog group named '{}'",
                group_name
            )));
        }
    }
    // renamed groups must be findable by their new name
    rebuild_fts(conn).map_err(map_err)?;
    Ok(())
}

/// The scanner-level source groups currently shown under one display name —
/// more than one means the card is a combination (renamed-together groups),
/// which is what makes it splittable: clearing the renames (rename_group
/// with an empty name) restores exactly these names as separate cards.
pub fn group_sources(conn: &Connection, group_name: &str) -> Result<Vec<String>, AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Group source lookup failed: {}", e));
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT COALESCE(m.group_name, m.name) AS src
             FROM models m
             LEFT JOIN group_renames r ON r.source_group = COALESCE(m.group_name, m.name)
             WHERE lower(COALESCE(r.display_name, m.group_name, m.name)) = lower(?1)
             ORDER BY src COLLATE NOCASE",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map([group_name], |row| row.get(0))
        .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        .map_err(map_err)?;
    Ok(rows)
}

/// (designer, release_name) origins among the models a rename/combine of
/// `group_name` would touch — the SAME predicate upsert_group_rename uses,
/// so this predicts exactly what a rename reaches. group_renames has no
/// root/designer scoping (see the group_renames CREATE TABLE comment), so
/// a generic scanner-derived name ("Spear") reused by an unrelated designer
/// collides here; more than one distinct origin is the signal a caller
/// should confirm with the user before committing the rename.
pub fn group_rename_origins(
    conn: &Connection,
    group_name: &str,
) -> Result<Vec<GroupOrigin>, AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Group origin lookup failed: {}", e));
    let mut stmt = conn
        .prepare(
            "SELECT m.designer, m.release_name, COUNT(*) AS model_count
             FROM models m
             LEFT JOIN group_renames r ON r.source_group = COALESCE(m.group_name, m.name)
             WHERE lower(COALESCE(r.display_name, m.group_name, m.name)) = lower(?1)
             GROUP BY m.designer, m.release_name
             ORDER BY model_count DESC",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map([group_name], |row| {
            Ok(GroupOrigin {
                designer: row.get(0)?,
                release_name: row.get(1)?,
                model_count: row.get(2)?,
            })
        })
        .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        .map_err(map_err)?;
    Ok(rows)
}

/// Undo ONE source's membership in a combined card — the fix for "I checked
/// one card too many when combining". Deletes just that source's rename row,
/// so it reappears as its own card under its folder-derived name; the rest
/// of the combination is untouched. Errors when the source sits in the card
/// by its own folder name (nothing to detach — that's a folder rename/move).
pub fn detach_group_source(
    conn: &Connection,
    group_name: &str,
    source_group: &str,
) -> Result<(), AppError> {
    let map_err = |e: rusqlite::Error| AppError::ConfigError(format!("Detach failed: {}", e));
    let removed = conn
        .execute(
            "DELETE FROM group_renames
             WHERE lower(source_group) = lower(?2) AND lower(display_name) = lower(?1)",
            params![group_name, source_group],
        )
        .map_err(map_err)?;
    if removed == 0 {
        return Err(AppError::InvalidInput(format!(
            "\"{}\" isn't combined into \"{}\" — it groups there under its own folder name, so rename or move the folder instead",
            source_group, group_name
        )));
    }
    rebuild_fts(conn).map_err(map_err)?;
    Ok(())
}

/// Remember which member fronts a group's card. Stored as the member's
/// identity (dir_path + optional variant_key), resolved to its CURRENT
/// preview at read time — a re-render updates the card automatically.
pub fn set_group_cover(
    conn: &Connection,
    group_name: &str,
    dir_path: &str,
    variant_key: Option<&str>,
) -> Result<(), AppError> {
    require_model(conn, dir_path)?;
    conn.execute(
        "INSERT INTO group_covers (group_name, dir_path, variant_key)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(group_name) DO UPDATE SET
             dir_path = excluded.dir_path,
             variant_key = excluded.variant_key",
        params![group_name, dir_path, variant_key],
    )
    .map_err(|e| AppError::ConfigError(format!("Failed to set card image: {}", e)))?;
    Ok(())
}

/// The chosen cover member's current preview, if a cover is set and its
/// member still has one.
pub(super) fn cover_preview(conn: &Connection, group_name: &str) -> Option<String> {
    conn.query_row(
        "SELECT COALESCE(vp.preview_path, u.preview_path, m.preview_path)
         FROM group_covers gc
         LEFT JOIN variant_previews vp ON vp.variant_key = gc.variant_key
         LEFT JOIN model_user_meta u ON u.dir_path = gc.dir_path
         LEFT JOIN models m ON m.dir_path = gc.dir_path
         WHERE gc.group_name = ?1",
        [group_name],
        |row| row.get(0),
    )
    .ok()
    .flatten()
}

/// The explicit merge tool: map every listed group onto one display name.
/// This is rename_group's merge behavior made first-class — folder
/// inference only groups what a creator's structure happens to encode,
/// and every creator structures differently, so combining must never
/// DEPEND on inference. One transaction, one FTS rebuild.
pub fn combine_groups(
    conn: &mut Connection,
    group_names: &[String],
    target_name: &str,
) -> Result<(), AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Group combine failed: {}", e));
    let target_name = target_name.trim();
    if target_name.is_empty() {
        return Err(AppError::InvalidInput(
            "A combined model needs a name".to_string(),
        ));
    }
    let tx = conn.transaction().map_err(map_err)?;
    let mut changed = 0;
    for group_name in group_names {
        changed += upsert_group_rename(&tx, group_name, target_name).map_err(map_err)?;
    }
    if changed == 0 {
        return Err(AppError::NotFoundError(
            "None of the selected groups exist anymore".to_string(),
        ));
    }
    rebuild_fts(&tx).map_err(map_err)?;
    tx.commit().map_err(map_err)?;
    Ok(())
}

/// The dir_paths shown under one card — the same display-name resolution as
/// group_members, for operations that apply to the whole logical model.
fn group_member_dirs(conn: &Connection, group_name: &str) -> Result<Vec<String>, AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Group member lookup failed: {}", e));
    let mut stmt = conn
        .prepare(
            "SELECT m.dir_path FROM models m
             LEFT JOIN group_renames r ON r.source_group = COALESCE(m.group_name, m.name)
             WHERE lower(COALESCE(r.display_name, m.group_name, m.name)) = lower(?1)",
        )
        .map_err(map_err)?;
    stmt.query_map([group_name], |row| row.get(0))
        .and_then(|rows| rows.collect())
        .map_err(map_err)
}

/// Tag every member of a group. A tag describes the mini, not one build of
/// it — tagging the supported and unsupported variants separately was just
/// busywork that drifted out of sync.
pub fn add_group_tag(conn: &Connection, group_name: &str, tag: &str) -> Result<(), AppError> {
    let dirs = group_member_dirs(conn, group_name)?;
    if dirs.is_empty() {
        return Err(AppError::NotFoundError(format!(
            "No catalog group named '{}'",
            group_name
        )));
    }
    for dir in &dirs {
        add_tag(conn, dir, tag)?;
    }
    Ok(())
}

pub fn remove_group_tag(conn: &Connection, group_name: &str, tag: &str) -> Result<(), AppError> {
    for dir in group_member_dirs(conn, group_name)? {
        remove_tag(conn, &dir, tag)?;
    }
    Ok(())
}

/// Collapse a whole card back to one undifferentiated pile: the scanner's
/// auto-split guessed variant/pose wrong and the user wants to re-file by
/// hand. Two clears, both surviving rescans. First, every member's variant
/// AND pose is tombstoned with '' — that beats the scanner's inference on the
/// next read (see update_model_facets and the NULLIF/COALESCE read path), so
/// the variant/pose tier chips disappear. Second, every per-file pose
/// assignment under those dirs is dropped, so any fanned-out dump folder folds
/// back into its single residual member. Nothing moves on disk — the files
/// stay put, ready for the assignment bar. Returns how many file assignments
/// were dropped (for the toast).
pub fn flatten_group(conn: &Connection, group_name: &str) -> Result<u32, AppError> {
    let dirs = group_member_dirs(conn, group_name)?;
    if dirs.is_empty() {
        return Err(AppError::NotFoundError(format!(
            "No catalog group named '{}'",
            group_name
        )));
    }
    let mut cleared = 0u32;
    for dir in &dirs {
        // Some("") is the tombstone; scale is left untouched with None.
        update_model_facets(conn, dir, Some(""), Some(""), None)?;
        cleared += conn
            .execute("DELETE FROM file_variants WHERE dir_path = ?1", params![dir])
            .map_err(|e| AppError::ConfigError(format!("Failed to clear assignments: {}", e)))?
            as u32;
    }
    Ok(cleared)
}

/// The supported/unsupported (and format-variant) builds of the same sculpt:
/// model dirs in the same group whose paths are identical once
/// support-status segments are ignored. Exact structural match only — no
/// fuzzy pairing — so an edit can never propagate to the wrong model.
pub fn support_twins(conn: &Connection, dir_path: &str) -> Result<Vec<String>, AppError> {
    let map_err = |e: rusqlite::Error| AppError::ConfigError(format!("Twin lookup failed: {}", e));
    let support_neutral_key = |path: &str| -> String {
        Path::new(path)
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .filter(|seg| crate::catalog::scanner::support_from_segment(seg).is_none())
            .collect::<Vec<_>>()
            .join("\u{1f}")
            .to_lowercase()
    };
    let own_key = support_neutral_key(dir_path);
    let mut stmt = conn
        .prepare(
            "SELECT m2.dir_path FROM models m2
             WHERE lower(COALESCE(m2.group_name, m2.name)) =
                   (SELECT lower(COALESCE(group_name, name)) FROM models WHERE dir_path = ?1)
               AND m2.dir_path <> ?1",
        )
        .map_err(map_err)?;
    let candidates: Vec<String> = stmt
        .query_map([dir_path], |row| row.get(0))
        .and_then(|rows| rows.collect())
        .map_err(map_err)?;
    Ok(candidates
        .into_iter()
        .filter(|c| support_neutral_key(c) == own_key)
        .collect())
}

pub fn add_tag(conn: &Connection, dir_path: &str, tag: &str) -> Result<(), AppError> {
    let tag = tag.trim().to_lowercase().replace(' ', "_");
    if tag.is_empty() {
        return Err(AppError::InvalidInput("Empty tag".to_string()));
    }
    conn.execute(
        "INSERT OR IGNORE INTO model_tags (dir_path, tag, source) VALUES (?1, ?2, 'user')",
        params![dir_path, tag],
    )
    .map_err(|e| AppError::ConfigError(format!("Failed to add tag: {}", e)))?;
    refresh_fts_row(conn, dir_path)
        .map_err(|e| AppError::ConfigError(format!("Failed to update search index: {}", e)))?;
    Ok(())
}

pub fn remove_tag(conn: &Connection, dir_path: &str, tag: &str) -> Result<(), AppError> {
    // Metadata tags reappear on the next scan by design — the metadata
    // file is their source of truth
    conn.execute(
        "DELETE FROM model_tags WHERE dir_path = ?1 AND tag = ?2",
        params![dir_path, tag],
    )
    .map_err(|e| AppError::ConfigError(format!("Failed to remove tag: {}", e)))?;
    refresh_fts_row(conn, dir_path)
        .map_err(|e| AppError::ConfigError(format!("Failed to update search index: {}", e)))?;
    Ok(())
}

/// Files for a member. `variant_key` (from a synthesized pose member)
/// narrows to just that pose's assigned files; `...{sep}` with an empty
/// pose returns the residual unassigned files; None returns every file in
/// the folder (the whole-folder member, and every non-split model).
pub fn model_files(
    conn: &Connection,
    dir_path: &str,
    variant_key: Option<&str>,
) -> Result<Vec<CatalogFile>, AppError> {
    let map = |e: rusqlite::Error| AppError::ConfigError(format!("File listing failed: {}", e));
    // The key's own dir prefix is ignored — dir_path is the authority — so a
    // stale key can never pull files from another folder.
    let facets = variant_key.map(parse_variant_key);
    let read = |row: &rusqlite::Row| {
        Ok(CatalogFile {
            path: row.get(0)?,
            file_name: row.get(1)?,
            extension: row.get(2)?,
            size_bytes: row.get::<_, i64>(3)? as f64,
            packed: row.get(4)?,
        })
    };
    let select = "SELECT f.path, f.file_name, f.extension, f.size_bytes,
                         f.archive_path IS NOT NULL FROM files f WHERE ";
    let order = " ORDER BY f.file_name COLLATE NOCASE";
    let rows = match facets {
        // whole-folder member: every file
        None => {
            let sql = format!("{select}f.dir_path = ?1{order}");
            conn.prepare(&sql)
                .and_then(|mut s| s.query_map(params![dir_path], read)?.collect())
        }
        // residual pool: files with no (variant/pose) assignment
        Some(("", "")) => {
            let sql = format!(
                "{select}f.dir_path = ?1 AND f.path NOT IN
                     (SELECT path FROM file_variants WHERE dir_path = ?1
                      AND (COALESCE(variant,'') <> '' OR COALESCE(pose,'') <> '')){order}"
            );
            conn.prepare(&sql)
                .and_then(|mut s| s.query_map(params![dir_path], read)?.collect())
        }
        // a specific (variant, pose) bucket. Mirrors expand_file_variants'
        // inheritance rule: a pose-only assignment (empty file-level
        // variant) belongs to the FOLDER's variant bucket — matching only
        // the exact value made every inherited-variant pose member list
        // zero files.
        Some((variant, pose)) => {
            let folder_variant: String = conn
                .query_row(
                    "SELECT COALESCE(u.variant, m.variant, '') FROM models m
                     LEFT JOIN model_user_meta u ON u.dir_path = m.dir_path
                     WHERE m.dir_path = ?1",
                    [dir_path],
                    |row| row.get(0),
                )
                .unwrap_or_default();
            let sql = format!(
                "{select}f.path IN (SELECT path FROM file_variants
                     WHERE dir_path = ?1
                       AND (COALESCE(variant,'') = ?2
                            OR (COALESCE(variant,'') = '' AND ?4 = ?2))
                       AND COALESCE(pose,'') = ?3){order}"
            );
            conn.prepare(&sql).and_then(|mut s| {
                s.query_map(params![dir_path, variant, pose, folder_variant], read)?
                    .collect()
            })
        }
    }
    .map_err(map)?;
    Ok(rows)
}

/// Display-group names under a render scope — same designer/selection
/// filters as pack_candidate_dirs, but returning the GROUP names because
/// render candidates are enumerated through group_members (which resolves
/// per-variant previews; a raw preview_path IS NULL over models would miss
/// fanned members).
pub fn render_scope_groups(
    conn: &Connection,
    designer: Option<&str>,
    groups: &[String],
) -> Result<Vec<String>, AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Render scope query failed: {}", e));
    let mut sql = String::from(
        "SELECT DISTINCT COALESCE(r.display_name, m.group_name, m.name) AS gname
         FROM models m
         LEFT JOIN model_user_meta u ON u.dir_path = m.dir_path
         LEFT JOIN group_renames r ON r.source_group = COALESCE(m.group_name, m.name)
         WHERE 1=1",
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
    sql.push_str(" ORDER BY gname COLLATE NOCASE");
    let params_ref: Vec<&dyn rusqlite::types::ToSql> = bound.iter().map(|b| b.as_ref()).collect();
    let mut stmt = conn.prepare(&sql).map_err(map_err)?;
    let rows = stmt
        .query_map(params_ref.as_slice(), |row| row.get(0))
        .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        .map_err(map_err)?;
    Ok(rows)
}

/// Point a single fanned-out member (one pose/variant of a dump folder) at a
/// preview, keyed by its full variant_key so sibling poses in the same folder
/// keep their own pictures. dir_path (the owning folder) rides along so a
/// rescan can prune previews for folders that no longer exist.
pub fn set_variant_preview(
    conn: &Connection,
    dir_path: &str,
    variant_key: &str,
    preview_path: &str,
) -> Result<(), AppError> {
    require_model(conn, dir_path)?;
    conn.execute(
        "INSERT INTO variant_previews (variant_key, dir_path, preview_path)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(variant_key) DO UPDATE SET
             preview_path = excluded.preview_path,
             dir_path = excluded.dir_path",
        params![variant_key, dir_path, preview_path],
    )
    .map_err(|e| AppError::ConfigError(format!("Failed to set variant preview: {}", e)))?;
    Ok(())
}

/// Route a preview to the right store: a fanned-out member (variant_key set)
/// gets a per-variant preview so poses in one folder don't clobber each other;
/// a whole-folder member falls back to model_user_meta.
pub fn set_preview(
    conn: &Connection,
    dir_path: &str,
    variant_key: Option<&str>,
    preview_path: &str,
) -> Result<(), AppError> {
    match variant_key {
        Some(key) => set_variant_preview(conn, dir_path, key, preview_path),
        None => set_model_preview(conn, dir_path, preview_path),
    }
}

/// variant_key -> preview_path for every per-variant preview under one folder.
/// Consulted by expand_file_variants to override the folder-level preview each
/// synthesized member would otherwise inherit.
fn get_variant_previews(
    conn: &Connection,
    dir_path: &str,
) -> Result<std::collections::HashMap<String, String>, AppError> {
    let map = |e: rusqlite::Error| {
        AppError::ConfigError(format!("Failed to read variant previews: {}", e))
    };
    let mut stmt = conn
        .prepare("SELECT variant_key, preview_path FROM variant_previews WHERE dir_path = ?1")
        .map_err(map)?;
    let rows = stmt
        .query_map([dir_path], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(map)?;
    rows.collect::<Result<_, _>>().map_err(map)
}

/// Assign a set of files to a pose bucket (and optional per-file support),
/// so a single dump folder can be split into pose members without touching
/// disk. dir_path is pulled from the files table, so unknown paths are
/// silently skipped rather than orphaning a row. A None pose clears the
/// pose while keeping the row — pass it through clear_file_variants to drop
/// the assignment entirely. Returns how many known files were assigned.
pub fn set_file_variants(
    conn: &mut Connection,
    paths: &[String],
    variant: Option<String>,
    pose: Option<String>,
    support_status: Option<String>,
) -> Result<u32, AppError> {
    let map = |e: rusqlite::Error| AppError::ConfigError(format!("Failed to assign files: {}", e));
    let tx = conn.transaction().map_err(map)?;
    let mut assigned = 0u32;
    {
        let mut stmt = tx
            .prepare(
                "INSERT INTO file_variants (path, dir_path, variant, pose, support_status)
                 SELECT ?1, dir_path, ?2, ?3, ?4 FROM files WHERE path = ?1
                 ON CONFLICT(path) DO UPDATE SET
                     variant = excluded.variant,
                     pose = excluded.pose,
                     support_status = excluded.support_status",
            )
            .map_err(map)?;
        for path in paths {
            assigned += stmt
                .execute(params![path, variant, pose, support_status])
                .map_err(map)? as u32;
        }
    }
    tx.commit().map_err(map)?;
    Ok(assigned)
}

/// Drop pose assignments for the given files, reverting them to plain
/// members of their folder.
/// Returns how many assignments actually existed — files that were never
/// filed clear nothing, and the UI should say so instead of claiming success.
pub fn clear_file_variants(conn: &Connection, paths: &[String]) -> Result<u32, AppError> {
    let mut cleared = 0u32;
    for path in paths {
        cleared += conn
            .execute("DELETE FROM file_variants WHERE path = ?1", params![path])
            .map_err(|e| AppError::ConfigError(format!("Failed to clear assignment: {}", e)))?
            as u32;
    }
    Ok(cleared)
}

/// Every file-pose assignment under one model folder, for the split UI.
pub fn get_file_variants(conn: &Connection, dir_path: &str) -> Result<Vec<FileVariant>, AppError> {
    let mut stmt = conn
        .prepare(
            "SELECT path, dir_path, variant, pose, support_status
             FROM file_variants WHERE dir_path = ?1 ORDER BY path",
        )
        .map_err(|e| AppError::ConfigError(format!("Failed to read assignments: {}", e)))?;
    let rows = stmt
        .query_map(params![dir_path], |row| {
            Ok(FileVariant {
                path: row.get(0)?,
                dir_path: row.get(1)?,
                variant: row.get(2)?,
                pose: row.get(3)?,
                support_status: row.get(4)?,
            })
        })
        .map_err(|e| AppError::ConfigError(format!("Failed to read assignments: {}", e)))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| AppError::ConfigError(format!("Failed to read assignments: {}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::db::*;
    use crate::catalog::db::test_util::*;
    use crate::catalog::{FileVariantRow, ModelRow};

    #[test]
    fn imported_file_poses_seed_but_never_clobber_a_user_split() {
        let path = "/lib/newt/GiantNewt_v02.stl";
        let seed = |variant: &str, pose: &str| {
            vec![FileVariantRow {
                path: path.into(),
                variant: Some(variant.into()),
                pose: Some(pose.into()),
                support_status: None,
            }]
        };
        let (files, models, tags) = sample_rows();

        // fresh catalog: the model.json split is imported
        let mut conn = test_conn();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &seed("sword", "1"), &[]).unwrap();
        let fv = get_file_variants(&conn, "/lib/newt").unwrap();
        assert_eq!(fv.len(), 1);
        assert_eq!(fv[0].variant.as_deref(), Some("sword"));
        assert_eq!(fv[0].pose.as_deref(), Some("1"));

        // but once the user has their own split, a rescan importing a
        // different one leaves theirs untouched (INSERT OR IGNORE on path)
        set_file_variants(&mut conn, &[path.into()], None, Some("Z".into()), None).unwrap();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &seed("bow", "9"), &[]).unwrap();
        let fv = get_file_variants(&conn, "/lib/newt").unwrap();
        assert_eq!(fv.len(), 1);
        assert_eq!(fv[0].pose.as_deref(), Some("Z"), "the user's split wins");
        assert!(fv[0].variant.is_none());
    }

    #[test]
    fn file_variants_round_trip_survive_rescan_and_prune() {
        let mut conn = test_conn();
        let (files, models, tags) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();

        // assign the newt's file to pose B; an unknown path is silently
        // skipped (no file row to hang dir_path off of)
        let assigned = set_file_variants(
            &mut conn,
            &[
                "/lib/newt/GiantNewt_v02.stl".into(),
                "/lib/newt/does-not-exist.stl".into(),
            ],
            Some("sword".into()),
            Some("B".into()),
            Some("unsupported".into()),
        )
        .unwrap();
        assert_eq!(assigned, 1, "only the known file is assigned");

        let variants = get_file_variants(&conn, "/lib/newt").unwrap();
        assert_eq!(variants.len(), 1);
        assert_eq!(variants[0].variant.as_deref(), Some("sword"));
        assert_eq!(variants[0].pose.as_deref(), Some("B"));
        assert_eq!(variants[0].support_status.as_deref(), Some("unsupported"));
        assert_eq!(
            variants[0].dir_path, "/lib/newt",
            "dir_path denormalized from files"
        );

        // reassigning updates in place rather than duplicating
        set_file_variants(
            &mut conn,
            &["/lib/newt/GiantNewt_v02.stl".into()],
            None,
            Some("C".into()),
            None,
        )
        .unwrap();
        let variants = get_file_variants(&conn, "/lib/newt").unwrap();
        assert_eq!(variants.len(), 1);
        assert_eq!(variants[0].pose.as_deref(), Some("C"));
        assert!(variants[0].variant.is_none(), "variant cleared on reassign");

        // a rescan that still lists the file keeps the assignment
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();
        assert_eq!(get_file_variants(&conn, "/lib/newt").unwrap().len(), 1);

        // but a rescan where the file is gone from disk prunes it
        let pruned_files = vec![files[1].clone()];
        let pruned_models = vec![models[1].clone()];
        replace_catalog(&mut conn, "/lib", &pruned_files, &pruned_models, &[], &[], &[]).unwrap();
        assert!(get_file_variants(&conn, "/lib/newt").unwrap().is_empty());

        // and clearing drops the assignment explicitly
        set_file_variants(
            &mut conn,
            &[files[1].path.clone()],
            None,
            Some("A".into()),
            None,
        )
        .unwrap();
        clear_file_variants(&conn, &[files[1].path.clone()]).unwrap();
        assert!(get_file_variants(&conn, "/lib/bugbear").unwrap().is_empty());
    }

    #[test]
    fn split_folder_fans_into_pose_members_with_scoped_files() {
        let mut conn = test_conn();
        // one dump folder holding three model files, no pose subfolders
        let files = vec![
            file_row("/dump/mob/a.stl", "/dump/mob", 100),
            file_row("/dump/mob/b.stl", "/dump/mob", 200),
            file_row("/dump/mob/c.stl", "/dump/mob", 400),
        ];
        let models = vec![ModelRow {
            dir_path: "/dump/mob".into(),
            name: "mob".into(),
            description: None,
            designer: None,
            release_name: None,
            preview_path: None,
            source: "heuristic".into(),
            uuid: None,
            file_count: 3,
            total_size_bytes: 700,
            pose: None,
            scale: None,
            support_status: None,
            release_date: None,
            variant: None,
            sculptor: None,
            base_round_mm: None,
            base_square_mm: None,
            group_name: Some("mob".into()),
            ..Default::default()
        }];
        replace_catalog(&mut conn, "/lib", &files, &models, &[], &[], &[]).unwrap();

        // before any split: one whole-folder member, all files, no key
        let members = group_members(&conn, "mob", true).unwrap();
        assert_eq!(members.len(), 1);
        assert!(members[0].variant_key.is_none());
        assert_eq!(model_files(&conn, "/dump/mob", None).unwrap().len(), 3);

        // a.stl -> variant sword / pose 1; b.stl -> pose 2 (no variant);
        // c.stl left unassigned
        set_file_variants(
            &mut conn,
            &["/dump/mob/a.stl".into()],
            Some("sword".into()),
            Some("1".into()),
            None,
        )
        .unwrap();
        set_file_variants(
            &mut conn,
            &["/dump/mob/b.stl".into()],
            None,
            Some("2".into()),
            None,
        )
        .unwrap();

        let members = group_members(&conn, "mob", true).unwrap();
        // two facet members + one residual
        assert_eq!(members.len(), 3);
        let swordy = members
            .iter()
            .find(|m| m.variant.as_deref() == Some("sword"))
            .unwrap();
        assert_eq!(swordy.name, "mob sword 1", "label shows variant then pose");
        assert_eq!(swordy.pose.as_deref(), Some("1"));
        assert_eq!(swordy.file_count, 1);
        assert_eq!(swordy.total_size_bytes, 100.0);
        assert_eq!(
            swordy.variant_key.as_deref(),
            Some("/dump/mob\u{1f}sword\u{1f}1")
        );

        let pose2 = members
            .iter()
            .find(|m| m.pose.as_deref() == Some("2"))
            .unwrap();
        assert!(pose2.variant.is_none());
        assert_eq!(pose2.variant_key.as_deref(), Some("/dump/mob\u{1f}\u{1f}2"));

        let residual = members.iter().find(|m| m.pose.is_none()).unwrap();
        assert_eq!(residual.name, "mob (unassigned)");
        assert_eq!(residual.file_count, 1);
        assert_eq!(
            residual.variant_key.as_deref(),
            Some("/dump/mob\u{1f}\u{1f}")
        );

        // files are scoped per member, keyed on (variant, pose)
        let f1 = model_files(&conn, "/dump/mob", swordy.variant_key.as_deref()).unwrap();
        assert_eq!(f1.len(), 1);
        assert_eq!(f1[0].file_name, "a.stl");
        let fr = model_files(&conn, "/dump/mob", residual.variant_key.as_deref()).unwrap();
        assert_eq!(fr.len(), 1);
        assert_eq!(fr[0].file_name, "c.stl");

        // clearing every assignment collapses back to the whole-folder member
        clear_file_variants(&conn, &["/dump/mob/a.stl".into(), "/dump/mob/b.stl".into()]).unwrap();
        assert_eq!(group_members(&conn, "mob", true).unwrap().len(), 1);
    }

    #[test]
    fn flatten_group_clears_inferred_facets_and_file_assignments() {
        let mut conn = test_conn();
        // two heuristic members of one card, each wearing a scanner-guessed
        // variant/pose the user never asked for
        let member = |dir: &str, variant: &str, pose: &str| ModelRow {
            dir_path: dir.into(),
            name: format!("goblin {} {}", variant, pose),
            source: "heuristic".into(),
            file_count: 1,
            total_size_bytes: 10,
            variant: Some(variant.into()),
            pose: Some(pose.into()),
            group_name: Some("goblin".into()),
            ..Default::default()
        };
        let files = vec![
            file_row("/lib/goblin/spear-a/a.stl", "/lib/goblin/spear-a", 10),
            file_row("/lib/goblin/spear-b/b.stl", "/lib/goblin/spear-b", 10),
        ];
        let models = vec![
            member("/lib/goblin/spear-a", "Spear", "A"),
            member("/lib/goblin/spear-b", "Spear", "B"),
        ];
        replace_catalog(&mut conn, "/lib", &files, &models, &[], &[], &[]).unwrap();
        // plus a per-file pose assignment on one of them
        set_file_variants(
            &mut conn,
            &["/lib/goblin/spear-a/a.stl".into()],
            Some("axe".into()),
            Some("2".into()),
            None,
        )
        .unwrap();

        // before: the card carries variants and poses
        let before = group_members(&conn, "goblin", true).unwrap();
        assert!(before.iter().any(|m| m.variant.is_some()));
        assert!(before.iter().any(|m| m.pose.is_some()));

        let cleared = flatten_group(&conn, "goblin").unwrap();
        assert_eq!(cleared, 1, "the one file assignment is dropped");

        // after: every member reads back with no variant and no pose, and the
        // clear is the '' tombstone so a rescan can't resurrect the guess
        replace_catalog(&mut conn, "/lib", &files, &models, &[], &[], &[]).unwrap();
        let after = group_members(&conn, "goblin", true).unwrap();
        assert!(after.iter().all(|m| m.variant.is_none()));
        assert!(after.iter().all(|m| m.pose.is_none()));
        assert!(get_file_variants(&conn, "/lib/goblin/spear-a")
            .unwrap()
            .is_empty());

        // an unknown card is an error, not a silent no-op
        assert!(flatten_group(&conn, "nope").is_err());
    }

    #[test]
    fn pose_members_inherit_the_folders_variant() {
        // The canonical leaf after a cleanup: .../Supported/Great Swords
        // carries variant on the DIR (sidecar) and pose-only assignments on
        // the files. Pose members must stay inside the Great Swords tab —
        // using only the file-level variant collapsed them all into a
        // variantless pool and the drawer's variant tier vanished.
        let mut conn = test_conn();
        let dir = "/lib/Dark Wardens/Supported/Great Swords";
        let files = vec![
            file_row(&format!("{}/warden A.stl", dir), dir, 100),
            file_row(&format!("{}/warden B.stl", dir), dir, 100),
        ];
        let models = vec![ModelRow {
            dir_path: dir.into(),
            name: "Dark Wardens".into(),
            description: None,
            designer: None,
            release_name: None,
            preview_path: None,
            source: "metadata".into(),
            uuid: None,
            file_count: 2,
            total_size_bytes: 200,
            pose: None,
            scale: None,
            support_status: Some("supported".into()),
            release_date: None,
            variant: Some("Great Swords".into()),
            sculptor: None,
            base_round_mm: None,
            base_square_mm: None,
            group_name: Some("Dark Wardens".into()),
            ..Default::default()
        }];
        replace_catalog(&mut conn, "/lib", &files, &models, &[], &[], &[]).unwrap();
        set_file_variants(
            &mut conn,
            &[format!("{}/warden A.stl", dir)],
            None,
            Some("A".into()),
            None,
        )
        .unwrap();
        set_file_variants(
            &mut conn,
            &[format!("{}/warden B.stl", dir)],
            None,
            Some("B".into()),
            None,
        )
        .unwrap();

        let members = group_members(&conn, "Dark Wardens", true).unwrap();
        assert_eq!(members.len(), 2);
        for member in &members {
            assert_eq!(
                member.variant.as_deref(),
                Some("Great Swords"),
                "pose member lost the folder's variant: {:?}",
                member.name
            );
        }
        // the label doesn't repeat what the folder already says
        assert!(members.iter().any(|m| m.name == "Dark Wardens A"));
        // and the inherited-variant key still scopes files correctly
        let member_a = members.iter().find(|m| m.pose.as_deref() == Some("A")).unwrap();
        assert_eq!(
            member_a.variant_key.as_deref(),
            Some("/lib/Dark Wardens/Supported/Great Swords\u{1f}Great Swords\u{1f}A")
        );
        let files_a = model_files(&conn, dir, member_a.variant_key.as_deref()).unwrap();
        assert_eq!(files_a.len(), 1);
        assert_eq!(files_a[0].file_name, "warden A.stl");
    }

    #[test]
    fn per_pose_previews_do_not_clobber_each_other() {
        // The bug: rendering pose A then pose B in one dump folder made every
        // member show B, because the preview was keyed by the shared dir_path.
        let mut conn = test_conn();
        let files = vec![
            file_row("/dump/mob/a.stl", "/dump/mob", 100),
            file_row("/dump/mob/b.stl", "/dump/mob", 200),
        ];
        let models = vec![ModelRow {
            dir_path: "/dump/mob".into(),
            name: "mob".into(),
            description: None,
            designer: None,
            release_name: None,
            preview_path: None,
            source: "heuristic".into(),
            uuid: None,
            file_count: 2,
            total_size_bytes: 300,
            pose: None,
            scale: None,
            support_status: None,
            release_date: None,
            variant: None,
            sculptor: None,
            base_round_mm: None,
            base_square_mm: None,
            group_name: Some("mob".into()),
            ..Default::default()
        }];
        replace_catalog(&mut conn, "/lib", &files, &models, &[], &[], &[]).unwrap();
        set_file_variants(
            &mut conn,
            &["/dump/mob/a.stl".into()],
            None,
            Some("A".into()),
            None,
        )
        .unwrap();
        set_file_variants(
            &mut conn,
            &["/dump/mob/b.stl".into()],
            None,
            Some("B".into()),
            None,
        )
        .unwrap();

        let key_a = variant_key("/dump/mob", "", "A");
        let key_b = variant_key("/dump/mob", "", "B");

        // render pose A, then pose B — the sequence that used to clobber
        set_preview(&conn, "/dump/mob", Some(&key_a), "/previews/a.png").unwrap();
        set_preview(&conn, "/dump/mob", Some(&key_b), "/previews/b.png").unwrap();

        let members = group_members(&conn, "mob", true).unwrap();
        let preview_of = |members: &[CatalogEntry], pose: &str| {
            members
                .iter()
                .find(|m| m.pose.as_deref() == Some(pose))
                .unwrap()
                .preview_path
                .clone()
        };
        assert_eq!(
            preview_of(&members, "A").as_deref(),
            Some("/previews/a.png")
        );
        assert_eq!(
            preview_of(&members, "B").as_deref(),
            Some("/previews/b.png"),
            "B did not clobber A",
        );

        // re-rendering A updates only A
        set_preview(&conn, "/dump/mob", Some(&key_a), "/previews/a2.png").unwrap();
        let members = group_members(&conn, "mob", true).unwrap();
        assert_eq!(
            preview_of(&members, "A").as_deref(),
            Some("/previews/a2.png")
        );
        assert_eq!(
            preview_of(&members, "B").as_deref(),
            Some("/previews/b.png")
        );

        // per-variant previews survive a rescan, like the other user metadata
        replace_catalog(&mut conn, "/lib", &files, &models, &[], &[], &[]).unwrap();
        set_file_variants(
            &mut conn,
            &["/dump/mob/a.stl".into()],
            None,
            Some("A".into()),
            None,
        )
        .unwrap();
        let members = group_members(&conn, "mob", true).unwrap();
        assert_eq!(
            preview_of(&members, "A").as_deref(),
            Some("/previews/a2.png")
        );
    }

    #[test]
    fn groups_collapse_variants_and_members_come_back_ordered() {
        let mut conn = test_conn();
        // one logical model, four variant dirs: 2 supports x 2 poses
        let variant = |support: &str, pose: &str| ModelRow {
            dir_path: format!("/lib/galeb duhr/{}/{}", support, pose),
            name: format!("galeb duhr {}", pose),
            description: None,
            designer: None,
            release_name: None,
            preview_path: None,
            source: "heuristic".into(),
            uuid: None,
            file_count: 2,
            total_size_bytes: 100,
            pose: Some(pose.into()),
            scale: None,
            support_status: Some(support.into()),
            release_date: None,
            variant: None,
            sculptor: None,
            base_round_mm: None,
            base_square_mm: None,
            group_name: Some("galeb duhr".into()),
            ..Default::default()
        };
        let models = vec![
            variant("unsupported", "B"),
            variant("unsupported", "A"),
            variant("supported", "A"),
            variant("supported", "B"),
        ];
        replace_catalog(&mut conn, "/lib", &[], &models, &[], &[], &[]).unwrap();

        let page = search_groups(&conn, "", &[], None, None, None, None, None, "name", 10, 0, true).unwrap();
        assert_eq!(page.total, 1, "four variants, one card");
        let group = &page.groups[0];
        assert_eq!(group.group_name, "galeb duhr");
        assert_eq!(group.variant_count, 4);
        assert_eq!(group.pose_count, 2);
        assert_eq!(group.file_count, 8);
        let mut supports = group.support_statuses.clone();
        supports.sort();
        assert_eq!(supports, vec!["supported", "unsupported"]);

        // FTS still finds the group through any variant's name
        let page = search_groups(&conn, "galeb", &[], None, None, None, None, None, "name", 10, 0, true).unwrap();
        assert_eq!(page.total, 1);

        // The displayed logical title is independently searchable. Variant
        // names need not repeat it, and matching is partial + case-insensitive.
        conn.execute("UPDATE models SET name = 'pose' || pose", [])
            .unwrap();
        rebuild_fts(&conn).unwrap();
        assert_eq!(
            search_groups(&conn, "gal", &[], None, None, None, None, None, "name", 10, 0, true)
                .unwrap()
                .total,
            1
        );
        assert_eq!(
            search_groups(&conn, "GALEB", &[], None, None, None, None, None, "name", 10, 0, true)
                .unwrap()
                .total,
            1
        );

        // members ordered: supported A, supported B, unsupported A, ...
        let members = group_members(&conn, "GALEB DUHR", true).unwrap();
        assert_eq!(members.len(), 4, "lookup is case-insensitive");
        let order: Vec<_> = members
            .iter()
            .map(|m| (m.support_status.clone().unwrap(), m.pose.clone().unwrap()))
            .collect();
        assert_eq!(
            order,
            vec![
                ("supported".to_string(), "A".to_string()),
                ("supported".to_string(), "B".to_string()),
                ("unsupported".to_string(), "A".to_string()),
                ("unsupported".to_string(), "B".to_string()),
            ]
        );
    }

    #[test]
    fn group_renames_survive_rescans_and_merge_when_named_alike() {
        let mut conn = test_conn();
        let (files, models, tags) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();

        rename_group(&conn, "Giant Newt", "Stone Guardian").unwrap();
        let page = search_groups(&conn, "", &[], None, None, None, None, None, "name", 10, 0, true).unwrap();
        assert!(page.groups.iter().any(|g| g.group_name == "Stone Guardian"));
        assert!(!page.groups.iter().any(|g| g.group_name == "Giant Newt"));

        // findable by the new name, both in FTS and member lookup
        assert_eq!(
            search_groups(&conn, "guardian", &[], None, None, None, None, None, "name", 10, 0, true).unwrap().total,
            1
        );
        assert_eq!(group_members(&conn, "stone guardian", true).unwrap().len(), 1);

        // a rescan keeps the rename (keyed on the scanner's group name)
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();
        assert_eq!(
            search_groups(&conn, "guardian", &[], None, None, None, None, None, "name", 10, 0, true).unwrap().total,
            1
        );

        // renaming another group to the same display name merges them
        rename_group(&conn, "Bugbear", "Stone Guardian").unwrap();
        let page = search_groups(&conn, "", &[], None, None, None, None, None, "name", 10, 0, true).unwrap();
        assert_eq!(page.total, 1, "two groups now share one card");
        assert_eq!(group_members(&conn, "Stone Guardian", true).unwrap().len(), 2);

        // empty name reverts every override displaying that name
        rename_group(&conn, "Stone Guardian", "").unwrap();
        let page = search_groups(&conn, "", &[], None, None, None, None, None, "name", 10, 0, true).unwrap();
        assert_eq!(page.total, 2);
        assert!(page.groups.iter().any(|g| g.group_name == "Giant Newt"));

        assert!(rename_group(&conn, "no such group", "x").is_err());
    }

    /// The safety check a caller should run before committing a rename: two
    /// unrelated designers/releases sharing a scanner-derived group name
    /// (group_renames has no root/designer scoping) must show up as two
    /// distinct origins, not silently merge invisibly.
    #[test]
    fn group_rename_origins_reports_each_distinct_designer_release() {
        let mut conn = test_conn();
        let (files, models, tags) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();

        // Giant Newt (DTL / Critterfolk) hasn't been touched yet — one origin
        let origins = group_rename_origins(&conn, "Giant Newt").unwrap();
        assert_eq!(origins.len(), 1);
        assert_eq!(origins[0].designer.as_deref(), Some("DTL"));
        assert_eq!(origins[0].release_name.as_deref(), Some("Critterfolk"));
        assert_eq!(origins[0].model_count, 1);

        // Renaming Bugbear (no designer/release) onto the same display name
        // as Giant Newt merges them — group_rename_origins on either name
        // must now surface BOTH origins so a caller can warn before this
        // happens, not just after
        rename_group(&conn, "Giant Newt", "Stone Guardian").unwrap();
        rename_group(&conn, "Bugbear", "Stone Guardian").unwrap();
        let origins = group_rename_origins(&conn, "Stone Guardian").unwrap();
        assert_eq!(origins.len(), 2);
        assert!(origins.iter().any(|o| o.designer.as_deref() == Some("DTL")
            && o.release_name.as_deref() == Some("Critterfolk")));
        assert!(origins
            .iter()
            .any(|o| o.designer.is_none() && o.release_name.is_none()));
    }

    #[test]
    fn combine_groups_merges_selected_under_one_name() {
        let mut conn = test_conn();
        let (files, models, tags) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();

        combine_groups(
            &mut conn,
            &["Giant Newt".to_string(), "Bugbear".to_string()],
            "Dungeon Denizens",
        )
        .unwrap();

        let page = search_groups(&conn, "", &[], None, None, None, None, None, "name", 10, 0, true).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.groups[0].group_name, "Dungeon Denizens");
        assert_eq!(group_members(&conn, "Dungeon Denizens", true).unwrap().len(), 2);
        // findable by the combined name
        assert_eq!(
            search_groups(&conn, "denizens", &[], None, None, None, None, None, "name", 10, 0, true).unwrap().total,
            1
        );

        assert!(combine_groups(&mut conn, &["Dungeon Denizens".to_string()], "  ").is_err());
        assert!(combine_groups(&mut conn, &["ghost".to_string()], "x").is_err());
    }

    #[test]
    fn a_combined_group_reports_its_sources_and_splits_apart() {
        let mut conn = test_conn();
        let (files, models, tags) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();

        // An untouched group is its own only source — nothing to split
        assert_eq!(group_sources(&conn, "Giant Newt").unwrap(), ["Giant Newt"]);

        combine_groups(
            &mut conn,
            &["Giant Newt".to_string(), "Bugbear".to_string()],
            "Dungeon Denizens",
        )
        .unwrap();

        // The combined card knows what it was made from (case-insensitive)
        assert_eq!(
            group_sources(&conn, "dungeon denizens").unwrap(),
            ["Bugbear", "Giant Newt"]
        );

        // Splitting = clearing the renames: the sources come back as cards
        rename_group(&conn, "Dungeon Denizens", "").unwrap();
        let page = search_groups(&conn, "", &[], None, None, None, None, None, "name", 10, 0, true).unwrap();
        let names: Vec<_> = page.groups.iter().map(|g| g.group_name.clone()).collect();
        assert!(names.contains(&"Giant Newt".to_string()));
        assert!(names.contains(&"Bugbear".to_string()));
        assert!(!names.contains(&"Dungeon Denizens".to_string()));
    }

    #[test]
    fn detaching_one_source_leaves_the_rest_combined() {
        let mut conn = test_conn();
        let (files, models, tags) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();

        combine_groups(
            &mut conn,
            &["Giant Newt".to_string(), "Bugbear".to_string()],
            "Dungeon Denizens",
        )
        .unwrap();

        // Pull one back out: it's its own card again, the other stays put
        detach_group_source(&conn, "Dungeon Denizens", "Bugbear").unwrap();
        let page = search_groups(&conn, "", &[], None, None, None, None, None, "name", 10, 0, true).unwrap();
        let names: Vec<_> = page.groups.iter().map(|g| g.group_name.clone()).collect();
        assert!(names.contains(&"Bugbear".to_string()));
        assert!(names.contains(&"Dungeon Denizens".to_string()));
        assert_eq!(group_members(&conn, "Dungeon Denizens", true).unwrap().len(), 1);

        // Detaching something that isn't rename-combined is a clear error,
        // not a silent no-op
        assert!(detach_group_source(&conn, "Dungeon Denizens", "Bugbear").is_err());
    }

    #[test]
    fn a_user_picked_cover_fronts_the_group_card() {
        let mut conn = test_conn();
        let (files, mut models, tags) = sample_rows();
        for m in &mut models {
            m.group_name = Some("critters".into());
        }
        models[0].preview_path = Some("/previews/newt.png".into());
        models[1].preview_path = Some("/previews/bugbear.png".into());
        let picked_dir = models[0].dir_path.clone();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();

        set_group_cover(&conn, "critters", &picked_dir, None).unwrap();
        let page = search_groups(&conn, "", &[], None, None, None, None, None, "name", 10, 0, true).unwrap();
        assert_eq!(
            page.groups[0].preview_path.as_deref(),
            Some("/previews/newt.png"),
            "the chosen member's preview wins over the arbitrary MAX()"
        );
    }

    #[test]
    fn support_twins_match_exact_structure_and_facets_propagate() {
        let mut conn = test_conn();
        let mk = |dir: &str, group: &str, support: &str| ModelRow {
            dir_path: dir.into(),
            name: group.into(),
            description: None,
            designer: None,
            release_name: None,
            preview_path: None,
            source: "heuristic".into(),
            uuid: None,
            file_count: 1,
            total_size_bytes: 10,
            variant: None,
            pose: None,
            scale: None,
            support_status: Some(support.into()),
            release_date: None,
            sculptor: None,
            base_round_mm: None,
            base_square_mm: None,
            group_name: Some(group.into()),
            ..Default::default()
        };
        let models = vec![
            mk("/lib/knight/Supported/A", "knight", "supported"),
            mk("/lib/knight/Unsupported/A", "knight", "unsupported"),
            mk("/lib/knight/Unsupported/B", "knight", "unsupported"),
        ];
        replace_catalog(&mut conn, "/lib", &[], &models, &[], &[], &[]).unwrap();

        // A's builds pair up; B is the same model but a different pose dir
        let twins = support_twins(&conn, "/lib/knight/Supported/A").unwrap();
        assert_eq!(twins, ["/lib/knight/Unsupported/A"]);

        // Some values propagate, None leaves the twin's own value alone
        update_model_facets(&conn, "/lib/knight/Unsupported/A", None, Some("A"), None).unwrap();
        update_model_facets(
            &conn,
            "/lib/knight/Unsupported/A",
            Some("spear"),
            None,
            None,
        )
        .unwrap();
        let (variant, pose): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT variant, pose FROM model_user_meta WHERE dir_path = ?1",
                ["/lib/knight/Unsupported/A"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(variant.as_deref(), Some("spear"));
        assert_eq!(pose.as_deref(), Some("A"), "None must not clear the pose");

        // Group tags hit every member in one call
        add_group_tag(&conn, "knight", "Cavalry").unwrap();
        let tagged: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM model_tags WHERE tag = 'cavalry'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tagged, 3, "normalized tag lands on all three members");
        remove_group_tag(&conn, "knight", "cavalry").unwrap();
        let left: u32 = conn
            .query_row("SELECT COUNT(*) FROM model_tags", [], |row| row.get(0))
            .unwrap();
        assert_eq!(left, 0);
    }

    #[test]
    fn render_scope_groups_narrows_by_designer_and_selection() {
        let mut conn = test_conn();
        let (files, models, tags) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();

        let all = render_scope_groups(&conn, None, &[]).unwrap();
        assert_eq!(all.len(), 2, "whole catalog when unscoped");

        let dtl = render_scope_groups(&conn, Some("DTL"), &[]).unwrap();
        assert_eq!(dtl, vec!["Giant Newt".to_string()]);

        let picked = render_scope_groups(&conn, None, &["bugbear".to_string()]).unwrap();
        assert_eq!(
            picked,
            vec!["Bugbear".to_string()],
            "case-insensitive selection"
        );
    }
}
