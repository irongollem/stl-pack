use crate::error::AppError;
use rusqlite::Connection;
use std::path::Path;

use super::ingest::rebuild_fts;

const SCHEMA_VERSION: i64 = 7;

/// Open (and if needed initialize) the catalog database.
pub fn open(db_path: &Path) -> Result<Connection, AppError> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AppError::IoError(format!("Failed to create catalog dir: {}", e)))?;
    }
    let conn = Connection::open(db_path)
        .map_err(|e| AppError::ConfigError(format!("Failed to open catalog db: {}", e)))?;
    // WAL lets the scanner write while searches read
    conn.pragma_update(None, "journal_mode", "WAL").ok();
    conn.busy_timeout(std::time::Duration::from_secs(10)).ok();
    init_schema(&conn)?;
    Ok(conn)
}

pub(super) fn init_schema(conn: &Connection) -> Result<(), AppError> {
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap_or(0);
    // The base CREATEs are all IF NOT EXISTS and run on EVERY open — only
    // the versioned migrations below are gated. Gating the base batch once
    // burned us: a build stamped user_version before a newly-coded table
    // existed, and the version check then guaranteed it could never appear
    // ("no such table" with no way out short of deleting the db).
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS files (
            path        TEXT PRIMARY KEY,
            dir_path    TEXT NOT NULL,
            file_name   TEXT NOT NULL,
            extension   TEXT NOT NULL,
            size_bytes  INTEGER NOT NULL,
            modified_at INTEGER NOT NULL,
            content_hash TEXT,
            -- Opaque physical-file id ("device:inode" on Unix, volume:index
            -- on Windows), captured during duplicate scans. Paths sharing it
            -- are hardlinks — one copy on disk — so equal-hash groups with
            -- one distinct identity are already deduplicated, not reclaimable.
            file_identity TEXT,
            -- The catalog root this row was scanned under. Scans replace
            -- only their own root's slice, so several roots can share the
            -- index without a scan of one wiping the others. NULL marks a
            -- row from a pre-multi-root build, adopted by the first scan
            -- of whichever root contains it.
            root        TEXT,
            indexed_at  INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_files_dir ON files(dir_path);
        CREATE INDEX IF NOT EXISTS idx_files_size ON files(size_bytes);
        CREATE INDEX IF NOT EXISTS idx_files_content_hash ON files(content_hash)
            WHERE content_hash IS NOT NULL;

        CREATE TABLE IF NOT EXISTS models (
            dir_path     TEXT PRIMARY KEY,
            name         TEXT NOT NULL,
            description  TEXT,
            designer     TEXT,
            release_name TEXT,
            preview_path TEXT,
            source       TEXT NOT NULL DEFAULT 'heuristic',
            uuid         TEXT,
            file_count   INTEGER NOT NULL DEFAULT 0,
            total_size_bytes INTEGER NOT NULL DEFAULT 0,
            -- The logical model this row is a variant of ("galeb duhr" for
            -- galeb duhr/unsupported/A). Scanner-derived; rows sharing a
            -- group_name (case-insensitive) render as ONE catalog card.
            group_name   TEXT,
            -- Same contract as files.root (scan scoping; NULL = legacy row).
            root         TEXT,
            indexed_at   INTEGER NOT NULL
        );

        -- Keyed by dir_path + tag (not scan-generated ids) so user tags
        -- survive full rescans; source distinguishes metadata imports.
        CREATE TABLE IF NOT EXISTS model_tags (
            dir_path TEXT NOT NULL,
            tag      TEXT NOT NULL,
            source   TEXT NOT NULL DEFAULT 'user',
            PRIMARY KEY (dir_path, tag)
        );

        -- trigram tokenizer: substring + fuzzy-ish matching ("ermaid" finds
        -- "Mermaid"), not just whole-token prefix. Punctuation is folded out
        -- on the way in (see fts_insert_select) so a query typed without an
        -- apostrophe still hits a possessive designer name.
        CREATE VIRTUAL TABLE IF NOT EXISTS models_fts USING fts5(
            name, description, tags, dir_path,
            tokenize = 'trigram'
        );

        CREATE TABLE IF NOT EXISTS meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        -- User-editable metadata lives OUTSIDE models on purpose: rescans
        -- rebuild models wholesale (replace_catalog), and anything stored
        -- there is lost. Keyed by dir_path like model_tags, surviving the
        -- same way. Scanner-inferred values stay in models; a row here
        -- overrides them (COALESCE in search). Three states per column:
        -- NULL = user hasn't spoken (scanner value shows through), '' =
        -- user explicitly cleared it (reads NULLIF the scanner value away),
        -- anything else = user override.
        CREATE TABLE IF NOT EXISTS model_user_meta (
            dir_path       TEXT PRIMARY KEY,
            custom_name    TEXT,
            pose           TEXT,
            scale          TEXT,
            support_status TEXT,
            release_date   TEXT,
            preview_path   TEXT,
            -- designer (the studio/brand) rides on the release for scanned
            -- models but is overridable per model; sculptor (the artist) has
            -- no folder signal at all, so it's user/manifest-supplied only.
            -- release_name likewise overrides the scanned release.json value.
            designer       TEXT,
            sculptor       TEXT,
            release_name   TEXT,
            -- the facet between support and pose (weapon/mount/etc.)
            variant        TEXT
        );

        -- Group display-name overrides, keyed by the SCANNER's group name
        -- so they survive rescans (folder names are stable; the override
        -- rides on top). Renaming two groups to the same display name
        -- merges them — that's the manual "combine under one model" tool.
        CREATE TABLE IF NOT EXISTS group_renames (
            source_group TEXT PRIMARY KEY COLLATE NOCASE,
            display_name TEXT NOT NULL
        );

        -- Per-file pose/support assignment for libraries that dump every
        -- pose into one folder. Metadata only (keyed by path, like
        -- model_user_meta): the file never moves, but a dir carrying these
        -- rows fans out into one member per pose at read time. dir_path is
        -- denormalized from files so the read path can group without a join.
        CREATE TABLE IF NOT EXISTS file_variants (
            path           TEXT PRIMARY KEY,
            dir_path       TEXT NOT NULL,
            variant        TEXT,
            pose           TEXT,
            support_status TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_file_variants_dir ON file_variants(dir_path);

        -- Per-variant preview override. A dump folder fans out into several
        -- members that all share one dir_path, so model_user_meta.preview_path
        -- (keyed by dir_path) can't tell them apart: a render for one pose
        -- would overwrite every pose's picture. Keyed by the member's full
        -- variant_key (dir\u1f variant\u1f pose) instead, so each variant keeps
        -- its own shot. dir_path rides along for rescan-time pruning.
        -- Whole-folder models keep using model_user_meta.
        CREATE TABLE IF NOT EXISTS variant_previews (
            variant_key  TEXT PRIMARY KEY,
            dir_path     TEXT NOT NULL,
            preview_path TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_variant_previews_dir ON variant_previews(dir_path);

        -- The user's pick for a group card's main image: WHICH member
        -- represents the card, not a copied path — the member's current
        -- preview is resolved at read time, so re-renders follow along.
        -- Keyed by display name (case-insensitive) so it survives rescans.
        CREATE TABLE IF NOT EXISTS group_covers (
            group_name  TEXT PRIMARY KEY COLLATE NOCASE,
            dir_path    TEXT NOT NULL,
            variant_key TEXT
        );

        -- One row per packed model dir (compressed at rest). Derived from
        -- pack.json sidecars on rescan and kept current in place by
        -- mark_packed/mark_unpacked, like files itself.
        CREATE TABLE IF NOT EXISTS packs (
            model_dir          TEXT PRIMARY KEY,
            archive_path       TEXT NOT NULL,
            archive_size_bytes INTEGER NOT NULL,
            archive_checksum   TEXT,
            packed_at          INTEGER
        );

        -- Folders the user soft-removed: "take it out of the catalog, keep
        -- the files". Scans skip anything at or under these paths — without
        -- this a soft remove silently undoes itself on the next rescan.
        -- Absolute paths, root-independent, prefix-scoped like everything
        -- else keyed on dir_path.
        CREATE TABLE IF NOT EXISTS scan_ignores (
            dir_path   TEXT PRIMARY KEY,
            ignored_at INTEGER NOT NULL
        );

        -- Designers whose entire output counts as 18+ unless a model
        -- explicitly opts out (model_user_meta.nsfw = 0 overrides this in
        -- the effective-flag COALESCE chain — see NSFW_EFFECTIVE_SQL).
        -- COLLATE NOCASE so a designer typed with different casing than the
        -- release metadata still matches, same as every other designer
        -- comparison in this file.
        CREATE TABLE IF NOT EXISTS nsfw_designers (
            designer TEXT PRIMARY KEY COLLATE NOCASE
        );

        -- Keyed by BARE blake3 hex (the dup scanner's format), so bytes
        -- mined once are never re-parsed under another path or name.
        CREATE TABLE IF NOT EXISTS file_geometry (
            content_hash TEXT PRIMARY KEY,
            tri_count    INTEGER NOT NULL,
            x_mm REAL NOT NULL, y_mm REAL NOT NULL, z_mm REAL NOT NULL,
            volume_mm3   REAL NOT NULL,
            -- NULL = edge stats skipped over the cap, not "unknown".
            open_edges   INTEGER,
            derived_at   INTEGER NOT NULL
        );
        "#,
    )
    .map_err(|e| AppError::ConfigError(format!("Failed to init catalog schema: {}", e)))?;

    // Column migrations are shape-checked, NOT version-gated: during dev
    // iteration a build can stamp user_version before an ALTER exists in
    // code, and a version gate then locks that ALTER out forever ("no such
    // column" with no way back). Asking the table what it actually has
    // makes the check idempotent and self-healing on every open.
    // Add any missing TEXT columns to a table. Racy-safe: several
    // connections open in parallel and can both see a column missing, so the
    // loser's "duplicate column" is the goal state, not a failure.
    let add_text_columns = |table: &str, columns: &[&str]| -> Result<(), AppError> {
        let existing: Vec<String> = conn
            .prepare(&format!("PRAGMA table_info({})", table))
            .and_then(|mut stmt| {
                stmt.query_map([], |row| row.get::<_, String>(1))
                    .and_then(|rows| rows.collect())
            })
            .map_err(|e| AppError::ConfigError(format!("Failed to inspect {}: {}", table, e)))?;
        for column in columns {
            if existing.iter().any(|c| c == column) {
                continue;
            }
            if let Err(e) = conn.execute(
                &format!("ALTER TABLE {} ADD COLUMN {} TEXT", table, column),
                [],
            ) {
                if !e.to_string().contains("duplicate column name") {
                    return Err(AppError::ConfigError(format!(
                        "Failed to migrate {} (add {}): {}",
                        table, column, e
                    )));
                }
            }
        }
        Ok(())
    };
    add_text_columns(
        "models",
        &[
            "pose",
            "scale",
            "support_status",
            "release_date",
            "group_name",
            "sculptor",
            "variant",
            "root",
        ],
    )?;
    // Base sizes are canonical dimension STRINGS ("25", "60x35") — TEXT.
    // Named without the _mm suffix to sidestep the short-lived INTEGER
    // columns an early build may have created: INTEGER affinity would
    // coerce "25" back to a number and break typed string reads.
    add_text_columns("models", &["base_round", "base_square"])?;
    add_text_columns("model_user_meta", &["base_round", "base_square"])?;
    // designer already exists on models (from the release); these are the
    // per-model user overrides plus the artist, release-name and variant.
    add_text_columns(
        "model_user_meta",
        &["designer", "sculptor", "release_name", "variant"],
    )?;
    add_text_columns("file_variants", &["variant"])?;
    add_text_columns("files", &["file_identity", "root"])?;
    // Render pipeline metadata: the chosen orientation (user curation, so it
    // ALSO gets a model_user_meta overlay) and machine-measured geometry
    // (models only — dims "60.2x35.1x88.7" in mm + part count, TEXT for the
    // same affinity reasons as base_round above).
    add_text_columns("models", &["rotation", "dims_mm", "part_count"])?;
    add_text_columns("model_user_meta", &["rotation"])?;
    // Set when the file's bytes live inside a pack archive; the row's path
    // is where the file would land when extracted.
    add_text_columns("files", &["archive_path"])?;
    // The 18+ override: NULL = unset (falls through to the designer rule),
    // 1 = flagged, 0 = explicit "not 18+" beating a designer-wide rule. Not
    // add_text_columns (TEXT-only by design — see its doc comment above):
    // TEXT affinity would store '1' as the *string* "1", and the
    // effective-flag SQL's `nsfw = 1` comparison would then silently never
    // match. Same idempotent shape-check, just a different ALTER type.
    if !conn
        .prepare("PRAGMA table_info(model_user_meta)")
        .and_then(|mut stmt| {
            stmt.query_map([], |row| row.get::<_, String>(1))
                .and_then(|rows| rows.collect::<Result<Vec<String>, _>>())
        })
        .map_err(|e| AppError::ConfigError(format!("Failed to inspect model_user_meta: {}", e)))?
        .iter()
        .any(|c| c == "nsfw")
    {
        if let Err(e) = conn.execute("ALTER TABLE model_user_meta ADD COLUMN nsfw INTEGER", []) {
            if !e.to_string().contains("duplicate column name") {
                return Err(AppError::ConfigError(format!(
                    "Failed to migrate model_user_meta (add nsfw): {}",
                    e
                )));
            }
        }
    }
    // Outside the base batch: on a pre-existing db the column only exists
    // after the migration above, and indexing a missing column is an error
    // even under IF NOT EXISTS.
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_files_identity ON files(file_identity)
         WHERE file_identity IS NOT NULL",
        [],
    )
    .map_err(|e| AppError::ConfigError(format!("Failed to index file identities: {}", e)))?;
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_files_root ON files(root);
         CREATE INDEX IF NOT EXISTS idx_models_root ON models(root);",
    )
    .map_err(|e| AppError::ConfigError(format!("Failed to index roots: {}", e)))?;

    if version >= SCHEMA_VERSION {
        return Ok(());
    }

    // v3: rescue metadata that v2 stored in models — those values were
    // silently wiped by every rescan, so anything a user typed in a v2 build
    // moves to the rescan-safe table before it can be lost again
    if version < 3 {
        conn.execute(
            "INSERT OR IGNORE INTO model_user_meta
                 (dir_path, pose, scale, support_status, release_date)
             SELECT dir_path, pose, scale, support_status, release_date FROM models
             WHERE pose IS NOT NULL OR scale IS NOT NULL
                OR support_status IS NOT NULL OR release_date IS NOT NULL",
            [],
        )
        .map_err(|e| AppError::ConfigError(format!("Failed to migrate user metadata: {}", e)))?;
    }

    // v5: the FTS index is derived, so switching it to the trigram tokenizer
    // is just a drop-and-rebuild. Existing dbs kept the old default-tokenizer
    // table via IF NOT EXISTS; replace it and repopulate from current models.
    if version < 5 {
        conn.execute("DROP TABLE IF EXISTS models_fts", [])
            .map_err(|e| AppError::ConfigError(format!("Failed to drop old FTS: {}", e)))?;
        conn.execute(
            "CREATE VIRTUAL TABLE models_fts USING fts5(
                 name, description, tags, dir_path, tokenize = 'trigram')",
            [],
        )
        .map_err(|e| AppError::ConfigError(format!("Failed to create trigram FTS: {}", e)))?;
        rebuild_fts(conn)
            .map_err(|e| AppError::ConfigError(format!("Failed to rebuild FTS: {}", e)))?;
    }

    // v6/v7: logical group names are what catalog cards display, but the
    // first index omitted them. Some development databases were also stamped
    // v5/v6 while their FTS table still had SQLite's default whole-word
    // tokenizer. Replace (rather than merely refill) this derived table so
    // existing catalogs genuinely gain partial trigram matching.
    if (5..7).contains(&version) {
        conn.execute("DROP TABLE IF EXISTS models_fts", [])
            .map_err(|e| AppError::ConfigError(format!("Failed to drop old FTS: {}", e)))?;
        conn.execute(
            "CREATE VIRTUAL TABLE models_fts USING fts5(
                 name, description, tags, dir_path, tokenize = 'trigram')",
            [],
        )
        .map_err(|e| AppError::ConfigError(format!("Failed to create trigram FTS: {}", e)))?;
        rebuild_fts(conn)
            .map_err(|e| AppError::ConfigError(format!("Failed to rebuild FTS: {}", e)))?;
    }

    conn.pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|e| AppError::ConfigError(format!("Failed to set schema version: {}", e)))?;
    Ok(())
}

/// Schema init for in-memory test databases in sibling modules.
#[cfg(test)]
pub(crate) fn test_init(conn: &Connection) {
    init_schema(conn).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::db::ingest::replace_catalog;
    use crate::catalog::db::search::{search, search_groups};
    use crate::catalog::db::test_util::sample_rows;

    #[test]
    fn base_tables_self_heal_on_a_version_stamped_db() {
        // The exact failure this guards: a dev build stamped user_version=4
        // before group_renames existed in the code, so the versioned early
        // return skipped its CREATE forever ("no such table: group_renames")
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        conn.execute_batch("DROP TABLE group_renames").unwrap();

        init_schema(&conn).unwrap();
        let count: u32 = conn
            .query_row("SELECT COUNT(*) FROM group_renames", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "table recreated despite current user_version");
    }

    #[test]
    fn v6_default_tokenizer_is_replaced_with_trigram_fts() {
        let mut conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        let (files, models, tags) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();

        // Reproduce the deployed failure: version metadata claimed the FTS
        // migration had run while the physical table was still whole-word.
        conn.execute_batch(
            "DROP TABLE models_fts;
             CREATE VIRTUAL TABLE models_fts USING fts5(
                 name, description, tags, dir_path
             );
             PRAGMA user_version = 6;",
        )
        .unwrap();
        rebuild_fts(&conn).unwrap();
        assert_eq!(search(&conn, "new", &[], None, None, None, None, 10, 0, true).unwrap().total, 0);

        init_schema(&conn).unwrap();

        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name = 'models_fts'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(sql.contains("trigram"));
        assert_eq!(search(&conn, "new", &[], None, None, None, None, 10, 0, true).unwrap().total, 1);
    }

    #[test]
    fn model_columns_self_heal_on_a_version_stamped_db() {
        // Sibling failure: user_version stamped before the group_name ALTER
        // existed in code — the version gate then skipped it forever ("no
        // such column: m.group_name"). Columns are now shape-checked.
        let mut conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        conn.execute_batch("ALTER TABLE models DROP COLUMN group_name")
            .unwrap();

        init_schema(&conn).unwrap();

        let (files, models, tags) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();
        let page = search_groups(&conn, "", &[], None, None, None, None, None, "name", 10, 0, true).unwrap();
        assert_eq!(page.total, 2, "grouped search works after self-heal");
    }
}
