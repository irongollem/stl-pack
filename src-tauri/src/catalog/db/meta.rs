use crate::error::AppError;
use rusqlite::{params, Connection};

use super::ingest::{rebuild_fts, refresh_fts_row};

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

pub(super) fn require_model(conn: &Connection, dir_path: &str) -> Result<(), AppError> {
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

/// Permanently suppress a model's base-size suggestion (see
/// db::model_base_suggestion) — set-once, like the nsfw flag; there's no
/// "undismiss" since a fresh suggestion only reappears once curation or the
/// mined facts actually change.
pub fn dismiss_base_suggestion(conn: &Connection, dir_path: &str) -> Result<(), AppError> {
    require_model(conn, dir_path)?;
    conn.execute(
        "INSERT INTO model_user_meta (dir_path, base_suggestion_dismissed) VALUES (?1, 1)
         ON CONFLICT(dir_path) DO UPDATE SET base_suggestion_dismissed = 1",
        params![dir_path],
    )
    .map_err(|e| AppError::ConfigError(format!("Failed to dismiss base suggestion: {}", e)))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::db::*;
    use crate::catalog::db::test_util::*;
    use crate::catalog::ModelRow;

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

        let hidden = search_groups(&conn, "", &[], None, None, None, None, None, "name", 10, 0, false).unwrap();
        assert_eq!(hidden.total, 2, "newt hidden; bugbear and owlbear still show");
        assert!(!hidden.groups.iter().any(|g| g.group_name == "Giant Newt"));

        let shown = search_groups(&conn, "", &[], None, None, None, None, None, "name", 10, 0, true).unwrap();
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

        let hidden = search_groups(&conn, "", &[], None, None, None, None, None, "name", 10, 0, false).unwrap();
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
        let hidden = search_groups(&conn, "", &[], None, None, None, None, None, "name", 10, 0, false).unwrap();
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
        let page = search(&conn, "repose", &[], None, None, None, None, 10, 0, true).unwrap();
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
            search(&conn, "mounted", &[], None, None, None, None, 10, 0, true).unwrap().total,
            1,
            "variant is searchable"
        );
        // fuzzy/trigram search: possessive apostrophe is folded out, so the
        // designer matches when typed as "trappers"; and a mid-word chunk of
        // sculptor matches by substring — neither worked with prefix-only FTS
        assert_eq!(search(&conn, "trappers", &[], None, None, None, None, 10, 0, true).unwrap().total, 1);
        assert_eq!(search(&conn, "ulpto", &[], None, None, None, None, 10, 0, true).unwrap().total, 1);
        // the release name is searchable too
        assert_eq!(search(&conn, "unicorn", &[], None, None, None, None, 10, 0, true).unwrap().total, 1);
        // a multi-field query still ANDs: designer word + the model name
        assert_eq!(
            search(&conn, "trappers repose", &[], None, None, None, None, 10, 0, true).unwrap().total,
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
        let page = search(&conn, "newt", &[], None, None, None, None, 10, 0, true).unwrap();
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
        let page = search(&conn, "newt", &[], None, None, None, None, 10, 0, true).unwrap();
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
        let page = search(&conn, "newt", &[], None, None, None, None, 10, 0, true).unwrap();
        assert!(
            page.entries[0].pose.is_none(),
            "cleared pose must not resurrect"
        );
        assert!(page.entries[0].scale.is_none());

        // ...and the clear survives a rescan repopulating models.pose
        replace_catalog(&mut conn, "/lib", &files, &models, &tags, &[], &[]).unwrap();
        let page = search(&conn, "newt", &[], None, None, None, None, 10, 0, true).unwrap();
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
        let page = search(&conn, "newt", &[], None, None, None, None, 10, 0, true).unwrap();
        assert_eq!(page.entries[0].pose.as_deref(), Some("B"));
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
        let page = search_groups(&conn, "", &[], None, None, None, None, None, "name", 10, 0, true).unwrap();
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
        let page = search_groups(&conn, "", &[], None, None, None, None, None, "name", 10, 0, true).unwrap();
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
                None,
                None,
                None,
                None,
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
}
