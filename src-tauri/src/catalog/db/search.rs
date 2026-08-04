use crate::error::AppError;
use rusqlite::Connection;

use crate::catalog::{CatalogEntry, CatalogGroup, DesignerCount, ReleaseSummary};

use super::groups::cover_preview;

/// Build a trigram FTS query: each word becomes a quoted substring match,
/// ANDed. Punctuation is stripped to mirror the indexed normalization, and
/// sub-trigram (<3 char) words are dropped — trigram can't match them, so
/// keeping them would return nothing. An all-short query yields "" and the
/// caller skips the FTS filter entirely.
fn fts_query(text: &str) -> String {
    text.split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
        })
        .filter(|word| word.chars().count() >= 3)
        .map(|word| format!("\"{}\"", word))
        .collect::<Vec<_>>()
        .join(" AND ")
}

pub struct SearchPage {
    pub entries: Vec<CatalogEntry>,
    pub total: u32,
}

/// FTS + tag filters shared by the flat and grouped searches; both operate
/// on `models m` so the clauses are interchangeable. `include_nsfw = false`
/// adds the browse-surface filter (see NSFW_EFFECTIVE_SQL) — callers that
/// need every row regardless of the flag (data ops: pack, render, move,
/// delete) pass true.
fn build_search_filter(
    query: &str,
    tags: &[String],
    include_nsfw: bool,
) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
    let mut where_clauses: Vec<String> = Vec::new();
    let mut bound: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    let trimmed = query.trim();
    if !trimmed.is_empty() {
        // May be empty if every word was sub-trigram (<3 chars); then we
        // skip the FTS filter rather than MATCH "" (which errors).
        let fts = fts_query(trimmed);
        if !fts.is_empty() {
            where_clauses.push(
                "m.dir_path IN (SELECT dir_path FROM models_fts WHERE models_fts MATCH ?)"
                    .to_string(),
            );
            bound.push(Box::new(fts));
        }
    }
    for tag in tags {
        where_clauses.push(
            "EXISTS (SELECT 1 FROM model_tags mt WHERE mt.dir_path = m.dir_path AND mt.tag = ?)"
                .to_string(),
        );
        bound.push(Box::new(tag.clone()));
    }
    if !include_nsfw {
        where_clauses.push(format!("{} = 0", NSFW_EFFECTIVE_SQL));
    }
    let where_sql = if where_clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", where_clauses.join(" AND "))
    };
    (where_sql, bound)
}

/// The one SELECT that yields CatalogEntry rows. name/preview/details
/// resolve user overrides over scanner values; custom_name additionally
/// travels raw so the UI can tell an override apart from an inferred name
/// (and clear it to revert).
pub(super) fn entry_select_sql(extra_join_sql: &str, where_sql: &str, tail_sql: &str) -> String {
    format!(
        "SELECT m.dir_path, COALESCE(u.custom_name, m.name), m.description,
                NULLIF(COALESCE(u.designer, m.designer), ''),
                NULLIF(COALESCE(u.release_name, m.release_name), ''),
                COALESCE(u.preview_path, m.preview_path),
                m.file_count, m.total_size_bytes,
                COALESCE((SELECT group_concat(t.tag, char(31)) FROM model_tags t
                          WHERE t.dir_path = m.dir_path), ''),
                NULLIF(COALESCE(u.pose, m.pose), ''),
                NULLIF(COALESCE(u.scale, m.scale), ''),
                NULLIF(COALESCE(u.support_status, m.support_status), ''),
                NULLIF(COALESCE(u.release_date, m.release_date), ''),
                u.custom_name, NULLIF(COALESCE(u.sculptor, m.sculptor), ''),
                NULLIF(COALESCE(u.variant, m.variant), ''),
                COALESCE(m.group_name, m.name),
                NULLIF(COALESCE(u.base_round, m.base_round), ''),
                NULLIF(COALESCE(u.base_square, m.base_square), ''),
                {packed},
                NULLIF(COALESCE(u.rotation, m.rotation), ''),
                m.dims_mm, m.part_count,
                {nsfw}
         FROM models m LEFT JOIN model_user_meta u ON u.dir_path = m.dir_path {} {} {}",
        extra_join_sql,
        where_sql,
        tail_sql,
        packed = MODEL_PACKED_SQL,
        nsfw = NSFW_EFFECTIVE_SQL,
    )
}

/// Whether a model row's folder is fully compressed at rest: it has archived
/// files and no loose ones. Valid wherever `m` is a models row.
const MODEL_PACKED_SQL: &str = "(EXISTS (SELECT 1 FROM files f WHERE f.dir_path = m.dir_path
        AND f.archive_path IS NOT NULL)
    AND NOT EXISTS (SELECT 1 FROM files f WHERE f.dir_path = m.dir_path
        AND f.archive_path IS NULL))";

/// Whether a model row counts as 18+ right now (0 or 1): an explicit
/// per-model flag beats a whole-designer rule (nsfw_designers), which beats
/// "nobody said so" (0, the COALESCE fallback). Some(false) on the model —
/// stored by set_models_nsfw as an explicit 0, not NULL — is what lets one
/// model opt OUT of an otherwise-flagged designer, since it's read before
/// the designer subquery ever runs. Valid wherever `m` is a models row LEFT
/// JOINed to `u` (model_user_meta) — every browse query already carries
/// that join for the other user overrides. nsfw_designers' PK is COLLATE
/// NOCASE, so the designer match is already case-insensitive.
pub(super) const NSFW_EFFECTIVE_SQL: &str = "COALESCE(u.nsfw, \
    (SELECT 1 FROM nsfw_designers nd WHERE nd.designer = COALESCE(u.designer, m.designer)), 0)";

/// Tallest mined file's z-extent stands in for the model's height; volumes
/// sum (a model may print as several parts). A dir_path with no mined
/// files reads NULL here, so every range bound excludes it by construction.
const MODEL_GEOMETRY_JOIN_SQL: &str = "LEFT JOIN (
        SELECT f.dir_path AS dir_path, MAX(g.z_mm) AS height_mm, SUM(g.volume_mm3) AS volume_mm3
        FROM files f JOIN file_geometry g ON g.content_hash = f.content_hash
        GROUP BY f.dir_path
    ) geo ON geo.dir_path = m.dir_path";

/// Appends `geo.height_mm`/`geo.volume_mm3` WHERE bounds for whichever of
/// the four are set — same optional-clause idiom as the designer facet
/// clause below, just row-scoped (the flat search has one row per model
/// already, unlike search_groups which aggregates several).
fn push_geometry_where(
    where_sql: &mut String,
    bound: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
    height_min_mm: Option<f64>,
    height_max_mm: Option<f64>,
    volume_min_mm3: Option<f64>,
    volume_max_mm3: Option<f64>,
) {
    for (clause, value) in [
        ("geo.height_mm >= ?", height_min_mm),
        ("geo.height_mm <= ?", height_max_mm),
        ("geo.volume_mm3 >= ?", volume_min_mm3),
        ("geo.volume_mm3 <= ?", volume_max_mm3),
    ] {
        if let Some(v) = value {
            *where_sql = if where_sql.is_empty() {
                format!("WHERE {}", clause)
            } else {
                format!("{} AND {}", where_sql, clause)
            };
            bound.push(Box::new(v));
        }
    }
}

/// Same four bounds as push_geometry_where, but as HAVING clauses over the
/// group-level MAX/SUM search_groups computes. The aggregate is repeated
/// verbatim rather than referenced by alias, matching how this file's other
/// ORDER BY arms resolve aggregates unambiguously.
fn push_geometry_having(
    having_sql: &mut String,
    bound: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
    height_min_mm: Option<f64>,
    height_max_mm: Option<f64>,
    volume_min_mm3: Option<f64>,
    volume_max_mm3: Option<f64>,
) {
    for (clause, value) in [
        ("MAX(geo.height_mm) >= ?", height_min_mm),
        ("MAX(geo.height_mm) <= ?", height_max_mm),
        ("SUM(geo.volume_mm3) >= ?", volume_min_mm3),
        ("SUM(geo.volume_mm3) <= ?", volume_max_mm3),
    ] {
        if let Some(v) = value {
            *having_sql = if having_sql.is_empty() {
                format!("HAVING {}", clause)
            } else {
                format!("{} AND {}", having_sql, clause)
            };
            bound.push(Box::new(v));
        }
    }
}

// CAST(...AS REAL) on a canonical dimension string ("25") parses cleanly;
// on an oval/rect ("60x35") SQLite's leading-numeric-prefix cast takes only
// the first number, so the facet matches an oval by its first dimension
// only — a documented approximation, not a bug (see search_groups' base
// facet test). NULLIF drops the '' clear-tombstone before the cast so a
// deliberately-cleared field reads as NULL, not 0.0.
const CURATED_BASE_ROUND_SQL: &str =
    "CAST(NULLIF(COALESCE(u.base_round, m.base_round), '') AS REAL)";
const CURATED_BASE_SQUARE_SQL: &str =
    "CAST(NULLIF(COALESCE(u.base_square, m.base_square), '') AS REAL)";

/// Row-scoped base-size facet for the flat search: matches the effective
/// CURATED base (never a mined suggestion — see catalog::db::geometry's
/// model_base_suggestion) within a flat ±1mm window. `shape` narrows to one
/// column; "any" (or unset) matches either.
fn push_base_where(
    where_sql: &mut String,
    bound: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
    base_shape: Option<&str>,
    base_mm: Option<f64>,
) {
    let Some(mm) = base_mm else { return };
    let clause = match base_shape {
        Some("round") => format!("ABS({CURATED_BASE_ROUND_SQL} - ?) <= 1.0"),
        Some("square") => format!("ABS({CURATED_BASE_SQUARE_SQL} - ?) <= 1.0"),
        _ => format!(
            "(ABS({CURATED_BASE_ROUND_SQL} - ?) <= 1.0 OR ABS({CURATED_BASE_SQUARE_SQL} - ?) <= 1.0)"
        ),
    };
    let param_count = if matches!(base_shape, Some("round") | Some("square")) { 1 } else { 2 };
    *where_sql = if where_sql.is_empty() {
        format!("WHERE {}", clause)
    } else {
        format!("{} AND {}", where_sql, clause)
    };
    for _ in 0..param_count {
        bound.push(Box::new(mm));
    }
}

/// Group-scoped twin of push_base_where: true when ANY member of the group
/// carries a matching curated base, mirroring push_geometry_having's
/// MAX()-over-members idiom.
fn push_base_having(
    having_sql: &mut String,
    bound: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
    base_shape: Option<&str>,
    base_mm: Option<f64>,
) {
    let Some(mm) = base_mm else { return };
    let clause = match base_shape {
        Some("round") => format!("MAX(ABS({CURATED_BASE_ROUND_SQL} - ?) <= 1.0) = 1"),
        Some("square") => format!("MAX(ABS({CURATED_BASE_SQUARE_SQL} - ?) <= 1.0) = 1"),
        _ => format!(
            "MAX(ABS({CURATED_BASE_ROUND_SQL} - ?) <= 1.0 OR ABS({CURATED_BASE_SQUARE_SQL} - ?) <= 1.0) = 1"
        ),
    };
    let param_count = if matches!(base_shape, Some("round") | Some("square")) { 1 } else { 2 };
    *having_sql = if having_sql.is_empty() {
        format!("HAVING {}", clause)
    } else {
        format!("{} AND {}", having_sql, clause)
    };
    for _ in 0..param_count {
        bound.push(Box::new(mm));
    }
}

pub(super) fn map_entry_row(row: &rusqlite::Row) -> rusqlite::Result<CatalogEntry> {
    let tags_joined: String = row.get(8)?;
    Ok(CatalogEntry {
        dir_path: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        designer: row.get(3)?,
        release_name: row.get(4)?,
        preview_path: row.get(5)?,
        file_count: row.get(6)?,
        total_size_bytes: row.get::<_, i64>(7)? as f64,
        tags: if tags_joined.is_empty() {
            Vec::new()
        } else {
            tags_joined.split('\u{1f}').map(String::from).collect()
        },
        pose: row.get(9)?,
        scale: row.get(10)?,
        support_status: row.get(11)?,
        release_date: row.get(12)?,
        custom_name: row.get(13)?,
        sculptor: row.get(14)?,
        variant: row.get(15)?,
        source_group: row.get(16)?,
        base_round_mm: row.get(17)?,
        base_square_mm: row.get(18)?,
        packed: row.get(19)?,
        rotation: row.get(20)?,
        dims_mm: row.get(21)?,
        part_count: row.get(22)?,
        nsfw: row.get(23)?,
        // Whole-folder member; expand_file_variants stamps a key on any
        // synthetic pose members it derives from this row.
        variant_key: None,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn search(
    conn: &Connection,
    query: &str,
    tags: &[String],
    height_min_mm: Option<f64>,
    height_max_mm: Option<f64>,
    volume_min_mm3: Option<f64>,
    volume_max_mm3: Option<f64>,
    limit: u32,
    offset: u32,
    include_nsfw: bool,
    base_shape: Option<&str>,
    base_mm: Option<f64>,
) -> Result<SearchPage, AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Catalog search failed: {}", e));
    let (mut where_sql, mut bound) = build_search_filter(query, tags, include_nsfw);
    push_geometry_where(
        &mut where_sql,
        &mut bound,
        height_min_mm,
        height_max_mm,
        volume_min_mm3,
        volume_max_mm3,
    );
    push_base_where(&mut where_sql, &mut bound, base_shape, base_mm);
    let params_ref: Vec<&dyn rusqlite::types::ToSql> = bound.iter().map(|b| b.as_ref()).collect();

    // LEFT JOIN model_user_meta u: the nsfw filter (like the tag/FTS ones)
    // may reference `u` — entry_select_sql below always carries this same
    // join, so the count and the page agree on what "total" counts.
    let total: u32 = conn
        .query_row(
            &format!(
                "SELECT COUNT(*) FROM models m \
                 LEFT JOIN model_user_meta u ON u.dir_path = m.dir_path {} {}",
                MODEL_GEOMETRY_JOIN_SQL, where_sql
            ),
            params_ref.as_slice(),
            |row| row.get(0),
        )
        .map_err(map_err)?;

    let sql = entry_select_sql(
        MODEL_GEOMETRY_JOIN_SQL,
        &where_sql,
        &format!(
            "ORDER BY COALESCE(u.custom_name, m.name) COLLATE NOCASE LIMIT {} OFFSET {}",
            limit, offset
        ),
    );
    let mut stmt = conn.prepare(&sql).map_err(map_err)?;
    let entries = stmt
        .query_map(params_ref.as_slice(), map_entry_row)
        .map_err(map_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_err)?;

    Ok(SearchPage { entries, total })
}

pub struct GroupPage {
    pub groups: Vec<CatalogGroup>,
    pub total: u32,
    /// Groups excluded by an active height/volume filter purely for lack of
    /// mined geometry (not because they're out of range) — see
    /// search_groups' not_mined_count computation for how this is kept
    /// honest about what a geometry filter can't yet see.
    pub not_mined_count: u32,
}

/// One row per LOGICAL model: variants sharing a group_name (supported/
/// unsupported builds, poses A/B/C) collapse into a single group with
/// aggregate counts. Rows scanned before v4 have no group_name and fall
/// back to their own name — a group of one, i.e. the old behavior.
#[allow(clippy::too_many_arguments)]
pub fn search_groups(
    conn: &Connection,
    query: &str,
    tags: &[String],
    designer: Option<&str>,
    height_min_mm: Option<f64>,
    height_max_mm: Option<f64>,
    volume_min_mm3: Option<f64>,
    volume_max_mm3: Option<f64>,
    sort: &str,
    limit: u32,
    offset: u32,
    include_nsfw: bool,
    base_shape: Option<&str>,
    base_mm: Option<f64>,
) -> Result<GroupPage, AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Catalog group search failed: {}", e));
    let (mut where_sql, mut bound) = build_search_filter(query, tags, include_nsfw);
    // The designer facet narrows to one designer exactly (the dropdown
    // offers only names that exist), unlike the fuzzy FTS query
    if let Some(name) = designer.map(str::trim).filter(|d| !d.is_empty()) {
        let clause = "lower(COALESCE(u.designer, m.designer)) = lower(?)";
        where_sql = if where_sql.is_empty() {
            format!("WHERE {}", clause)
        } else {
            format!("{} AND {}", where_sql, clause)
        };
        bound.push(Box::new(name.to_string()));
    }

    // NULL never satisfies a comparison, so a geometry bound would silently
    // drop every un-mined group. Count those against the base filters —
    // before the bounds apply — so the UI can say "not mined yet: N".
    let geometry_filter_active = height_min_mm.is_some()
        || height_max_mm.is_some()
        || volume_min_mm3.is_some()
        || volume_max_mm3.is_some();
    let not_mined_count: u32 = if geometry_filter_active {
        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            bound.iter().map(|b| b.as_ref()).collect();
        conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM (
                     SELECT 1
                     FROM models m
                     LEFT JOIN model_user_meta u ON u.dir_path = m.dir_path
                     LEFT JOIN group_renames r ON r.source_group = COALESCE(m.group_name, m.name)
                     {geo}
                     {where_sql}
                     GROUP BY lower(COALESCE(r.display_name, m.group_name, m.name))
                     HAVING MAX(geo.height_mm) IS NULL
                 )",
                geo = MODEL_GEOMETRY_JOIN_SQL,
                where_sql = where_sql,
            ),
            params_ref.as_slice(),
            |row| row.get(0),
        )
        .map_err(map_err)?
    } else {
        0
    };

    let mut having_sql = String::new();
    push_geometry_having(
        &mut having_sql,
        &mut bound,
        height_min_mm,
        height_max_mm,
        volume_min_mm3,
        volume_max_mm3,
    );
    push_base_having(&mut having_sql, &mut bound, base_shape, base_mm);
    let params_ref: Vec<&dyn rusqlite::types::ToSql> = bound.iter().map(|b| b.as_ref()).collect();

    // Effective group = rename override > scanner group > own name. The
    // rename join keys on the scanner name so it survives rescans, and two
    // groups renamed alike collapse into one (deliberate merge tool).
    // Wrapped in a subquery (rather than COUNT(DISTINCT ...)) once a
    // geometry filter is active: the HAVING clause needs its own GROUP BY
    // to evaluate, and COUNT(DISTINCT) can't express that.
    let total: u32 = conn
        .query_row(
            &format!(
                "SELECT COUNT(*) FROM (
                     SELECT 1
                     FROM models m
                     LEFT JOIN model_user_meta u ON u.dir_path = m.dir_path
                     LEFT JOIN group_renames r ON r.source_group = COALESCE(m.group_name, m.name)
                     {geo}
                     {where_sql}
                     GROUP BY lower(COALESCE(r.display_name, m.group_name, m.name))
                     {having_sql}
                 )",
                geo = MODEL_GEOMETRY_JOIN_SQL,
                where_sql = where_sql,
                having_sql = having_sql,
            ),
            params_ref.as_slice(),
            |row| row.get(0),
        )
        .map_err(map_err)?;

    // Aggregates repeated verbatim in ORDER BY (not via alias) so SQLite
    // resolves them unambiguously inside expressions like the date parse
    const DESIGNER: &str = "MAX(NULLIF(COALESCE(u.designer, m.designer), ''))";
    const RELEASE: &str = "MAX(NULLIF(COALESCE(u.release_name, m.release_name), ''))";
    const REL_DATE: &str = "MAX(NULLIF(COALESCE(u.release_date, m.release_date), ''))";
    // release_date is "M/YYYY" from the release builder; split on the slash
    // and sort year-then-month. Dateless formats cast to 0 and sink to the
    // bottom of their designer rather than erroring.
    let year = format!("CAST(substr({d}, instr({d}, '/') + 1) AS INTEGER)", d = REL_DATE);
    let month = format!(
        "CAST(substr({d}, 1, instr({d}, '/') - 1) AS INTEGER)",
        d = REL_DATE
    );
    let order = match sort {
        // designer A–Z, their releases A–Z, models A–Z; metadata-less rows last
        "designer" => format!(
            "{d} IS NULL, {d} COLLATE NOCASE, {r} IS NULL, {r} COLLATE NOCASE, gname COLLATE NOCASE",
            d = DESIGNER,
            r = RELEASE
        ),
        // designer A–Z, their releases newest first (a library grows at the front)
        "designer_date" => format!(
            "{d} IS NULL, {d} COLLATE NOCASE, {t} IS NULL, {y} DESC, {mo} DESC, {r} COLLATE NOCASE, gname COLLATE NOCASE",
            d = DESIGNER,
            t = REL_DATE,
            y = year,
            mo = month,
            r = RELEASE
        ),
        // tallest/largest first; metadata-less (un-mined) rows sink last
        // rather than sorting as zero, which would read as "smallest"
        "height" => "MAX(geo.height_mm) IS NULL, MAX(geo.height_mm) DESC".to_string(),
        "volume" => "SUM(geo.volume_mm3) IS NULL, SUM(geo.volume_mm3) DESC".to_string(),
        _ => "gname COLLATE NOCASE".to_string(),
    };

    // MAX(preview) = any variant's image is better than none;
    // MAX(designer)/MAX(release) likewise pick an arbitrary non-null
    // representative
    let sql = format!(
        "SELECT COALESCE(r.display_name, m.group_name, m.name) AS gname,
                {DESIGNER},
                {RELEASE},
                {REL_DATE},
                COUNT(*),
                COUNT(DISTINCT NULLIF(COALESCE(u.pose, m.pose), '')),
                group_concat(DISTINCT NULLIF(COALESCE(u.support_status, m.support_status), '')),
                SUM(m.file_count),
                SUM(m.total_size_bytes),
                MAX(COALESCE(u.preview_path, m.preview_path)),
                MIN({packed}),
                MAX({nsfw}),
                MAX(geo.height_mm),
                SUM(geo.volume_mm3)
         FROM models m
         LEFT JOIN model_user_meta u ON u.dir_path = m.dir_path
         LEFT JOIN group_renames r ON r.source_group = COALESCE(m.group_name, m.name)
         {geo}
         {where_sql}
         GROUP BY lower(gname)
         {having_sql}
         ORDER BY {order}
         LIMIT {limit} OFFSET {offset}",
        geo = MODEL_GEOMETRY_JOIN_SQL,
        where_sql = where_sql,
        having_sql = having_sql,
        order = order,
        limit = limit,
        offset = offset,
        packed = MODEL_PACKED_SQL,
        nsfw = NSFW_EFFECTIVE_SQL,
    );
    let mut stmt = conn.prepare(&sql).map_err(map_err)?;
    let mut groups = stmt
        .query_map(params_ref.as_slice(), |row| {
            let supports: Option<String> = row.get(6)?;
            Ok(CatalogGroup {
                group_name: row.get(0)?,
                designer: row.get(1)?,
                release_name: row.get(2)?,
                release_date: row.get(3)?,
                variant_count: row.get(4)?,
                pose_count: row.get(5)?,
                support_statuses: supports
                    .map(|s| s.split(',').map(String::from).collect())
                    .unwrap_or_default(),
                file_count: row.get::<_, i64>(7)? as u32,
                total_size_bytes: row.get::<_, i64>(8)? as f64,
                preview_path: row.get(9)?,
                packed: row.get::<_, i64>(10)? != 0,
                // Any member effectively flagged makes the whole card 18+ —
                // matches the filter's row-level scope: a mixed group hidden
                // by one flagged variant should still read as flagged, not
                // as clean-with-a-gap.
                nsfw: row.get::<_, i64>(11)? != 0,
                height_mm: row.get(12)?,
                volume_mm3: row.get(13)?,
            })
        })
        .map_err(map_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_err)?;

    // A user-picked cover beats the arbitrary MAX() representative
    for group in &mut groups {
        if let Some(preview) = cover_preview(conn, &group.group_name) {
            group.preview_path = Some(preview);
        }
    }

    Ok(GroupPage { groups, total, not_mined_count })
}

pub fn list_tags(conn: &Connection) -> Result<Vec<(String, u32)>, AppError> {
    let mut stmt = conn
        .prepare(
            "SELECT tag, COUNT(*) FROM model_tags GROUP BY tag
             ORDER BY COUNT(*) DESC, tag COLLATE NOCASE",
        )
        .map_err(|e| AppError::ConfigError(format!("Tag listing failed: {}", e)))?;
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        .map_err(|e| AppError::ConfigError(format!("Tag listing failed: {}", e)))?;
    Ok(rows)
}

/// Tag facets for the browse UI. When mature content is locked, counts and
/// even tag names are derived only from visible models so a tag such as an
/// adult theme cannot leak through an otherwise empty search screen.
pub fn list_tags_for_browse(
    conn: &Connection,
    include_nsfw: bool,
) -> Result<Vec<(String, u32)>, AppError> {
    if include_nsfw {
        return list_tags(conn);
    }
    let sql = format!(
        "SELECT mt.tag, COUNT(*)
         FROM model_tags mt
         JOIN models m ON m.dir_path = mt.dir_path
         LEFT JOIN model_user_meta u ON u.dir_path = m.dir_path
         WHERE {NSFW_EFFECTIVE_SQL} = 0
         GROUP BY mt.tag
         ORDER BY COUNT(*) DESC, mt.tag COLLATE NOCASE"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| AppError::ConfigError(format!("Tag listing failed: {e}")))?;
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        .map_err(|e| AppError::ConfigError(format!("Tag listing failed: {e}")))
}

/// Distinct release_name groups found across scanned models, most-models
/// first. Purely a read over already-indexed columns — see ReleaseSummary
/// for why this isn't a "publish log".
pub fn list_releases(conn: &Connection) -> Result<Vec<ReleaseSummary>, AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Release listing failed: {}", e));
    let mut stmt = conn
        .prepare(
            "SELECT release_name,
                    -- designer isn't guaranteed uniform across a release's
                    -- models (heuristic entries may lack one); take the
                    -- first non-null value as a representative label
                    (SELECT designer FROM models m2
                     WHERE m2.release_name = m1.release_name AND designer IS NOT NULL
                     LIMIT 1),
                    COUNT(*), COALESCE(SUM(total_size_bytes), 0)
             FROM models m1
             WHERE release_name IS NOT NULL AND release_name != ''
             GROUP BY release_name
             ORDER BY COUNT(*) DESC",
        )
        .map_err(map_err)?;
    let releases = stmt
        .query_map([], |row| {
            Ok(ReleaseSummary {
                release_name: row.get(0)?,
                designer: row.get(1)?,
                model_count: row.get(2)?,
                total_size_bytes: row.get::<_, i64>(3)? as f64,
            })
        })
        .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        .map_err(map_err)?;
    Ok(releases)
}

pub fn list_releases_for_browse(
    conn: &Connection,
    include_nsfw: bool,
) -> Result<Vec<ReleaseSummary>, AppError> {
    if include_nsfw {
        return list_releases(conn);
    }
    let sql = format!(
        "SELECT m.release_name,
                MIN(COALESCE(u.designer, m.designer)),
                COUNT(*), COALESCE(SUM(m.total_size_bytes), 0)
         FROM models m
         LEFT JOIN model_user_meta u ON u.dir_path = m.dir_path
         WHERE m.release_name IS NOT NULL AND m.release_name != ''
           AND {NSFW_EFFECTIVE_SQL} = 0
         GROUP BY m.release_name
         ORDER BY COUNT(*) DESC"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| AppError::ConfigError(format!("Release listing failed: {e}")))?;
    stmt.query_map([], |row| {
        Ok(ReleaseSummary {
            release_name: row.get(0)?,
            designer: row.get(1)?,
            model_count: row.get(2)?,
            total_size_bytes: row.get::<_, i64>(3)? as f64,
        })
    })
    .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
    .map_err(|e| AppError::ConfigError(format!("Release listing failed: {e}")))
}

/// Every designer in the catalog with their logical-model (group) count,
/// A–Z — the option list for the catalog's designer filter. Counts groups,
/// not folder entries, so the numbers match the cards the filter yields.
pub fn designers(conn: &Connection) -> Result<Vec<DesignerCount>, AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Designer listing failed: {}", e));
    let mut stmt = conn
        .prepare(
            "SELECT COALESCE(u.designer, m.designer) AS d,
                    COUNT(DISTINCT lower(COALESCE(r.display_name, m.group_name, m.name)))
             FROM models m
             LEFT JOIN model_user_meta u ON u.dir_path = m.dir_path
             LEFT JOIN group_renames r ON r.source_group = COALESCE(m.group_name, m.name)
             WHERE COALESCE(u.designer, m.designer) IS NOT NULL
               AND COALESCE(u.designer, m.designer) != ''
             GROUP BY lower(d)
             ORDER BY d COLLATE NOCASE",
        )
        .map_err(map_err)?;
    let designers = stmt
        .query_map([], |row| {
            Ok(DesignerCount {
                designer: row.get(0)?,
                model_count: row.get(1)?,
            })
        })
        .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        .map_err(map_err)?;
    Ok(designers)
}

pub fn designers_for_browse(
    conn: &Connection,
    include_nsfw: bool,
) -> Result<Vec<DesignerCount>, AppError> {
    if include_nsfw {
        return designers(conn);
    }
    let sql = format!(
        "SELECT COALESCE(u.designer, m.designer) AS d,
                COUNT(DISTINCT lower(COALESCE(r.display_name, m.group_name, m.name)))
         FROM models m
         LEFT JOIN model_user_meta u ON u.dir_path = m.dir_path
         LEFT JOIN group_renames r ON r.source_group = COALESCE(m.group_name, m.name)
         WHERE COALESCE(u.designer, m.designer) IS NOT NULL
           AND COALESCE(u.designer, m.designer) != ''
           AND {NSFW_EFFECTIVE_SQL} = 0
         GROUP BY lower(d)
         ORDER BY d COLLATE NOCASE"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| AppError::ConfigError(format!("Designer listing failed: {e}")))?;
    stmt.query_map([], |row| {
        Ok(DesignerCount {
            designer: row.get(0)?,
            model_count: row.get(1)?,
        })
    })
    .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
    .map_err(|e| AppError::ConfigError(format!("Designer listing failed: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::db::*;
    use crate::catalog::db::test_util::*;
    use crate::catalog::stl_facts::StlFacts;
    use crate::catalog::{FileRow, ModelRow};

    #[test]
    fn groups_sort_by_designer_and_filter_by_designer() {
        let mut conn = test_conn();
        let model = |name: &str, designer: Option<&str>, release: Option<&str>, date: Option<&str>| ModelRow {
            dir_path: format!("/lib/{}", name),
            name: name.into(),
            description: None,
            designer: designer.map(String::from),
            release_name: release.map(String::from),
            preview_path: None,
            source: "heuristic".into(),
            uuid: None,
            file_count: 1,
            total_size_bytes: 100,
            pose: None,
            scale: None,
            support_status: None,
            release_date: date.map(String::from),
            variant: None,
            sculptor: None,
            base_round_mm: None,
            base_square_mm: None,
            group_name: None,
            ..Default::default()
        };
        let models = vec![
            model("stray", None, None, None),
            model("bog hag", Some("Bestiarum"), Some("Dread Swamp"), Some("12/2025")),
            model("ash golem", Some("Bestiarum"), Some("Emberpeak"), Some("2/2026")),
            model("zeb", Some("Archvillain"), Some("Zebra"), Some("1/2026")),
        ];
        replace_catalog(&mut conn, "/lib", &[], &models, &[], &[], &[]).unwrap();

        // designer A–Z, releases A–Z within, metadata-less rows last
        let names = |page: GroupPage| -> Vec<String> {
            page.groups.into_iter().map(|g| g.group_name).collect()
        };
        let page = search_groups(&conn, "", &[], None, None, None, None, None, "designer", 10, 0, true, None, None).unwrap();
        assert_eq!(names(page), vec!["zeb", "bog hag", "ash golem", "stray"]);

        // date mode: newest release first WITHIN a designer; 2/2026 must beat
        // 12/2025 (string comparison would get this backwards)
        let page = search_groups(&conn, "", &[], None, None, None, None, None, "designer_date", 10, 0, true, None, None).unwrap();
        assert_eq!(names(page), vec!["zeb", "ash golem", "bog hag", "stray"]);

        // the facet is exact but case-insensitive, and total honors it
        let page = search_groups(&conn, "", &[], Some("bestiarum"), None, None, None, None, "name", 10, 0, true, None, None).unwrap();
        assert_eq!(page.total, 2);
        assert_eq!(names(page), vec!["ash golem", "bog hag"]);

        // the dropdown's option list: A–Z with per-designer group counts
        let list = designers(&conn).unwrap();
        let pairs: Vec<_> = list
            .into_iter()
            .map(|d| (d.designer, d.model_count))
            .collect();
        assert_eq!(
            pairs,
            vec![("Archvillain".to_string(), 1), ("Bestiarum".to_string(), 2)]
        );

        // release fields ride on the group rows for the UI's section headers
        let page = search_groups(&conn, "", &[], None, None, None, None, None, "designer", 10, 1, true, None, None).unwrap();
        assert_eq!(page.groups[0].release_name.as_deref(), Some("Dread Swamp"));
        assert_eq!(page.groups[0].release_date.as_deref(), Some("12/2025"));
    }

    #[test]
    fn geometry_range_filters_sort_and_report_not_mined() {
        let mut conn = test_conn();
        let model = |name: &str| ModelRow {
            dir_path: format!("/lib/{}", name),
            name: name.into(),
            source: "heuristic".into(),
            file_count: 1,
            total_size_bytes: 100,
            group_name: Some(name.into()),
            ..Default::default()
        };
        let files = vec![
            FileRow {
                content_hash: Some("hash-tall".into()),
                ..file_row("/lib/tall ogre/Tall.stl", "/lib/tall ogre", 4096)
            },
            FileRow {
                content_hash: Some("hash-short".into()),
                ..file_row("/lib/short ogre/Short.stl", "/lib/short ogre", 2048)
            },
            // never mined: no content_hash, so it can't join file_geometry
            file_row("/lib/ghost/Ghost.stl", "/lib/ghost", 1024),
        ];
        let models = vec![model("tall ogre"), model("short ogre"), model("ghost")];
        replace_catalog(&mut conn, "/lib", &files, &models, &[], &[], &[]).unwrap();

        let facts = |z_mm: f32, volume_mm3: f64| StlFacts {
            tri_count: 12,
            min: (0.0, 0.0, 0.0),
            max: (10.0, 10.0, z_mm),
            volume_mm3,
            open_edge_count: Some(0),
            base: None,
        };
        store_file_geometry(&conn, "hash-tall", &facts(50.0, 2000.0), 1_000).unwrap();
        store_file_geometry(&conn, "hash-short", &facts(10.0, 500.0), 1_000).unwrap();

        // A height filter finds only the mined model in range; the query
        // itself excludes "ghost" here, so nothing is un-mined-and-hidden yet.
        let page =
            search_groups(&conn, "ogre", &[], None, Some(20.0), None, None, None, "name", 10, 0, true, None, None)
                .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.groups[0].group_name, "tall ogre");
        assert_eq!(page.groups[0].height_mm, Some(50.0));
        assert_eq!(page.groups[0].volume_mm3, Some(2000.0));
        assert_eq!(page.not_mined_count, 0);

        // Widen the query to include "ghost": it now fails the height bound
        // (NULL never satisfies a comparison) and gets COUNTED rather than
        // just silently dropped from the page.
        let page = search_groups(&conn, "", &[], None, Some(20.0), None, None, None, "name", 10, 0, true, None, None)
            .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.not_mined_count, 1);

        // Volume range narrows the same way, and un-mined counts the same way
        let page = search_groups(
            &conn, "", &[], None, None, None, Some(1000.0), Some(3000.0), "name", 10, 0, true,
            None, None,
        )
        .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.groups[0].group_name, "tall ogre");
        assert_eq!(page.not_mined_count, 1);

        // No filter active: not_mined_count stays 0 even though "ghost" is
        // un-mined — it's an answer to "what is this filter hiding", not a
        // standing background alarm.
        let page = search_groups(&conn, "", &[], None, None, None, None, None, "name", 10, 0, true, None, None).unwrap();
        assert_eq!(page.total, 3);
        assert_eq!(page.not_mined_count, 0);

        // Sort by height: tallest first, un-mined sinks last rather than
        // sorting as zero (which would read as "shortest").
        let page = search_groups(&conn, "", &[], None, None, None, None, None, "height", 10, 0, true, None, None).unwrap();
        let names: Vec<_> = page.groups.iter().map(|g| g.group_name.clone()).collect();
        assert_eq!(names, vec!["tall ogre", "short ogre", "ghost"]);

        // Sort by volume: largest first
        let page = search_groups(&conn, "", &[], None, None, None, None, None, "volume", 10, 0, true, None, None).unwrap();
        let names: Vec<_> = page.groups.iter().map(|g| g.group_name.clone()).collect();
        assert_eq!(names, vec!["tall ogre", "short ogre", "ghost"]);

        // The flat (ungrouped) search carries the same range filters
        let flat = search(&conn, "", &[], Some(20.0), None, None, None, 10, 0, true, None, None).unwrap();
        assert_eq!(flat.total, 1);
        assert_eq!(flat.entries[0].name, "tall ogre");
    }

    #[test]
    fn base_facet_matches_curated_base_within_tolerance_and_respects_precedence() {
        let mut conn = test_conn();
        let model = |name: &str, round: Option<&str>, square: Option<&str>| ModelRow {
            dir_path: format!("/lib/{}", name),
            name: name.into(),
            source: "heuristic".into(),
            file_count: 1,
            total_size_bytes: 100,
            group_name: Some(name.into()),
            base_round_mm: round.map(String::from),
            base_square_mm: square.map(String::from),
            ..Default::default()
        };
        let models = vec![
            model("round32", Some("32"), None),
            model("square25", None, Some("25")),
            model("uncurated", None, None),
            // Scanner says 99mm round, but the user's override (below) is
            // what the facet must actually see.
            model("overridden", Some("99"), None),
        ];
        replace_catalog(&mut conn, "/lib", &[], &models, &[], &[], &[]).unwrap();
        update_model_user_meta(
            &conn, "/lib/overridden", None, None, None, None, None, None, None, None, None,
            Some("32".into()), None,
        )
        .unwrap();

        let names = |page: GroupPage| -> Vec<String> {
            page.groups.into_iter().map(|g| g.group_name).collect()
        };

        // An exact shape narrows to that column, within the ±1mm window.
        let page = search_groups(&conn, "", &[], None, None, None, None, None, "name", 10, 0, true, Some("round"), Some(32.3)).unwrap();
        let mut got = names(page);
        got.sort();
        assert_eq!(got, vec!["overridden", "round32"]);

        let page = search_groups(&conn, "", &[], None, None, None, None, None, "name", 10, 0, true, Some("square"), Some(25.6)).unwrap();
        assert_eq!(names(page), vec!["square25"]);

        // "any" (no shape) matches whichever curated column is close enough.
        let page = search_groups(&conn, "", &[], None, None, None, None, None, "name", 10, 0, true, None, Some(25.6)).unwrap();
        assert_eq!(names(page), vec!["square25"]);

        // Outside the window, or the wrong shape entirely: no match — and
        // the uncurated model never matches regardless (base is curation,
        // not measurement, so it's never counted as "hidden" the way an
        // un-mined height/volume filter would be).
        let page = search_groups(&conn, "", &[], None, None, None, None, None, "name", 10, 0, true, Some("round"), Some(25.0)).unwrap();
        assert_eq!(page.total, 0);
        assert_eq!(page.not_mined_count, 0);

        // The flat search carries the same facet.
        let flat = search(&conn, "", &[], None, None, None, None, 10, 0, true, Some("round"), Some(32.3)).unwrap();
        let mut got: Vec<_> = flat.entries.into_iter().map(|e| e.name).collect();
        got.sort();
        assert_eq!(got, vec!["overridden", "round32"]);
    }

    #[test]
    fn lists_releases_grouped_from_scanned_models() {
        let mut conn = test_conn();
        let (files, models, tags) = sample_rows();
        // sample_rows' bugbear model has no release_name (heuristic, no
        // metadata) — only the newt's "Critterfolk" should surface
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();

        let releases = list_releases(&conn).unwrap();
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].release_name, "Critterfolk");
        assert_eq!(releases[0].designer.as_deref(), Some("DTL"));
        assert_eq!(releases[0].model_count, 1);
    }
}
