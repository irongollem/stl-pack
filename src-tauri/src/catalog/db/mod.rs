//! Split by domain (schema, ingest, search, groups, meta, geometry, packing,
//! housekeeping); every submodule's public API is re-exported below so
//! `db::foo(...)` call sites are unaffected by the split.

mod geometry;
mod groups;
mod housekeeping;
mod ingest;
mod meta;
mod packing;
mod schema;
mod search;
#[cfg(test)]
mod test_util;

pub use geometry::{
    duplicate_groups, duplicate_size_candidates, geometry_satisfies, known_hash, model_geometry,
    store_file_geometry, store_hash, store_identities, store_merge_results,
    stl_geometry_candidates,
};
pub use groups::{
    add_group_tag, add_tag, clear_file_variants, combine_groups, detach_group_source,
    flatten_group, get_file_variants, group_members, group_rename_origins, group_sources,
    model_files, remove_group_tag, remove_tag, render_scope_groups, rename_group, set_file_variants,
    set_group_cover, set_preview, support_twins,
};
// set_variant_preview is reached externally only through set_preview's dispatch,
// not called by its own name — kept public for API parity with pre-split db.rs.
#[allow(unused_imports)]
pub use groups::set_variant_preview;
pub use housekeeping::{
    add_scan_ignores, dirs_summary, known_roots, list_scan_ignores, model_dirs_under, move_file_index,
    move_model, move_tree_index, preview_sweep_keys, propagate_group_meta, remove_files,
    remove_models, remove_scan_ignores_under,
};
pub use ingest::{
    purge_root, rebuild_search_index, replace_catalog, root_scan_times, root_summary, stats,
};
pub use meta::{
    list_nsfw_designers, rename_designer, rename_release, set_designer_nsfw, set_measured,
    set_models_nsfw, set_rotation, update_model_facets, update_model_user_meta,
};
// set_model_preview is reached externally only through set_preview's dispatch,
// not called by its own name — kept public for API parity with pre-split db.rs.
#[allow(unused_imports)]
pub use meta::set_model_preview;
pub use packing::{
    archive_paths_for, dir_contains_pack, dir_size_bytes, mark_packed, mark_unpacked,
    pack_candidate_dirs, packed_model_dirs,
};
pub use schema::open;
#[cfg(test)]
pub(crate) use schema::test_init;
pub use search::{
    designers_for_browse, list_releases_for_browse, list_tags_for_browse, search, search_groups,
};
// designers/list_releases/list_tags are reached externally only through their
// _for_browse wrappers; GroupPage/SearchPage are returned but never named by
// callers (bound via `let page = db::search(...)`). Kept public for API
// parity with pre-split db.rs.
#[allow(unused_imports)]
pub use search::{designers, list_releases, list_tags, GroupPage, SearchPage};
