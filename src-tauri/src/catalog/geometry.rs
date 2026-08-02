//! Backend mining stage of issue #15: turns each loose STL the catalog
//! already knows about into a row of stl_facts (bbox, volume, open-edge
//! count) in `file_geometry`, keyed by the file's BARE blake3 content hash
//! so mining any given set of bytes ever happens once, no matter how many
//! catalog paths (duplicates, re-scans) point at them.
//!
//! Deliberately mirrors catalog::dups::find_duplicates in shape (a
//! `Connection` + cancel flag + progress callback, called from a
//! spawn_blocking job in commands.rs) — mining and duplicate detection are
//! both "walk the index, maybe touch disk once per candidate" passes over
//! the same `files` table, just computing different facts.
//!
//! Staleness contract (same as the dup scanner's): candidates and their
//! known hashes come from the files INDEX, not a fresh disk walk. A file
//! edited on disk is picked up on the next catalog scan — replace_catalog
//! only re-attaches a stored content_hash when path+size+mtime all still
//! match, so a changed file re-enters mining hash-less and gets re-read,
//! re-hashed, and re-mined under its new hash. Facts rows themselves can
//! never go stale: keyed by content hash, same bytes ⇒ same geometry,
//! forever. Rescan first, then mine.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use crate::error::AppError;

use super::db;
use super::stl_facts::{parse_header, FactsAccumulator, StlFacts, COUNT_LEN, HEADER_LEN, RECORD_LEN};

/// Bytes streamed per read while mining: a whole number of 50-byte records
/// (~800 KiB) so record parsing never straddles a chunk boundary. This is
/// the mining pass's ENTIRE per-file memory footprint no matter how large
/// the file — a 4 GiB display bust streams through the same buffer as a
/// 4 MB mini. (There is deliberately no file-size cap: a size cutoff would
/// make big files silently vanish from the geometry report, which reads as
/// "file missing" in the drawer. The only bounded-by-triangle-count cost is
/// stl_facts' edge map, which caps itself.)
const STREAM_CHUNK_BYTES: usize = RECORD_LEN * 16 * 1024;

/// Tally of one mine_geometry pass, reported in GeometryStatus::Completed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GeometryOutcome {
    /// Newly parsed and stored this run.
    pub mined: u32,
    /// Skipped without a disk read: this content hash already had a
    /// file_geometry row (from an earlier mining run, possibly under a
    /// different path/name).
    pub already_known: u32,
    /// Unreadable (missing file, permission error) or unparsable (not a
    /// well-formed binary STL) — one bad file never aborts the run.
    pub failed: u32,
}

/// One candidate, streamed once: every byte goes through the blake3 hasher,
/// and — when the byte length matches the header's declared triangle count —
/// through a FactsAccumulator in the same pass.
///
/// Outer Err (io): the file couldn't be read completely, or its size changed
/// mid-read (bytes hashed != size at open) — no hash is trustworthy, the
/// caller stores nothing. Inner Err: the bytes are fine (hash valid, worth
/// storing for dup detection) but they aren't a well-formed binary STL.
fn stream_mine(path: &str) -> std::io::Result<(String, Result<StlFacts, String>)> {
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
        Some(FactsAccumulator::new(tri_count).expect("tri_count checked non-zero"))
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

/// Mine bbox/volume/open-edge facts for every loose (unpacked) STL the
/// catalog's `files` table knows about.
///
/// For each candidate:
///   - if its files row already carries a content_hash AND file_geometry
///     already has that hash, it's `already_known` — no disk access at all;
///   - otherwise the file is STREAMED once through a fixed ~800 KiB buffer
///     (see stream_mine — no size limit, no whole-file allocation), hashing
///     and fact-accumulating in the same pass. A files row that lacked a
///     content_hash gets one stored via `db::store_hash` as a side effect —
///     free duplicate-detection fodder, since a later dup scan then skips a
///     disk read for this path entirely;
///   - if that hash turns out to already be known (a duplicate of a file
///     mined earlier this same run, or one whose files row hadn't recorded
///     the hash yet), it's `already_known` too;
///   - a parse failure counts as `failed` and mining continues — the point
///     of a mining pass is to harvest what it can, not to guarantee every
///     file succeeds.
///
/// `cancel` is checked once per candidate, matching find_duplicates; on
/// cancellation the same `AppError::UserCancelled` variant is returned so
/// callers (see commands::start_geometry_scan) route it to Cancelled instead
/// of Failed exactly like every other catalog job.
pub fn mine_geometry(
    conn: &Connection,
    cancel: &AtomicBool,
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
            if db::geometry_exists(conn, hash)? {
                outcome.already_known += 1;
                continue;
            }
        }

        let (hash, parsed) = match stream_mine(&path) {
            Ok(streamed) => streamed,
            Err(_) => {
                outcome.failed += 1;
                continue;
            }
        };
        if known_hash.is_none() {
            db::store_hash(conn, &path, &hash)?;
        }
        if db::geometry_exists(conn, &hash)? {
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
        let outcome = mine_geometry(&conn, &cancel, |_, _| {}).unwrap();
        assert_eq!(
            outcome,
            GeometryOutcome {
                mined: 1,
                already_known: 0,
                failed: 0,
            }
        );

        let hash = db::known_hash(&conn, &path.to_string_lossy()).expect("hash stored as a side effect");
        assert!(db::geometry_exists(&conn, &hash).unwrap());

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
        let first = mine_geometry(&conn, &cancel, |_, _| {}).unwrap();
        assert_eq!(first.mined, 1);

        // The file is gone; a re-read would surface as `failed`, not
        // `already_known` — proving the second run trusts the stored hash
        // + file_geometry row instead of touching disk again.
        fs::remove_file(&path).unwrap();

        let second = mine_geometry(&conn, &cancel, |_, _| {}).unwrap();
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
        let outcome = mine_geometry(&conn, &cancel, |_, _| {}).unwrap();
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
        let outcome = mine_geometry(&conn, &cancel, |_, _| {}).unwrap();
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
        let outcome = mine_geometry(&conn, &cancel, |_, _| {}).unwrap();
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
        mine_geometry(&conn, &cancel, |_, _| {}).unwrap();

        let a_facts = db::model_geometry(&conn, &dir_a.to_string_lossy()).unwrap();
        assert_eq!(a_facts.len(), 1);
        assert_eq!(a_facts[0].file_name, "a.stl");

        let b_facts = db::model_geometry(&conn, &dir_b.to_string_lossy()).unwrap();
        assert_eq!(b_facts.len(), 1);
        assert_eq!(b_facts[0].file_name, "b.stl");

        fs::remove_dir_all(&dir_a).ok();
        fs::remove_dir_all(&dir_b).ok();
    }
}
