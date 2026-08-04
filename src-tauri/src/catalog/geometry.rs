//! Geometry mining: file_geometry rows keyed by bare blake3 content hash,
//! so any given bytes are mined once across every path that holds them.
//! Candidates come from the files index, not a disk walk — a file edited
//! on disk re-mines only after a rescan drops its stored hash.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use sysinfo::{MemoryRefreshKind, RefreshKind, System};

use crate::error::AppError;

use super::db;
use super::stl_facts::{
    parse_header, FactsAccumulator, StlFacts, COUNT_LEN, EDGE_STATS_MAX_TRIS, HEADER_LEN, RECORD_LEN,
};

// A whole number of records per chunk so parsing never straddles a read;
// this buffer is the entire per-file footprint. No size cap on purpose: a
// cutoff would make big files silently show no geometry at all.
const STREAM_CHUNK_BYTES: usize = RECORD_LEN * 16 * 1024;

// An eighth, not more: mining runs alongside the user's slicer and
// browser, and must never be why the OS starts swapping.
const EDGE_MAP_RAM_FRACTION: u64 = 8;

// ~3 unique edges/triangle x ~34 B/map entry x ~1.5 growth headroom.
// Sized to the worst case (fully open mesh) on purpose — leaky meshes are
// exactly what this signal exists to catch.
const BYTES_PER_TRIANGLE_WORST_CASE: u64 = 150;

// 30M tris x 150 B ~= 4.5 GB transient — the most a background pass
// should ask of any machine, however large.
const RECOMMENDED_EDGE_CAP_CEILING: u32 = 30_000_000;

/// Total (not free) RAM: the edge map competes with everything running.
/// Split from the sysinfo caller so the formula tests without mocking.
fn recommended_edge_cap_for(total_ram_bytes: u64) -> u32 {
    if total_ram_bytes == 0 {
        // sysinfo reported nothing usable — fall back to the safe floor
        // rather than let a bogus 0/0 division stand in for "unlimited".
        return EDGE_STATS_MAX_TRIS;
    }
    let budget_bytes = total_ram_bytes / EDGE_MAP_RAM_FRACTION;
    let tris = budget_bytes / BYTES_PER_TRIANGLE_WORST_CASE;
    tris.clamp(
        u64::from(EDGE_STATS_MAX_TRIS),
        u64::from(RECOMMENDED_EDGE_CAP_CEILING),
    ) as u32
}

pub fn recommended_edge_cap() -> u32 {
    let sys = System::new_with_specifics(
        RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram()),
    );
    recommended_edge_cap_for(sys.total_memory())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GeometryOutcome {
    pub mined: u32,
    /// Skipped without a disk read: the hash already has a complete row.
    pub already_known: u32,
    /// Unreadable or not a well-formed binary STL; the run continues.
    pub failed: u32,
}

/// Outer Err: the read failed or the size changed mid-read — no hash is
/// trustworthy, store nothing. Inner Err: the bytes hashed fine but are
/// not a well-formed binary STL.
pub(crate) fn stream_mine(
    path: &str,
    edge_cap: u32,
) -> std::io::Result<(String, Result<StlFacts, String>)> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let mut hasher = blake3::Hasher::new();

    let mut preamble = [0u8; HEADER_LEN + COUNT_LEN];
    let mut got = 0usize;
    while got < preamble.len() {
        let n = file.read(&mut preamble[got..])?;
        if n == 0 {
            break;
        }
        got += n;
    }
    hasher.update(&preamble[..got]);
    if got < preamble.len() {
        // Shorter than any binary STL can be; the whole file is hashed.
        if got as u64 != len {
            return Err(std::io::Error::other("file changed while being read"));
        }
        return Ok((
            hasher.finalize().to_hex().to_string(),
            Err(format!(
                "file is only {got} bytes — too short for a binary STL's 84-byte header+count"
            )),
        ));
    }

    let tri_count = parse_header(&preamble).expect("preamble is exactly header+count sized");
    let expected_len =
        (HEADER_LEN + COUNT_LEN) as u64 + tri_count as u64 * RECORD_LEN as u64;
    let mut acc = if tri_count > 0 && len == expected_len {
        Some(FactsAccumulator::with_cap(tri_count, edge_cap).expect("tri_count checked non-zero"))
    } else {
        None
    };
    let parse_err = if tri_count == 0 {
        "binary STL header reports zero triangles".to_string()
    } else {
        format!(
            "byte length {len} does not match the {tri_count} triangles the header declares \
             (expected exactly {expected_len} bytes) — not a well-formed binary STL, or an ASCII STL"
        )
    };

    let mut buf = vec![0u8; STREAM_CHUNK_BYTES];
    let mut total: u64 = got as u64;
    loop {
        // Fill the chunk completely (or to EOF): when the length formula
        // matched, every full chunk is then a whole number of records, and
        // the final short chunk still is — total record bytes divide by 50.
        let mut filled = 0usize;
        while filled < buf.len() {
            let n = file.read(&mut buf[filled..])?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        if filled == 0 {
            break;
        }
        hasher.update(&buf[..filled]);
        total += filled as u64;
        if let Some(acc) = acc.as_mut() {
            for record in buf[..filled].chunks_exact(RECORD_LEN) {
                acc.push_record(record.try_into().expect("chunks_exact yields RECORD_LEN"));
            }
        }
    }
    if total != len {
        // Grew or shrank under us (NAS sync, slicer re-save): neither the
        // hash nor any partial facts describe a coherent file.
        return Err(std::io::Error::other("file changed while being read"));
    }

    let hash = hasher.finalize().to_hex().to_string();
    Ok((hash, acc.map(FactsAccumulator::finish).ok_or(parse_err)))
}

/// Streams every un-mined loose STL once, hashing and accumulating facts
/// in one pass; hashes land in files.content_hash as a side effect. A row
/// stored under a smaller cap re-streams when `edge_cap` now covers it.
pub fn mine_geometry(
    conn: &Connection,
    cancel: &AtomicBool,
    edge_cap: u32,
    mut on_progress: impl FnMut(u32, u32),
) -> Result<GeometryOutcome, AppError> {
    let candidates = db::stl_geometry_candidates(conn)?;
    let total = candidates.len() as u32;
    let mut outcome = GeometryOutcome::default();

    for (index, (path, known_hash)) in candidates.into_iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            return Err(AppError::UserCancelled("Geometry mining cancelled".into()));
        }
        on_progress(index as u32, total);

        if let Some(hash) = known_hash.as_deref() {
            if db::geometry_satisfies(conn, hash, edge_cap)? {
                outcome.already_known += 1;
                continue;
            }
        }

        let (hash, parsed) = match stream_mine(&path, edge_cap) {
            Ok(streamed) => streamed,
            Err(_) => {
                outcome.failed += 1;
                continue;
            }
        };
        if known_hash.is_none() {
            db::store_hash(conn, &path, &hash)?;
        }
        if db::geometry_satisfies(conn, &hash, edge_cap)? {
            outcome.already_known += 1;
            continue;
        }

        match parsed {
            Ok(facts) => {
                let derived_at = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                db::store_file_geometry(conn, &hash, &facts, derived_at)?;
                outcome.mined += 1;
            }
            Err(_) => outcome.failed += 1,
        }
    }

    on_progress(total, total);
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::stl_facts::parse_binary_stl_facts;
    use crate::catalog::{FileRow, ModelRow};
    use std::fs;

    /// Builds a minimal well-formed binary STL: an 80-byte header, a
    /// triangle count, then one triangle's 9 floats — mirrors
    /// stl_facts::tests::build_binary_stl, kept local since that helper is
    /// private to its own module.
    fn build_binary_stl(triangles: &[[(f32, f32, f32); 3]]) -> Vec<u8> {
        let mut bytes = vec![0u8; 80];
        bytes.extend_from_slice(&(triangles.len() as u32).to_le_bytes());
        for tri in triangles {
            bytes.extend_from_slice(&[0u8; 12]); // normal, unused
            for &(x, y, z) in tri {
                bytes.extend_from_slice(&x.to_le_bytes());
                bytes.extend_from_slice(&y.to_le_bytes());
                bytes.extend_from_slice(&z.to_le_bytes());
            }
            bytes.extend_from_slice(&[0u8; 2]); // attribute byte count
        }
        bytes
    }

    fn one_triangle_stl() -> Vec<u8> {
        build_binary_stl(&[[(0.0, 0.0, 0.0), (5.0, 0.0, 0.0), (0.0, 5.0, 0.0)]])
    }

    fn test_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("plinth_geometry_test_{}_{}", name, std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn mines_and_stores_geometry_keyed_by_bare_hash() {
        let dir = test_dir("mine");
        let path = dir.join("tri.stl");
        fs::write(&path, one_triangle_stl()).unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        db::test_init(&conn);
        let row = FileRow {
            path: path.to_string_lossy().into_owned(),
            dir_path: dir.to_string_lossy().into_owned(),
            file_name: "tri.stl".into(),
            extension: "stl".into(),
            size_bytes: one_triangle_stl().len() as i64,
            modified_at: 100,
            ..Default::default()
        };
        let model = ModelRow {
            dir_path: dir.to_string_lossy().into_owned(),
            name: "test".into(),
            source: "heuristic".into(),
            file_count: 1,
            ..Default::default()
        };
        db::replace_catalog(&mut conn, &dir.to_string_lossy(), &[row], &[model], &[], &[], &[]).unwrap();

        let cancel = AtomicBool::new(false);
        let outcome = mine_geometry(&conn, &cancel, EDGE_STATS_MAX_TRIS, |_, _| {}).unwrap();
        assert_eq!(
            outcome,
            GeometryOutcome {
                mined: 1,
                already_known: 0,
                failed: 0,
            }
        );

        let hash = db::known_hash(&conn, &path.to_string_lossy()).expect("hash stored as a side effect");
        assert!(db::geometry_satisfies(&conn, &hash, EDGE_STATS_MAX_TRIS).unwrap());

        let facts = db::model_geometry(&conn, &dir.to_string_lossy()).unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].file_name, "tri.stl");
        assert_eq!(facts[0].tri_count, 1);
        assert_eq!(facts[0].open_edges, Some(3));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rerun_counts_already_known_without_rereading_the_file() {
        let dir = test_dir("rerun");
        let path = dir.join("tri.stl");
        fs::write(&path, one_triangle_stl()).unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        db::test_init(&conn);
        let row = FileRow {
            path: path.to_string_lossy().into_owned(),
            dir_path: dir.to_string_lossy().into_owned(),
            file_name: "tri.stl".into(),
            extension: "stl".into(),
            size_bytes: one_triangle_stl().len() as i64,
            modified_at: 100,
            ..Default::default()
        };
        let model = ModelRow {
            dir_path: dir.to_string_lossy().into_owned(),
            name: "test".into(),
            source: "heuristic".into(),
            file_count: 1,
            ..Default::default()
        };
        db::replace_catalog(&mut conn, &dir.to_string_lossy(), &[row], &[model], &[], &[], &[]).unwrap();

        let cancel = AtomicBool::new(false);
        let first = mine_geometry(&conn, &cancel, EDGE_STATS_MAX_TRIS, |_, _| {}).unwrap();
        assert_eq!(first.mined, 1);

        // The file is gone; a re-read would surface as `failed`, not
        // `already_known` — proving the second run trusts the stored hash
        // + file_geometry row instead of touching disk again.
        fs::remove_file(&path).unwrap();

        let second = mine_geometry(&conn, &cancel, EDGE_STATS_MAX_TRIS, |_, _| {}).unwrap();
        assert_eq!(
            second,
            GeometryOutcome {
                mined: 0,
                already_known: 1,
                failed: 0,
            }
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn garbage_bytes_count_as_failed_without_aborting_the_run() {
        let dir = test_dir("garbage");
        let bad = dir.join("bad.stl");
        let good = dir.join("good.stl");
        fs::write(&bad, b"not an stl at all, just some junk bytes").unwrap();
        fs::write(&good, one_triangle_stl()).unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        db::test_init(&conn);
        let rows: Vec<FileRow> = [(&bad, "bad.stl"), (&good, "good.stl")]
            .iter()
            .map(|(p, name)| FileRow {
                path: p.to_string_lossy().into_owned(),
                dir_path: dir.to_string_lossy().into_owned(),
                file_name: (*name).into(),
                extension: "stl".into(),
                size_bytes: fs::metadata(p).unwrap().len() as i64,
                modified_at: 100,
                ..Default::default()
            })
            .collect();
        let model = ModelRow {
            dir_path: dir.to_string_lossy().into_owned(),
            name: "test".into(),
            source: "heuristic".into(),
            file_count: 2,
            ..Default::default()
        };
        db::replace_catalog(&mut conn, &dir.to_string_lossy(), &rows, &[model], &[], &[], &[]).unwrap();

        let cancel = AtomicBool::new(false);
        let outcome = mine_geometry(&conn, &cancel, EDGE_STATS_MAX_TRIS, |_, _| {}).unwrap();
        assert_eq!(outcome.failed, 1);
        assert_eq!(outcome.mined, 1);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_row_with_hash_and_existing_geometry_is_skipped_without_disk_access() {
        let dir = test_dir("preseeded");
        // Deliberately never written to disk: a disk read would error, so
        // any success here proves the geometry-known short-circuit fired
        // before fs::File::open was ever reached.
        let path = dir.join("never-written.stl");

        let mut conn = Connection::open_in_memory().unwrap();
        db::test_init(&conn);
        let bare_hash = "deadbeefcafef00d";
        let row = FileRow {
            path: path.to_string_lossy().into_owned(),
            dir_path: dir.to_string_lossy().into_owned(),
            file_name: "never-written.stl".into(),
            extension: "stl".into(),
            size_bytes: 1234,
            modified_at: 100,
            content_hash: Some(bare_hash.to_string()),
            ..Default::default()
        };
        let model = ModelRow {
            dir_path: dir.to_string_lossy().into_owned(),
            name: "test".into(),
            source: "heuristic".into(),
            file_count: 1,
            ..Default::default()
        };
        db::replace_catalog(&mut conn, &dir.to_string_lossy(), &[row], &[model], &[], &[], &[]).unwrap();

        let facts = parse_binary_stl_facts(&one_triangle_stl()).unwrap();
        db::store_file_geometry(&conn, bare_hash, &facts, 100).unwrap();

        let cancel = AtomicBool::new(false);
        let outcome = mine_geometry(&conn, &cancel, EDGE_STATS_MAX_TRIS, |_, _| {}).unwrap();
        assert_eq!(
            outcome,
            GeometryOutcome {
                mined: 0,
                already_known: 1,
                failed: 0,
            }
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn packed_files_are_not_candidates() {
        let dir = test_dir("packed");
        // Packed rows carry an archive_path and no bytes on disk at that
        // path — a real geometry mine must skip them entirely, not fail on
        // them, since "packed" is an expected steady state, not an error.
        let mut conn = Connection::open_in_memory().unwrap();
        db::test_init(&conn);
        let row = FileRow {
            path: dir.join("packed.stl").to_string_lossy().into_owned(),
            dir_path: dir.to_string_lossy().into_owned(),
            file_name: "packed.stl".into(),
            extension: "stl".into(),
            size_bytes: 999,
            modified_at: 100,
            archive_path: Some(dir.join("model.plinthpack").to_string_lossy().into_owned()),
            content_hash: Some("somehash".into()),
        };
        let model = ModelRow {
            dir_path: dir.to_string_lossy().into_owned(),
            name: "test".into(),
            source: "heuristic".into(),
            file_count: 1,
            ..Default::default()
        };
        db::replace_catalog(&mut conn, &dir.to_string_lossy(), &[row], &[model], &[], &[], &[]).unwrap();

        let cancel = AtomicBool::new(false);
        let outcome = mine_geometry(&conn, &cancel, EDGE_STATS_MAX_TRIS, |_, _| {}).unwrap();
        assert_eq!(outcome, GeometryOutcome::default());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn model_geometry_query_is_scoped_to_its_dir() {
        let dir_a = test_dir("model_a");
        let dir_b = test_dir("model_b");
        let a_path = dir_a.join("a.stl");
        let b_path = dir_b.join("b.stl");
        fs::write(&a_path, one_triangle_stl()).unwrap();
        fs::write(&b_path, one_triangle_stl()).unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        db::test_init(&conn);
        let rows = vec![
            FileRow {
                path: a_path.to_string_lossy().into_owned(),
                dir_path: dir_a.to_string_lossy().into_owned(),
                file_name: "a.stl".into(),
                extension: "stl".into(),
                size_bytes: one_triangle_stl().len() as i64,
                modified_at: 100,
                ..Default::default()
            },
            FileRow {
                path: b_path.to_string_lossy().into_owned(),
                dir_path: dir_b.to_string_lossy().into_owned(),
                file_name: "b.stl".into(),
                extension: "stl".into(),
                size_bytes: one_triangle_stl().len() as i64,
                modified_at: 100,
                ..Default::default()
            },
        ];
        let models = vec![
            ModelRow {
                dir_path: dir_a.to_string_lossy().into_owned(),
                name: "a".into(),
                source: "heuristic".into(),
                file_count: 1,
                ..Default::default()
            },
            ModelRow {
                dir_path: dir_b.to_string_lossy().into_owned(),
                name: "b".into(),
                source: "heuristic".into(),
                file_count: 1,
                ..Default::default()
            },
        ];
        db::replace_catalog(&mut conn, "/", &rows, &models, &[], &[], &[]).unwrap();

        let cancel = AtomicBool::new(false);
        mine_geometry(&conn, &cancel, EDGE_STATS_MAX_TRIS, |_, _| {}).unwrap();

        let a_facts = db::model_geometry(&conn, &dir_a.to_string_lossy()).unwrap();
        assert_eq!(a_facts.len(), 1);
        assert_eq!(a_facts[0].file_name, "a.stl");

        let b_facts = db::model_geometry(&conn, &dir_b.to_string_lossy()).unwrap();
        assert_eq!(b_facts.len(), 1);
        assert_eq!(b_facts[0].file_name, "b.stl");

        fs::remove_dir_all(&dir_a).ok();
        fs::remove_dir_all(&dir_b).ok();
    }

    /// A 10mm axis-aligned cube's 12 outward-wound triangles, binary-STL
    /// encoded — mirrors stl_facts::tests::cube_triangles (duplicated
    /// locally since that helper is private to its own module). Only its
    /// triangle count (12, bigger than a cap of 1) matters to the tests
    /// below, not its exact geometry.
    fn cube_stl() -> Vec<u8> {
        let a = (0.0, 0.0, 0.0);
        let b = (10.0, 0.0, 0.0);
        let c = (10.0, 10.0, 0.0);
        let d = (0.0, 10.0, 0.0);
        let e = (0.0, 0.0, 10.0);
        let f = (10.0, 0.0, 10.0);
        let g = (10.0, 10.0, 10.0);
        let h = (0.0, 10.0, 10.0);
        build_binary_stl(&[
            [a, c, b],
            [a, d, c],
            [e, f, g],
            [e, g, h],
            [a, b, f],
            [a, f, e],
            [d, g, c],
            [d, h, g],
            [a, h, d],
            [a, e, h],
            [b, c, g],
            [b, g, f],
        ])
    }

    #[test]
    fn raising_the_cap_and_rerunning_backfills_a_capped_rows_edge_stats() {
        let dir = test_dir("raise_cap");
        let path = dir.join("cube.stl");
        fs::write(&path, cube_stl()).unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        db::test_init(&conn);
        let row = FileRow {
            path: path.to_string_lossy().into_owned(),
            dir_path: dir.to_string_lossy().into_owned(),
            file_name: "cube.stl".into(),
            extension: "stl".into(),
            size_bytes: cube_stl().len() as i64,
            modified_at: 100,
            ..Default::default()
        };
        let model = ModelRow {
            dir_path: dir.to_string_lossy().into_owned(),
            name: "test".into(),
            source: "heuristic".into(),
            file_count: 1,
            ..Default::default()
        };
        db::replace_catalog(&mut conn, &dir.to_string_lossy(), &[row], &[model], &[], &[], &[]).unwrap();

        let cancel = AtomicBool::new(false);
        // 12 triangles > cap of 1: edge stats skipped, the row stores NULL.
        let first = mine_geometry(&conn, &cancel, 1, |_, _| {}).unwrap();
        assert_eq!(first.mined, 1);

        let hash = db::known_hash(&conn, &path.to_string_lossy()).expect("hash stored");
        let facts = db::model_geometry(&conn, &dir.to_string_lossy()).unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].open_edges, None);
        assert!(!db::geometry_satisfies(&conn, &hash, 1000).unwrap());

        // Same hash, same file still on disk, cap now covers all 12
        // triangles: the stored NULL row doesn't satisfy the new cap, so it
        // re-streams and replaces the row — counted `mined`, not
        // `already_known`.
        let second = mine_geometry(&conn, &cancel, 1000, |_, _| {}).unwrap();
        assert_eq!(
            second,
            GeometryOutcome {
                mined: 1,
                already_known: 0,
                failed: 0,
            }
        );

        let facts = db::model_geometry(&conn, &dir.to_string_lossy()).unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].open_edges, Some(0));
        assert!(db::geometry_satisfies(&conn, &hash, 1000).unwrap());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_complete_row_is_still_skipped_no_matter_how_large_the_cap() {
        let dir = test_dir("complete_row_any_cap");
        let path = dir.join("tri.stl");
        fs::write(&path, one_triangle_stl()).unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        db::test_init(&conn);
        let row = FileRow {
            path: path.to_string_lossy().into_owned(),
            dir_path: dir.to_string_lossy().into_owned(),
            file_name: "tri.stl".into(),
            extension: "stl".into(),
            size_bytes: one_triangle_stl().len() as i64,
            modified_at: 100,
            ..Default::default()
        };
        let model = ModelRow {
            dir_path: dir.to_string_lossy().into_owned(),
            name: "test".into(),
            source: "heuristic".into(),
            file_count: 1,
            ..Default::default()
        };
        db::replace_catalog(&mut conn, &dir.to_string_lossy(), &[row], &[model], &[], &[], &[]).unwrap();

        let cancel = AtomicBool::new(false);
        let first = mine_geometry(&conn, &cancel, EDGE_STATS_MAX_TRIS, |_, _| {}).unwrap();
        assert_eq!(first.mined, 1);

        // File gone: any disk read would surface as `failed`, so
        // `already_known` here proves a much larger cap on rerun didn't
        // force a re-read of a row that already has open_edges populated.
        fs::remove_file(&path).unwrap();

        let second = mine_geometry(&conn, &cancel, 30_000_000, |_, _| {}).unwrap();
        assert_eq!(
            second,
            GeometryOutcome {
                mined: 0,
                already_known: 1,
                failed: 0,
            }
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn recommended_edge_cap_for_never_drops_below_the_floor() {
        assert_eq!(recommended_edge_cap_for(0), EDGE_STATS_MAX_TRIS);
        assert_eq!(recommended_edge_cap_for(1), EDGE_STATS_MAX_TRIS);
        // 1 GiB: 1 GiB / 8 / 150 bytes/tri ~= 895K tris, under the floor.
        let one_gib = 1024u64 * 1024 * 1024;
        assert_eq!(recommended_edge_cap_for(one_gib), EDGE_STATS_MAX_TRIS);
    }

    #[test]
    fn recommended_edge_cap_for_never_exceeds_the_ceiling() {
        let gib = 1024u64 * 1024 * 1024;
        assert_eq!(
            recommended_edge_cap_for(64 * gib),
            RECOMMENDED_EDGE_CAP_CEILING
        );
        assert_eq!(recommended_edge_cap_for(u64::MAX), RECOMMENDED_EDGE_CAP_CEILING);
    }

    #[test]
    fn recommended_edge_cap_for_is_monotone_in_ram() {
        let gib = 1024u64 * 1024 * 1024;
        let sizes_gib = [1, 2, 4, 8, 16, 32, 64, 128];
        let caps: Vec<u32> = sizes_gib
            .iter()
            .map(|gb| recommended_edge_cap_for(gb * gib))
            .collect();
        for pair in caps.windows(2) {
            assert!(pair[0] <= pair[1], "caps not monotone in RAM: {:?}", caps);
        }
    }

    /// Pins the doc comment's worked examples (8 GB -> ~7M, 16 GB -> ~14M,
    /// 64 GB+ -> the 30M ceiling) so the constants and the prose can't
    /// silently drift apart.
    #[test]
    fn recommended_edge_cap_for_matches_the_documented_worked_examples() {
        let gib = 1024u64 * 1024 * 1024;

        let eight_gb = recommended_edge_cap_for(8 * gib);
        assert!((6_900_000..=7_300_000).contains(&eight_gb), "got {eight_gb}");

        let sixteen_gb = recommended_edge_cap_for(16 * gib);
        assert!(
            (14_000_000..=14_600_000).contains(&sixteen_gb),
            "got {sixteen_gb}"
        );

        assert_eq!(
            recommended_edge_cap_for(64 * gib),
            RECOMMENDED_EDGE_CAP_CEILING
        );
    }
}
