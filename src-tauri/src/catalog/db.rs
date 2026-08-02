use crate::error::AppError;
use rusqlite::{params, Connection};
use std::path::Path;

use super::stl_facts::StlFacts;
use super::{
    CatalogEntry, CatalogFile, CatalogGroup, CatalogStats, DesignerCount, DuplicateGroup,
    ExtensionStat, FileRow, FileVariant, FileVariantRow, GroupOrigin, ModelFileGeometry, ModelRow,
    PackRow, ReleaseSummary,
};

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

fn init_schema(conn: &Connection) -> Result<(), AppError> {
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

        -- Mined-once-per-content geometry facts (issue #15 backend mining
        -- stage), keyed by the BARE blake3 hex of the file's bytes — the dup
        -- scanner's format (see pack::bare_hash and files.content_hash
        -- above), so a file that's been mined once is never re-parsed even
        -- if it later shows up again under a different path or name.
        CREATE TABLE IF NOT EXISTS file_geometry (
            content_hash TEXT PRIMARY KEY,
            tri_count    INTEGER NOT NULL,
            x_mm REAL NOT NULL, y_mm REAL NOT NULL, z_mm REAL NOT NULL,
            volume_mm3   REAL NOT NULL,
            -- NULL = edge stats skipped above stl_facts::EDGE_STATS_MAX_TRIS
            -- (mirrors StlFacts.open_edge_count exactly), not "unknown".
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
fn prune_orphans(tx: &rusqlite::Transaction) -> Result<(), rusqlite::Error> {
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

fn rebuild_fts(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute("DELETE FROM models_fts", [])?;
    conn.execute(&fts_insert_select(), [])?;
    Ok(())
}

/// Refresh the FTS row for one model after a tag or user-meta change.
fn refresh_fts_row(conn: &Connection, dir_path: &str) -> Result<(), rusqlite::Error> {
    conn.execute("DELETE FROM models_fts WHERE dir_path = ?1", [dir_path])?;
    conn.execute(
        &format!("{} WHERE m.dir_path = ?1", fts_insert_select()),
        [dir_path],
    )?;
    Ok(())
}

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
fn entry_select_sql(where_sql: &str, tail_sql: &str) -> String {
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
         FROM models m LEFT JOIN model_user_meta u ON u.dir_path = m.dir_path {} {}",
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
const NSFW_EFFECTIVE_SQL: &str = "COALESCE(u.nsfw, \
    (SELECT 1 FROM nsfw_designers nd WHERE nd.designer = COALESCE(u.designer, m.designer)), 0)";

fn map_entry_row(row: &rusqlite::Row) -> rusqlite::Result<CatalogEntry> {
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

pub fn search(
    conn: &Connection,
    query: &str,
    tags: &[String],
    limit: u32,
    offset: u32,
    include_nsfw: bool,
) -> Result<SearchPage, AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Catalog search failed: {}", e));
    let (where_sql, bound) = build_search_filter(query, tags, include_nsfw);
    let params_ref: Vec<&dyn rusqlite::types::ToSql> = bound.iter().map(|b| b.as_ref()).collect();

    // LEFT JOIN model_user_meta u: the nsfw filter (like the tag/FTS ones)
    // may reference `u` — entry_select_sql below always carries this same
    // join, so the count and the page agree on what "total" counts.
    let total: u32 = conn
        .query_row(
            &format!(
                "SELECT COUNT(*) FROM models m \
                 LEFT JOIN model_user_meta u ON u.dir_path = m.dir_path {}",
                where_sql
            ),
            params_ref.as_slice(),
            |row| row.get(0),
        )
        .map_err(map_err)?;

    let sql = entry_select_sql(
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
    sort: &str,
    limit: u32,
    offset: u32,
    include_nsfw: bool,
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
    let params_ref: Vec<&dyn rusqlite::types::ToSql> = bound.iter().map(|b| b.as_ref()).collect();

    // Effective group = rename override > scanner group > own name. The
    // rename join keys on the scanner name so it survives rescans, and two
    // groups renamed alike collapse into one (deliberate merge tool).
    let total: u32 = conn
        .query_row(
            &format!(
                "SELECT COUNT(DISTINCT lower(COALESCE(r.display_name, m.group_name, m.name)))
                 FROM models m
                 LEFT JOIN model_user_meta u ON u.dir_path = m.dir_path
                 LEFT JOIN group_renames r ON r.source_group = COALESCE(m.group_name, m.name) {}",
                where_sql
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
                MAX({nsfw})
         FROM models m
         LEFT JOIN model_user_meta u ON u.dir_path = m.dir_path
         LEFT JOIN group_renames r ON r.source_group = COALESCE(m.group_name, m.name) {}
         GROUP BY lower(gname)
         ORDER BY {}
         LIMIT {} OFFSET {}",
        where_sql,
        order,
        limit,
        offset,
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

    Ok(GroupPage { groups, total })
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
fn cover_preview(conn: &Connection, group_name: &str) -> Option<String> {
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

/// Partial user-meta upsert used for twin propagation: only Some fields are
/// written (COALESCE keeps the twin's own values for the rest), so a
/// file-split member sending null facets never clears its twin.
pub fn update_model_facets(
    conn: &Connection,
    dir_path: &str,
    variant: Option<&str>,
    pose: Option<&str>,
    scale: Option<&str>,
) -> Result<(), AppError> {
    conn.execute(
        "INSERT INTO model_user_meta (dir_path, variant, pose, scale)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(dir_path) DO UPDATE SET
             variant = COALESCE(excluded.variant, model_user_meta.variant),
             pose    = COALESCE(excluded.pose, model_user_meta.pose),
             scale   = COALESCE(excluded.scale, model_user_meta.scale)",
        params![dir_path, variant, pose, scale],
    )
    .map_err(|e| AppError::ConfigError(format!("Failed to propagate facets: {}", e)))?;
    Ok(())
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
    sidecar: &super::pack::PackSidecar,
    kept: &[String],
) -> Result<(), AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Catalog pack update failed: {}", e));
    let archive_path = Path::new(model_dir)
        .join(super::pack::PACK_ARCHIVE_NAME)
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
            let path = super::pack::entry_disk_path(Path::new(model_dir), &entry.name)
                .to_string_lossy()
                .into_owned();
            if kept.contains(&path) {
                continue;
            }
            update
                .execute(params![
                    archive_path,
                    super::pack::bare_hash(&entry.checksum),
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

/// Whether `content_hash` (bare blake3 hex) already has a file_geometry row
/// that `edge_cap` can't improve on — checked before any disk read so a
/// duplicate's second (or hundredth) occurrence never re-parses its bytes,
/// AND so raising the edge-stats cap and re-running actually backfills the
/// rows it unblocks instead of skipping them forever.
///
/// True iff a row exists AND (its open_edges is already populated — mining
/// under any cap can't improve on a complete row — OR its tri_count still
/// exceeds `edge_cap`, meaning re-mining under this cap would just produce
/// the same NULL again). False (re-mine) only for a stored row with
/// `open_edges IS NULL` whose `tri_count` now fits under a raised cap.
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

/// Upsert one content hash's mined facts. x_mm/y_mm/z_mm are the STL's
/// bounding-box dimensions (max − min per axis), not the raw min/max —
/// the natural "measured size" number, matching the models.dims_mm
/// convention elsewhere in this file.
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

/// A model dir's mined per-file geometry, joined from file_geometry via
/// each file's content_hash — files with no hash yet, or a hash mining
/// hasn't reached, simply don't appear (an inner join, not a left join:
/// there is nothing useful to show for a file with no facts). Mirrors
/// model_files' dir-matching semantics exactly (`f.dir_path = ?1`, the
/// whole-folder case — no variant/pose narrowing, since geometry is a fact
/// about the file, not about which pose bucket it's filed under).
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

/// Rename an effective designer across the entire catalog. The values live
/// as user overrides so a rescan cannot resurrect the old scanner spelling.
/// A designer-wide mature-content rule follows the rename as well.
pub fn rename_designer(
    conn: &mut Connection,
    old_name: &str,
    new_name: &str,
) -> Result<u32, AppError> {
    let map_err = |e: rusqlite::Error| {
        AppError::ConfigError(format!("Designer rename failed: {e}"))
    };
    let tx = conn.transaction().map_err(map_err)?;
    let changed = tx
        .execute(
            "INSERT INTO model_user_meta (dir_path, designer)
             SELECT m.dir_path, ?2
             FROM models m
             LEFT JOIN model_user_meta u ON u.dir_path = m.dir_path
             WHERE lower(COALESCE(u.designer, m.designer, '')) = lower(?1)
             ON CONFLICT(dir_path) DO UPDATE SET designer = excluded.designer",
            params![old_name, new_name],
        )
        .map_err(map_err)?;
    tx.execute(
        "INSERT OR IGNORE INTO nsfw_designers (designer)
         SELECT ?2 FROM nsfw_designers WHERE lower(designer) = lower(?1)",
        params![old_name, new_name],
    )
    .map_err(map_err)?;
    tx.execute(
        "DELETE FROM nsfw_designers WHERE lower(designer) = lower(?1)",
        [old_name],
    )
    .map_err(map_err)?;
    rebuild_fts(&tx).map_err(map_err)?;
    tx.commit().map_err(map_err)?;
    Ok(changed as u32)
}

/// Rename one release/collection within a designer. Release labels are not
/// globally unique, so the designer scope prevents an identically named
/// collection from another studio being changed with it.
pub fn rename_release(
    conn: &mut Connection,
    designer: &str,
    old_name: &str,
    new_name: &str,
) -> Result<u32, AppError> {
    let map_err = |e: rusqlite::Error| AppError::ConfigError(format!("Release rename failed: {e}"));
    let tx = conn.transaction().map_err(map_err)?;
    let changed = tx
        .execute(
        "INSERT INTO model_user_meta (dir_path, release_name)
         SELECT m.dir_path, ?3
         FROM models m
         LEFT JOIN model_user_meta u ON u.dir_path = m.dir_path
         WHERE lower(COALESCE(u.designer, m.designer, '')) = lower(?1)
           AND lower(COALESCE(u.release_name, m.release_name, '')) = lower(?2)
         ON CONFLICT(dir_path) DO UPDATE SET release_name = excluded.release_name",
        params![designer, old_name, new_name],
    )
        .map_err(map_err)?;
    rebuild_fts(&tx).map_err(map_err)?;
    tx.commit().map_err(map_err)?;
    Ok(changed as u32)
}

fn require_model(conn: &Connection, dir_path: &str) -> Result<(), AppError> {
    conn.query_row(
        "SELECT 1 FROM models WHERE dir_path = ?1",
        [dir_path],
        |_| Ok(()),
    )
    .map_err(|_| AppError::NotFoundError(format!("No cataloged model at '{}'", dir_path)))
}

/// Upsert the user-editable fields (rescan-safe, see model_user_meta).
/// A None custom_name clears the override, reverting to the scanner name.
#[allow(clippy::too_many_arguments)]
pub fn update_model_user_meta(
    conn: &Connection,
    dir_path: &str,
    custom_name: Option<String>,
    pose: Option<String>,
    scale: Option<String>,
    support_status: Option<String>,
    release_date: Option<String>,
    designer: Option<String>,
    sculptor: Option<String>,
    release_name: Option<String>,
    variant: Option<String>,
    base_round_mm: Option<String>,
    base_square_mm: Option<String>,
) -> Result<(), AppError> {
    require_model(conn, dir_path)?;
    // This is the full-form save: a None facet means the field was blank in
    // the editor, i.e. the user wants it EMPTY. Storing NULL can't say that
    // — NULL means "no opinion" and COALESCE would resurrect the scanner's
    // value on the next read. Store the '' tombstone instead; reads strip
    // it with NULLIF. custom_name keeps NULL semantics: clearing it is the
    // documented way to revert to the inferred name.
    conn.execute(
        "INSERT INTO model_user_meta
             (dir_path, custom_name, pose, scale, support_status, release_date,
              designer, sculptor, release_name, variant, base_round,
              base_square)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(dir_path) DO UPDATE SET
             custom_name = excluded.custom_name,
             pose = excluded.pose,
             scale = excluded.scale,
             support_status = excluded.support_status,
             release_date = excluded.release_date,
             designer = excluded.designer,
             sculptor = excluded.sculptor,
             release_name = excluded.release_name,
             variant = excluded.variant,
             base_round = excluded.base_round,
             base_square = excluded.base_square",
        params![
            dir_path,
            custom_name,
            pose.unwrap_or_default(),
            scale.unwrap_or_default(),
            support_status.unwrap_or_default(),
            release_date.unwrap_or_default(),
            designer.unwrap_or_default(),
            sculptor.unwrap_or_default(),
            release_name.unwrap_or_default(),
            variant.unwrap_or_default(),
            base_round_mm.unwrap_or_default(),
            base_square_mm.unwrap_or_default()
        ],
    )
    .map_err(|e| AppError::ConfigError(format!("Failed to update metadata: {}", e)))?;
    // custom_name feeds search — keep the FTS row in step
    refresh_fts_row(conn, dir_path)
        .map_err(|e| AppError::ConfigError(format!("Failed to update search index: {}", e)))?;
    Ok(())
}

/// Point a model at a user-chosen or rendered preview image. Stored in
/// model_user_meta so it survives rescans and beats the scanner's pick.
pub fn set_model_preview(
    conn: &Connection,
    dir_path: &str,
    preview_path: &str,
) -> Result<(), AppError> {
    require_model(conn, dir_path)?;
    conn.execute(
        "INSERT INTO model_user_meta (dir_path, preview_path) VALUES (?1, ?2)
         ON CONFLICT(dir_path) DO UPDATE SET preview_path = excluded.preview_path",
        params![dir_path, preview_path],
    )
    .map_err(|e| AppError::ConfigError(format!("Failed to set preview: {}", e)))?;
    Ok(())
}

/// Store the chosen render orientation ("x,y,z" Blender euler degrees) —
/// user curation like preview_path, so it lives in model_user_meta and
/// survives rescans. Batch renders read it back so re-renders never need
/// repositioning.
pub fn set_rotation(conn: &Connection, dir_path: &str, rotation: &str) -> Result<(), AppError> {
    require_model(conn, dir_path)?;
    conn.execute(
        "INSERT INTO model_user_meta (dir_path, rotation) VALUES (?1, ?2)
         ON CONFLICT(dir_path) DO UPDATE SET rotation = excluded.rotation",
        params![dir_path, rotation],
    )
    .map_err(|e| AppError::ConfigError(format!("Failed to set rotation: {}", e)))?;
    Ok(())
}

/// Record machine-measured geometry (true printed dimensions in mm +
/// part count) on the scanner row. Machine-derived, so it goes to `models`
/// directly — rescan survival comes from the model.json round-trip, not
/// from user meta. A vanished row (mid-rescan) is a silent no-op.
pub fn set_measured(
    conn: &Connection,
    dir_path: &str,
    dims_mm: &str,
    part_count: u32,
) -> Result<(), AppError> {
    conn.execute(
        "UPDATE models SET dims_mm = ?2, part_count = ?3 WHERE dir_path = ?1",
        params![dir_path, dims_mm, part_count.to_string()],
    )
    .map_err(|e| AppError::ConfigError(format!("Failed to set measured geometry: {}", e)))?;
    Ok(())
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

/// Upsert model_user_meta.nsfw for a set of dirs — same ON CONFLICT DO
/// UPDATE-one-column pattern as set_model_preview/set_rotation, so the
/// other user-meta fields on each row are untouched. `None` writes NULL
/// (unset, falls through to the designer rule); `Some(false)` writes an
/// explicit 0, which is what lets one model opt OUT of an otherwise-flagged
/// designer (see NSFW_EFFECTIVE_SQL — u.nsfw is read before the designer
/// subquery even runs).
pub fn set_models_nsfw(
    conn: &Connection,
    dirs: &[String],
    nsfw: Option<bool>,
) -> Result<(), AppError> {
    let map_err =
        |e: rusqlite::Error| AppError::ConfigError(format!("Failed to set 18+ flag: {}", e));
    let value = nsfw.map(|b| b as i64);
    let mut stmt = conn
        .prepare(
            "INSERT INTO model_user_meta (dir_path, nsfw) VALUES (?1, ?2)
             ON CONFLICT(dir_path) DO UPDATE SET nsfw = excluded.nsfw",
        )
        .map_err(map_err)?;
    for dir in dirs {
        stmt.execute(params![dir, value]).map_err(map_err)?;
    }
    Ok(())
}

/// The designer-wide 18+ list, A–Z — the Settings chip list.
pub fn list_nsfw_designers(conn: &Connection) -> Result<Vec<String>, AppError> {
    let map_err = |e: rusqlite::Error| AppError::ConfigError(format!("Catalog read failed: {}", e));
    let mut stmt = conn
        .prepare("SELECT designer FROM nsfw_designers ORDER BY designer COLLATE NOCASE")
        .map_err(map_err)?;
    let rows = stmt
        .query_map([], |row| row.get(0))
        .map_err(map_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_err)?;
    Ok(rows)
}

/// Add or remove a designer from the 18+ list. OR IGNORE/plain DELETE (like
/// add_scan_ignores/remove_scan_ignores_under) rather than an error on a
/// no-op — re-adding an already-listed designer or removing an absent one
/// is just the UI's toggle landing on the state it already wants.
pub fn set_designer_nsfw(conn: &Connection, designer: &str, nsfw: bool) -> Result<(), AppError> {
    let map_err = |e: rusqlite::Error| {
        AppError::ConfigError(format!("Failed to update designer 18+ rule: {}", e))
    };
    if nsfw {
        conn.execute(
            "INSERT OR IGNORE INTO nsfw_designers (designer) VALUES (?1)",
            params![designer],
        )
        .map_err(map_err)?;
    } else {
        conn.execute(
            "DELETE FROM nsfw_designers WHERE designer = ?1",
            params![designer],
        )
        .map_err(map_err)?;
    }
    Ok(())
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

/// Rebuild the FTS index from scratch — the batch-move closer.
pub fn rebuild_search_index(conn: &Connection) -> Result<(), AppError> {
    rebuild_fts(conn)
        .map_err(|e| AppError::ConfigError(format!("Search index rebuild failed: {}", e)))
}

/// Schema init for in-memory test databases in sibling modules.
#[cfg(test)]
pub(crate) fn test_init(conn: &Connection) {
    init_schema(conn).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        conn
    }

    fn file_row(path: &str, dir_path: &str, size_bytes: i64) -> FileRow {
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

    fn sample_rows() -> (Vec<FileRow>, Vec<ModelRow>, Vec<(String, String)>) {
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

    #[test]
    fn fts_prefix_search_finds_models() {
        let mut conn = test_conn();
        let (files, models, tags) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();

        // prefix match on name
        let page = search(&conn, "new", &[], 10, 0, true).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.entries[0].name, "Giant Newt");
        assert_eq!(page.entries[0].tags, vec!["amphibian"]);

        // tag search through FTS
        let page = search(&conn, "amphib", &[], 10, 0, true).unwrap();
        assert_eq!(page.total, 1);

        // empty query lists everything
        let page = search(&conn, "", &[], 10, 0, true).unwrap();
        assert_eq!(page.total, 2);

        // tag filter
        let page = search(&conn, "", &["amphibian".to_string()], 10, 0, true).unwrap();
        assert_eq!(page.total, 1);

        // no match
        let page = search(&conn, "dragon", &[], 10, 0, true).unwrap();
        assert_eq!(page.total, 0);
    }

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
        assert_eq!(search(&conn, "newt", &[], 10, 0, true).unwrap().total, 0);
        assert_eq!(search(&conn, "bugbear", &[], 10, 0, true).unwrap().total, 1);

        let (file_count, bytes) = dirs_summary(&conn, &["/lib/bugbear".to_string()]).unwrap();
        assert_eq!((file_count, bytes), (1, 4096));
    }

    #[test]
    fn soft_removed_folders_stay_gone_across_rescans() {
        let mut conn = test_conn();
        let (files, models, tags) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();

        // Soft remove the newt: rows go, marker stays
        add_scan_ignores(&conn, &["/lib/newt".to_string()]).unwrap();
        remove_models(&mut conn, &["/lib/newt".to_string()]).unwrap();
        assert_eq!(search(&conn, "", &[], 10, 0, true).unwrap().total, 1);

        // The rescan walks the SAME disk state (newt still exists on disk) —
        // the whole point: it must not resurrect what the user removed
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();
        assert_eq!(search(&conn, "newt", &[], 10, 0, true).unwrap().total, 0);
        assert_eq!(search(&conn, "bugbear", &[], 10, 0, true).unwrap().total, 1);
        let file_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(file_count, 1, "ignored dir's files filtered at the door");

        // Unignore + rescan brings it back
        remove_scan_ignores_under(&conn, &["/lib/newt".to_string()]).unwrap();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();
        assert_eq!(search(&conn, "newt", &[], 10, 0, true).unwrap().total, 1);
    }

    #[test]
    fn nsfw_flag_and_designer_rule_hide_from_browse_but_not_data_ops() {
        let mut conn = test_conn();
        let (mut files, mut models, tags) = sample_rows();
        // A third model sharing Giant Newt's designer (DTL) but carrying no
        // per-model override of its own — the designer-rule half of the test.
        files.push(file_row("/lib/owlbear/Owlbear.stl", "/lib/owlbear", 512));
        models.push(ModelRow {
            dir_path: "/lib/owlbear".into(),
            name: "Owlbear".into(),
            designer: Some("DTL".into()),
            source: "heuristic".into(),
            file_count: 1,
            total_size_bytes: 512,
            group_name: Some("Owlbear".into()),
            ..Default::default()
        });
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();

        // Explicit per-model flag: Giant Newt is 18+, nobody else is yet
        set_models_nsfw(&conn, &["/lib/newt".to_string()], Some(true)).unwrap();

        let hidden = search_groups(&conn, "", &[], None, "name", 10, 0, false).unwrap();
        assert_eq!(hidden.total, 2, "newt hidden; bugbear and owlbear still show");
        assert!(!hidden.groups.iter().any(|g| g.group_name == "Giant Newt"));

        let shown = search_groups(&conn, "", &[], None, "name", 10, 0, true).unwrap();
        assert_eq!(shown.total, 3);
        let newt_group = shown.groups.iter().find(|g| g.group_name == "Giant Newt").unwrap();
        assert!(newt_group.nsfw, "any effectively-flagged member marks the card");

        assert!(group_members(&conn, "Giant Newt", false).unwrap().is_empty());
        let newt_members = group_members(&conn, "Giant Newt", true).unwrap();
        assert_eq!(newt_members.len(), 1);
        assert!(newt_members[0].nsfw);

        // Designer-wide rule: every DTL model becomes 18+ unless it opts out
        set_designer_nsfw(&conn, "DTL", true).unwrap();
        assert_eq!(list_nsfw_designers(&conn).unwrap(), vec!["DTL".to_string()]);

        let hidden = search_groups(&conn, "", &[], None, "name", 10, 0, false).unwrap();
        assert_eq!(hidden.total, 1, "owlbear now hidden too, via the designer rule");
        assert!(group_members(&conn, "Owlbear", false).unwrap().is_empty());
        assert!(group_members(&conn, "Owlbear", true).unwrap()[0].nsfw);
        assert!(list_tags_for_browse(&conn, false).unwrap().is_empty());
        assert_eq!(
            list_tags_for_browse(&conn, true).unwrap(),
            vec![("amphibian".to_string(), 1)]
        );
        assert!(designers_for_browse(&conn, false).unwrap().is_empty());
        assert_eq!(designers_for_browse(&conn, true).unwrap()[0].designer, "DTL");
        assert!(list_releases_for_browse(&conn, false).unwrap().is_empty());
        assert_eq!(
            list_releases_for_browse(&conn, true).unwrap()[0].release_name,
            "Critterfolk"
        );

        // Explicit "not 18+" on Owlbear overrides the designer-wide rule —
        // it's read first in the COALESCE chain (NSFW_EFFECTIVE_SQL)
        set_models_nsfw(&conn, &["/lib/owlbear".to_string()], Some(false)).unwrap();
        let hidden = search_groups(&conn, "", &[], None, "name", 10, 0, false).unwrap();
        assert_eq!(hidden.total, 2, "owlbear opted out is visible again; newt stays hidden");
        let owlbear_members = group_members(&conn, "Owlbear", false).unwrap();
        assert_eq!(owlbear_members.len(), 1);
        assert!(!owlbear_members[0].nsfw);
        assert!(
            group_members(&conn, "Giant Newt", false).unwrap().is_empty(),
            "newt's own explicit flag is untouched by owlbear's override"
        );

        // Removing the designer rule doesn't resurrect it — list_nsfw_designers
        // is the source of truth for the chip list Settings shows
        set_designer_nsfw(&conn, "DTL", false).unwrap();
        assert!(list_nsfw_designers(&conn).unwrap().is_empty());
    }

    #[test]
    fn user_tags_survive_rescan_and_metadata_tags_refresh() {
        let mut conn = test_conn();
        let (files, models, tags) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();

        add_tag(&conn, "/lib/newt", "painted").unwrap();
        // searchable immediately
        assert_eq!(search(&conn, "painted", &[], 10, 0, true).unwrap().total, 1);

        // rescan with metadata tags gone: user tag survives, metadata tag drops
        replace_catalog(&mut conn, "/lib", &files, &models, &[], &[], &[]).unwrap();
        let page = search(&conn, "", &["painted".to_string()], 10, 0, true).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(
            search(&conn, "", &["amphibian".to_string()], 10, 0, true)
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

    fn model_row(dir_path: &str, name: &str) -> ModelRow {
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

    #[test]
    fn scans_replace_only_their_own_root() {
        let mut conn = test_conn();
        let (files, models, tags) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();

        let other_files = vec![file_row("/other/wyvern/wyvern.stl", "/other/wyvern", 10)];
        let other_models = vec![model_row("/other/wyvern", "Wyvern")];
        replace_catalog(&mut conn, "/other", &other_files, &other_models, &[], &[], &[]).unwrap();

        // both roots coexist in one index
        assert_eq!(search(&conn, "", &[], 10, 0, true).unwrap().total, 3);
        assert_eq!(search(&conn, "wyvern", &[], 10, 0, true).unwrap().total, 1);
        assert_eq!(search(&conn, "newt", &[], 10, 0, true).unwrap().total, 1);

        // a root whose scan comes back empty disappears — its sibling doesn't
        replace_catalog(&mut conn, "/other", &[], &[], &[], &[], &[]).unwrap();
        assert_eq!(search(&conn, "wyvern", &[], 10, 0, true).unwrap().total, 0);
        assert_eq!(search(&conn, "newt", &[], 10, 0, true).unwrap().total, 1);
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
            search(&conn, "", &[tag.to_string()], 10, 0, true)
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

        assert_eq!(search(&conn, "newt", &[], 10, 0, true).unwrap().total, 0);
        assert_eq!(search(&conn, "ghoul", &[], 10, 0, true).unwrap().total, 1);
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
        assert_eq!(search(&conn, "", &[], 10, 0, true).unwrap().total, 1);
        assert_eq!(search(&conn, "newt", &[], 10, 0, true).unwrap().total, 0);
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

        assert_eq!(search(&conn, "newt", &[], 10, 0, true).unwrap().total, 0);
        assert_eq!(search(&conn, "wyvern", &[], 10, 0, true).unwrap().total, 1);
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
            search(&conn, "newt", &[], 10, 0, true).unwrap().total,
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
        let page = search(&conn, "newt", &[], 10, 0, true).unwrap();
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
            search(&conn, "", &["painted".to_string()], 10, 0, true)
                .unwrap()
                .total,
            1
        );
    }

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
    fn user_meta_edits_survive_rescan_and_reject_unknown_models() {
        let mut conn = test_conn();
        let (files, models, tags) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();

        update_model_user_meta(
            &conn,
            "/lib/newt",
            Some("Newt, Giant (repose)".into()),
            Some("A".into()),
            Some("32mm".into()),
            Some("supported".into()),
            None,
            Some("Dragon Trapper's Lodge".into()),
            Some("A. Sculptor".into()),
            Some("Order of the Unicorn".into()),
            Some("mounted".into()),
            None,
            None,
        )
        .unwrap();
        set_model_preview(&conn, "/lib/newt", "/appdata/previews/abc.png").unwrap();

        // the whole point of model_user_meta: a full rescan keeps user edits
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();
        let page = search(&conn, "repose", &[], 10, 0, true).unwrap();
        assert_eq!(page.total, 1, "custom name is searchable after rescan");
        let entry = &page.entries[0];
        assert_eq!(entry.name, "Newt, Giant (repose)");
        assert_eq!(entry.custom_name.as_deref(), Some("Newt, Giant (repose)"));
        assert_eq!(entry.pose.as_deref(), Some("A"));
        assert_eq!(entry.scale.as_deref(), Some("32mm"));
        // designer overrides the release's, sculptor is user-only
        assert_eq!(entry.designer.as_deref(), Some("Dragon Trapper's Lodge"));
        assert_eq!(entry.sculptor.as_deref(), Some("A. Sculptor"));
        assert_eq!(entry.release_name.as_deref(), Some("Order of the Unicorn"));
        assert_eq!(entry.variant.as_deref(), Some("mounted"));
        assert_eq!(
            search(&conn, "mounted", &[], 10, 0, true).unwrap().total,
            1,
            "variant is searchable"
        );
        // fuzzy/trigram search: possessive apostrophe is folded out, so the
        // designer matches when typed as "trappers"; and a mid-word chunk of
        // sculptor matches by substring — neither worked with prefix-only FTS
        assert_eq!(search(&conn, "trappers", &[], 10, 0, true).unwrap().total, 1);
        assert_eq!(search(&conn, "ulpto", &[], 10, 0, true).unwrap().total, 1);
        // the release name is searchable too
        assert_eq!(search(&conn, "unicorn", &[], 10, 0, true).unwrap().total, 1);
        // a multi-field query still ANDs: designer word + the model name
        assert_eq!(
            search(&conn, "trappers repose", &[], 10, 0, true).unwrap().total,
            1
        );
        assert_eq!(
            entry.preview_path.as_deref(),
            Some("/appdata/previews/abc.png")
        );

        // clearing the NAME reverts to the scanner name (custom_name keeps
        // NULL semantics — a model always needs some name to fall back to);
        // clearing a FACET means empty, full stop — the scanner's value
        // must NOT resurrect it (that was the un-deletable-pose bug)
        update_model_user_meta(
            &conn,
            "/lib/newt",
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
            None,
        )
        .unwrap();
        let page = search(&conn, "newt", &[], 10, 0, true).unwrap();
        assert_eq!(page.entries[0].name, "Giant Newt");
        assert!(
            page.entries[0].designer.is_none(),
            "cleared designer stays cleared, not the release's"
        );
        assert!(page.entries[0].sculptor.is_none());
        assert!(
            page.entries[0].release_name.is_none(),
            "cleared release stays cleared, not the scanned one"
        );
        // ...but the preview set separately is untouched by a metadata save
        assert_eq!(
            page.entries[0].preview_path.as_deref(),
            Some("/appdata/previews/abc.png")
        );

        assert!(update_model_user_meta(
            &conn, "/nope", None, None, None, None, None, None, None, None, None, None,
            None
        )
        .is_err());
        assert!(set_model_preview(&conn, "/nope", "/x.png").is_err());
    }

    #[test]
    fn clearing_a_scanner_provided_pose_sticks() {
        let mut conn = test_conn();
        let (files, mut models, tags) = sample_rows();
        // the scanner inferred these from model.json / the folder name
        models[0].pose = Some("Attacking".into());
        models[0].scale = Some("32mm".into());
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();

        // untouched, the scanner value shows through
        let page = search(&conn, "newt", &[], 10, 0, true).unwrap();
        assert_eq!(page.entries[0].pose.as_deref(), Some("Attacking"));

        // the user blanks the pose (the full-form save sends None for
        // every empty field) — the scanner value must NOT come back
        update_model_user_meta(
            &conn,
            "/lib/newt",
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
            None,
        )
        .unwrap();
        let page = search(&conn, "newt", &[], 10, 0, true).unwrap();
        assert!(
            page.entries[0].pose.is_none(),
            "cleared pose must not resurrect"
        );
        assert!(page.entries[0].scale.is_none());

        // ...and the clear survives a rescan repopulating models.pose
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();
        let page = search(&conn, "newt", &[], 10, 0, true).unwrap();
        assert!(
            page.entries[0].pose.is_none(),
            "rescan must not resurrect the cleared pose"
        );

        // a later real edit still beats the tombstone
        update_model_user_meta(
            &conn,
            "/lib/newt",
            None,
            Some("B".into()),
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
        let page = search(&conn, "newt", &[], 10, 0, true).unwrap();
        assert_eq!(page.entries[0].pose.as_deref(), Some("B"));
    }

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
        assert_eq!(search(&conn, "new", &[], 10, 0, true).unwrap().total, 0);

        init_schema(&conn).unwrap();

        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name = 'models_fts'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(sql.contains("trigram"));
        assert_eq!(search(&conn, "new", &[], 10, 0, true).unwrap().total, 1);
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
        let page = search_groups(&conn, "", &[], None, "name", 10, 0, true).unwrap();
        assert_eq!(page.total, 2, "grouped search works after self-heal");
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

        let page = search_groups(&conn, "", &[], None, "name", 10, 0, true).unwrap();
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
        let page = search_groups(&conn, "galeb", &[], None, "name", 10, 0, true).unwrap();
        assert_eq!(page.total, 1);

        // The displayed logical title is independently searchable. Variant
        // names need not repeat it, and matching is partial + case-insensitive.
        conn.execute("UPDATE models SET name = 'pose' || pose", [])
            .unwrap();
        rebuild_fts(&conn).unwrap();
        assert_eq!(
            search_groups(&conn, "gal", &[], None, "name", 10, 0, true)
                .unwrap()
                .total,
            1
        );
        assert_eq!(
            search_groups(&conn, "GALEB", &[], None, "name", 10, 0, true)
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
        let page = search_groups(&conn, "", &[], None, "designer", 10, 0, true).unwrap();
        assert_eq!(names(page), vec!["zeb", "bog hag", "ash golem", "stray"]);

        // date mode: newest release first WITHIN a designer; 2/2026 must beat
        // 12/2025 (string comparison would get this backwards)
        let page = search_groups(&conn, "", &[], None, "designer_date", 10, 0, true).unwrap();
        assert_eq!(names(page), vec!["zeb", "ash golem", "bog hag", "stray"]);

        // the facet is exact but case-insensitive, and total honors it
        let page = search_groups(&conn, "", &[], Some("bestiarum"), "name", 10, 0, true).unwrap();
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
        let page = search_groups(&conn, "", &[], None, "designer", 10, 1, true).unwrap();
        assert_eq!(page.groups[0].release_name.as_deref(), Some("Dread Swamp"));
        assert_eq!(page.groups[0].release_date.as_deref(), Some("12/2025"));
    }

    #[test]
    fn facet_renames_update_all_matching_models_and_keep_release_scope() {
        let mut conn = test_conn();
        let (files, mut models, tags) = sample_rows();
        let mut second_dtl = models[0].clone();
        second_dtl.dir_path = "/lib/toad".into();
        second_dtl.name = "Giant Toad".into();
        second_dtl.group_name = Some("Giant Toad".into());
        let mut other_studio = models[0].clone();
        other_studio.dir_path = "/lib/other-newt".into();
        other_studio.name = "Other Newt".into();
        other_studio.group_name = Some("Other Newt".into());
        other_studio.designer = Some("Other Studio".into());
        models.extend([second_dtl, other_studio]);
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();

        set_designer_nsfw(&conn, "DTL", true).unwrap();
        assert_eq!(rename_designer(&mut conn, "dtl", "Dragon Trappers Lodge").unwrap(), 2);
        assert_eq!(list_nsfw_designers(&conn).unwrap(), vec!["Dragon Trappers Lodge"]);
        let page = search_groups(&conn, "", &[], None, "name", 10, 0, true).unwrap();
        assert_eq!(
            page.groups
                .iter()
                .filter(|group| group.designer.as_deref() == Some("Dragon Trappers Lodge"))
                .count(),
            2
        );

        assert_eq!(
            rename_release(
                &mut conn,
                "Dragon Trappers Lodge",
                "Critterfolk",
                "Critter Folk",
            )
            .unwrap(),
            2
        );
        let page = search_groups(&conn, "", &[], None, "name", 10, 0, true).unwrap();
        for group in &page.groups {
            if group.designer.as_deref() == Some("Dragon Trappers Lodge") {
                assert_eq!(group.release_name.as_deref(), Some("Critter Folk"));
            }
        }
        assert_eq!(
            page.groups
                .iter()
                .find(|group| group.designer.as_deref() == Some("Other Studio"))
                .and_then(|group| group.release_name.as_deref()),
            Some("Critterfolk")
        );
        assert_eq!(
            search_groups(
                &conn,
                "critter folk",
                &[],
                Some("Dragon Trappers Lodge"),
                "name",
                10,
                0,
                true,
            )
            .unwrap()
            .total,
            2
        );
    }

    #[test]
    fn group_renames_survive_rescans_and_merge_when_named_alike() {
        let mut conn = test_conn();
        let (files, models, tags) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();

        rename_group(&conn, "Giant Newt", "Stone Guardian").unwrap();
        let page = search_groups(&conn, "", &[], None, "name", 10, 0, true).unwrap();
        assert!(page.groups.iter().any(|g| g.group_name == "Stone Guardian"));
        assert!(!page.groups.iter().any(|g| g.group_name == "Giant Newt"));

        // findable by the new name, both in FTS and member lookup
        assert_eq!(
            search_groups(&conn, "guardian", &[], None, "name", 10, 0, true).unwrap().total,
            1
        );
        assert_eq!(group_members(&conn, "stone guardian", true).unwrap().len(), 1);

        // a rescan keeps the rename (keyed on the scanner's group name)
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();
        assert_eq!(
            search_groups(&conn, "guardian", &[], None, "name", 10, 0, true).unwrap().total,
            1
        );

        // renaming another group to the same display name merges them
        rename_group(&conn, "Bugbear", "Stone Guardian").unwrap();
        let page = search_groups(&conn, "", &[], None, "name", 10, 0, true).unwrap();
        assert_eq!(page.total, 1, "two groups now share one card");
        assert_eq!(group_members(&conn, "Stone Guardian", true).unwrap().len(), 2);

        // empty name reverts every override displaying that name
        rename_group(&conn, "Stone Guardian", "").unwrap();
        let page = search_groups(&conn, "", &[], None, "name", 10, 0, true).unwrap();
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

        let page = search_groups(&conn, "", &[], None, "name", 10, 0, true).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.groups[0].group_name, "Dungeon Denizens");
        assert_eq!(group_members(&conn, "Dungeon Denizens", true).unwrap().len(), 2);
        // findable by the combined name
        assert_eq!(
            search_groups(&conn, "denizens", &[], None, "name", 10, 0, true).unwrap().total,
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
        let page = search_groups(&conn, "", &[], None, "name", 10, 0, true).unwrap();
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
        let page = search_groups(&conn, "", &[], None, "name", 10, 0, true).unwrap();
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
        let page = search_groups(&conn, "", &[], None, "name", 10, 0, true).unwrap();
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

        let page = search(&conn, "shiny", &[], 10, 0, true).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.entries[0].dir_path, "/lib/amphibians/newt");
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

    #[test]
    fn pack_marking_flips_flags_stats_and_back() {
        let mut conn = test_conn();
        let (files, models, tags) = sample_rows();
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();

        let sidecar = super::super::pack::PackSidecar {
            format: super::super::pack::PACK_FORMAT.into(),
            version: super::super::pack::PACK_VERSION,
            generator: "plinth/test".into(),
            archive: super::super::pack::PACK_ARCHIVE_NAME.into(),
            archive_checksum: "blake3:abc".into(),
            archive_size_bytes: 512,
            packed_at: 42,
            files: vec![super::super::pack::PackFileEntry {
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

    #[test]
    fn rotation_and_measured_round_trip_through_the_entry_read() {
        let mut conn = test_conn();
        let (files, mut models, tags) = sample_rows();
        // scanner-provided values (from a model.json) on the newt
        models[0].rotation = Some("0,0,90".into());
        models[0].dims_mm = Some("60.2x35.1x88.7".into());
        models[0].part_count = Some("3".into());
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();

        let newt = group_members(&conn, "Giant Newt", true).unwrap();
        assert_eq!(newt[0].rotation.as_deref(), Some("0,0,90"));
        assert_eq!(newt[0].dims_mm.as_deref(), Some("60.2x35.1x88.7"));
        assert_eq!(newt[0].part_count.as_deref(), Some("3"));
        // packed flag kept its positional index — the new columns appended after
        assert!(!newt[0].packed);

        // the studio-saved rotation (user meta) overlays the scanner value
        set_rotation(&conn, "/lib/newt", "90,0,0").unwrap();
        let newt = group_members(&conn, "Giant Newt", true).unwrap();
        assert_eq!(newt[0].rotation.as_deref(), Some("90,0,0"));

        // measured geometry lands in place (the batch job path)
        set_measured(&conn, "/lib/bugbear", "25.0x25.0x40.5", 1).unwrap();
        let bugbear = group_members(&conn, "Bugbear", true).unwrap();
        assert_eq!(bugbear[0].dims_mm.as_deref(), Some("25.0x25.0x40.5"));
        assert_eq!(bugbear[0].part_count.as_deref(), Some("1"));

        // a rescan rebuilds models wholesale — the user-meta rotation survives
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();
        let newt = group_members(&conn, "Giant Newt", true).unwrap();
        assert_eq!(
            newt[0].rotation.as_deref(),
            Some("90,0,0"),
            "user-meta rotation survives the rescan"
        );
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
