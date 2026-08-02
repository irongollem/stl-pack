//! Geometry facts from binary STL triangle records, accumulated
//! incrementally so the miner can stream instead of buffering whole files.

use std::collections::HashMap;

/// Facts derived from one binary STL in a single pass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StlFacts {
    pub tri_count: u32,
    pub min: (f32, f32, f32),
    pub max: (f32, f32, f32),
    /// abs of the signed tetrahedron sum; ~0 for open sheets.
    pub volume_mm3: f64,
    /// Edges not shared by exactly two triangles; None above the edge cap.
    pub open_edge_count: Option<u32>,
}

// The floor every runtime edge cap clamps to: no setting may regress
// mining below what an uncapped build handled, and no mesh may force an
// unbounded edge map (~150 B/triangle worst case).
pub const EDGE_STATS_MAX_TRIS: u32 = 1_500_000;

pub(crate) const HEADER_LEN: usize = 80;
pub(crate) const COUNT_LEN: usize = 4;
pub(crate) const RECORD_LEN: usize = 50; // 12 (normal) + 3*12 (verts) + 2 (attr byte count)

// Keyed on the raw f32 bits, not the values: NaN != NaN and -0.0/0.0
// differ, so a naive == would split or merge vertices.
type VertexKey = (u32, u32, u32);

// Canonicalized (min, max) so the two adjacent triangles walking a shared
// edge in opposite directions count the same edge.
type EdgeKey = (VertexKey, VertexKey);

fn edge_key(a: VertexKey, b: VertexKey) -> EdgeKey {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

pub struct FactsAccumulator {
    tri_count: u32,
    min: [f32; 3],
    max: [f32; 3],
    volume_acc: f64,
    // None when tri_count exceeds the cap: no per-edge allocation at all,
    // not even an empty map, so the skip actually bounds memory.
    edge_counts: Option<HashMap<EdgeKey, u32>>,
}

impl FactsAccumulator {
    /// The caller validates byte length against `tri_count` and pushes
    /// exactly that many records; `edge_cap` gates the edge map only.
    pub(crate) fn with_cap(tri_count: u32, edge_cap: u32) -> Result<Self, String> {
        if tri_count == 0 {
            return Err("binary STL header reports zero triangles".to_string());
        }
        Ok(Self {
            tri_count,
            min: [f32::INFINITY; 3],
            max: [f32::NEG_INFINITY; 3],
            volume_acc: 0.0,
            edge_counts: (tri_count <= edge_cap).then(HashMap::new),
        })
    }

    pub fn push_record(&mut self, record: &[u8; RECORD_LEN]) {
        let mut offset = 12; // skip the facet normal

        let mut verts_f32 = [[0.0f32; 3]; 3];
        let mut verts_key = [(0u32, 0u32, 0u32); 3];
        for (v, key) in verts_f32.iter_mut().zip(verts_key.iter_mut()) {
            let mut raw = [0u32; 3];
            for (axis, raw_axis) in raw.iter_mut().enumerate() {
                let coord_bytes: [u8; 4] = record[offset..offset + 4]
                    .try_into()
                    .expect("slice of exactly 4 bytes");
                let value = f32::from_le_bytes(coord_bytes);
                if value < self.min[axis] {
                    self.min[axis] = value;
                }
                if value > self.max[axis] {
                    self.max[axis] = value;
                }
                v[axis] = value;
                *raw_axis = u32::from_le_bytes(coord_bytes);
                offset += 4;
            }
            *key = (raw[0], raw[1], raw[2]);
        }

        let v0 = verts_f32[0].map(f64::from);
        let v1 = verts_f32[1].map(f64::from);
        let v2 = verts_f32[2].map(f64::from);
        let cross = [
            v1[1] * v2[2] - v1[2] * v2[1],
            v1[2] * v2[0] - v1[0] * v2[2],
            v1[0] * v2[1] - v1[1] * v2[0],
        ];
        let dot = v0[0] * cross[0] + v0[1] * cross[1] + v0[2] * cross[2];
        self.volume_acc += dot / 6.0;

        if let Some(counts) = self.edge_counts.as_mut() {
            // A degenerate triangle's repeated vertex yields duplicate edge
            // keys; it counts as extra open edges, which is honest.
            for &(a, b) in &[
                (verts_key[0], verts_key[1]),
                (verts_key[1], verts_key[2]),
                (verts_key[2], verts_key[0]),
            ] {
                *counts.entry(edge_key(a, b)).or_insert(0) += 1;
            }
        }
    }

    pub fn finish(self) -> StlFacts {
        let open_edge_count = self
            .edge_counts
            .map(|counts| counts.values().filter(|&&count| count != 2).count() as u32);
        StlFacts {
            tri_count: self.tri_count,
            min: (self.min[0], self.min[1], self.min[2]),
            max: (self.max[0], self.max[1], self.max[2]),
            volume_mm3: self.volume_acc.abs(),
            open_edge_count,
        }
    }
}

pub(crate) fn parse_header(preamble: &[u8]) -> Result<u32, String> {
    if preamble.len() < HEADER_LEN + COUNT_LEN {
        return Err(format!(
            "file is only {} bytes — too short for a binary STL's 84-byte header+count",
            preamble.len()
        ));
    }
    let count_bytes: [u8; 4] = preamble[HEADER_LEN..HEADER_LEN + COUNT_LEN]
        .try_into()
        .expect("slice of exactly 4 bytes");
    Ok(u32::from_le_bytes(count_bytes))
}

/// Test-only whole-slice wrapper: production mining streams through
/// FactsAccumulator; this is how tests exercise the same math.
#[cfg(test)]
pub fn parse_binary_stl_facts(bytes: &[u8]) -> Result<StlFacts, String> {
    parse_binary_stl_facts_with_cap(bytes, EDGE_STATS_MAX_TRIS)
}

// Cap passed explicitly so tests hit the boundary with a tiny value
// instead of a multi-million-triangle fixture.
#[cfg(test)]
fn parse_binary_stl_facts_with_cap(bytes: &[u8], edge_cap: u32) -> Result<StlFacts, String> {
    let tri_count = parse_header(bytes)?;
    if tri_count == 0 {
        return Err("binary STL header reports zero triangles".to_string());
    }

    let expected_len = HEADER_LEN + COUNT_LEN + tri_count as usize * RECORD_LEN;
    if bytes.len() != expected_len {
        return Err(format!(
            "byte length {} does not match the {} triangles the header declares \
             (expected exactly {} bytes) — not a well-formed binary STL, or an ASCII STL",
            bytes.len(),
            tri_count,
            expected_len
        ));
    }

    let mut acc = FactsAccumulator::with_cap(tri_count, edge_cap)?;
    let mut offset = HEADER_LEN + COUNT_LEN;
    for _ in 0..tri_count {
        let record: &[u8; RECORD_LEN] = bytes[offset..offset + RECORD_LEN]
            .try_into()
            .expect("slice of exactly RECORD_LEN bytes");
        acc.push_record(record);
        offset += RECORD_LEN;
    }
    Ok(acc.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal well-formed binary STL: an 80-byte header, a
    /// triangle count, then `triangles` raw 9-float (3 vertices) records —
    /// normals and the attribute byte count are zeroed since the parser
    /// never reads them.
    fn build_binary_stl(triangles: &[[(f32, f32, f32); 3]]) -> Vec<u8> {
        let mut bytes = vec![0u8; HEADER_LEN];
        bytes.extend_from_slice(&(triangles.len() as u32).to_le_bytes());
        for tri in triangles {
            bytes.extend_from_slice(&[0u8; 12]); // normal, unused by the parser
            for &(x, y, z) in tri {
                bytes.extend_from_slice(&x.to_le_bytes());
                bytes.extend_from_slice(&y.to_le_bytes());
                bytes.extend_from_slice(&z.to_le_bytes());
            }
            bytes.extend_from_slice(&[0u8; 2]); // attribute byte count, unused
        }
        bytes
    }

    // A 10mm axis-aligned cube, corners named for face-building below.
    // Each face is wound outward via the right-hand rule (checked by hand:
    // normal = cross(v1-v0, v2-v0) points away from the cube's center).
    const A: (f32, f32, f32) = (0.0, 0.0, 0.0);
    const B: (f32, f32, f32) = (10.0, 0.0, 0.0);
    const C: (f32, f32, f32) = (10.0, 10.0, 0.0);
    const D: (f32, f32, f32) = (0.0, 10.0, 0.0);
    const E: (f32, f32, f32) = (0.0, 0.0, 10.0);
    const F: (f32, f32, f32) = (10.0, 0.0, 10.0);
    const G: (f32, f32, f32) = (10.0, 10.0, 10.0);
    const H: (f32, f32, f32) = (0.0, 10.0, 10.0);

    /// The 12 outward-wound triangles of the A..H cube above: bottom, top,
    /// front (y=0), back (y=10), left (x=0), right (x=10) — two triangles
    /// each, verified by hand to have outward-pointing normals.
    fn cube_triangles() -> Vec<[(f32, f32, f32); 3]> {
        vec![
            [A, C, B], // bottom, -z
            [A, D, C],
            [E, F, G], // top, +z
            [E, G, H],
            [A, B, F], // front, -y
            [A, F, E],
            [D, G, C], // back, +y
            [D, H, G],
            [A, H, D], // left, -x
            [A, E, H],
            [B, C, G], // right, +x
            [B, G, F],
        ]
    }

    fn invert(tri: [(f32, f32, f32); 3]) -> [(f32, f32, f32); 3] {
        [tri[0], tri[2], tri[1]]
    }

    #[test]
    fn closed_cube_reports_volume_bbox_and_zero_open_edges() {
        let bytes = build_binary_stl(&cube_triangles());
        let facts = parse_binary_stl_facts(&bytes).expect("well-formed cube STL");
        assert_eq!(facts.tri_count, 12);
        assert_eq!(facts.min, (0.0, 0.0, 0.0));
        assert_eq!(facts.max, (10.0, 10.0, 10.0));
        assert!(
            (facts.volume_mm3 - 1000.0).abs() < 1e-3,
            "got {}",
            facts.volume_mm3
        );
        assert_eq!(facts.open_edge_count, Some(0));
    }

    #[test]
    fn cube_with_one_face_removed_has_four_open_edges() {
        // Drop the two top-face triangles, opening a square hole. The
        // shared internal diagonal (E-G) simply vanishes from the map
        // (it had no other owner), so only the hole's four rim edges — each
        // now touched by just its one remaining side-wall triangle — come
        // out with count 1. Volume of an open mesh is a best-effort signed
        // sum (the divergence-theorem trick assumes a closed surface), not
        // a physically meaningful number — this test only asserts it stays
        // finite, not any particular value.
        let mut triangles = cube_triangles();
        triangles.remove(3); // [E, G, H]
        triangles.remove(2); // [E, F, G]
        let bytes = build_binary_stl(&triangles);
        let facts = parse_binary_stl_facts(&bytes).expect("well-formed open-top STL");
        assert_eq!(facts.tri_count, 10);
        assert_eq!(facts.open_edge_count, Some(4));
        assert!(facts.volume_mm3.is_finite());
    }

    #[test]
    fn single_triangle_through_the_origin_has_zero_volume_and_three_open_edges() {
        // v0 is the origin, so dot(v0, cross(v1, v2)) is exactly 0 — the
        // single term in the sum vanishes regardless of the other two
        // vertices, giving an exact (not just approximate) zero.
        let bytes = build_binary_stl(&[[(0.0, 0.0, 0.0), (5.0, 0.0, 0.0), (0.0, 5.0, 0.0)]]);
        let facts = parse_binary_stl_facts(&bytes).expect("well-formed single-triangle STL");
        assert_eq!(facts.tri_count, 1);
        assert_eq!(facts.volume_mm3, 0.0);
        assert_eq!(facts.open_edge_count, Some(3));
    }

    #[test]
    fn inverted_winding_still_reports_correct_abs_volume_and_zero_open_edges() {
        // Flip every triangle's winding (v1/v2 swapped): the signed sum
        // flips sign, but volume_mm3 is an abs() and edge counting keys on
        // unordered vertex pairs, so both facts come out unchanged.
        let inverted: Vec<_> = cube_triangles().into_iter().map(invert).collect();
        let bytes = build_binary_stl(&inverted);
        let facts = parse_binary_stl_facts(&bytes).expect("well-formed inverted cube STL");
        assert!(
            (facts.volume_mm3 - 1000.0).abs() < 1e-3,
            "got {}",
            facts.volume_mm3
        );
        assert_eq!(facts.open_edge_count, Some(0));
    }

    #[test]
    fn mixed_winding_on_a_closed_cube_still_reports_zero_open_edges() {
        // Half the faces wound one way, half the other — a real mesh would
        // never do this, but it proves edge counting is undirected.
        let mut triangles = cube_triangles();
        for tri in triangles.iter_mut().step_by(2) {
            *tri = invert(*tri);
        }
        let bytes = build_binary_stl(&triangles);
        let facts = parse_binary_stl_facts(&bytes).expect("well-formed mixed-winding cube STL");
        assert_eq!(facts.open_edge_count, Some(0));
    }

    #[test]
    fn rejects_a_file_too_short_for_the_header() {
        let err = parse_binary_stl_facts(&[0u8; 10]).unwrap_err();
        assert!(err.contains("too short"), "got: {err}");
    }

    #[test]
    fn rejects_a_zero_triangle_header() {
        let mut bytes = vec![0u8; HEADER_LEN];
        bytes.extend_from_slice(&0u32.to_le_bytes());
        let err = parse_binary_stl_facts(&bytes).unwrap_err();
        assert!(err.contains("zero triangles"), "got: {err}");
    }

    #[test]
    fn rejects_a_byte_length_mismatch_truncated_binary() {
        let mut bytes = build_binary_stl(&[[(0.0, 0.0, 0.0), (1.0, 0.0, 0.0), (0.0, 1.0, 0.0)]]);
        bytes.truncate(bytes.len() - 10); // corrupt: header claims 1 tri, body is short
        let err = parse_binary_stl_facts(&bytes).unwrap_err();
        assert!(err.contains("does not match"), "got: {err}");
    }

    #[test]
    fn rejects_an_ascii_stl() {
        let ascii = b"solid test\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\n\
                       vertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid test\n";
        let err = parse_binary_stl_facts(ascii).unwrap_err();
        // Not a crash, and (since ASCII bytes coincidentally never satisfy
        // the exact-length formula) rejected as a shape mismatch.
        assert!(!err.is_empty());
    }

    #[test]
    fn edge_stats_are_skipped_above_the_cap_but_bbox_and_volume_still_run() {
        // A tiny cap (1) against a 2-triangle STL exercises the skip path
        // without needing a real multi-million-triangle fixture.
        let triangles = vec![
            [(0.0, 0.0, 0.0), (5.0, 0.0, 0.0), (0.0, 5.0, 0.0)],
            [(0.0, 0.0, 0.0), (0.0, 5.0, 0.0), (5.0, 0.0, 0.0)],
        ];
        let bytes = build_binary_stl(&triangles);
        let facts = parse_binary_stl_facts_with_cap(&bytes, 1).expect("well-formed two-tri STL");
        assert_eq!(facts.tri_count, 2);
        assert_eq!(facts.open_edge_count, None);
        assert_eq!(facts.min, (0.0, 0.0, 0.0));
        assert_eq!(facts.max, (5.0, 5.0, 0.0));

        // Same STL, cap raised to cover it: edge stats come back.
        let facts_uncapped =
            parse_binary_stl_facts_with_cap(&bytes, 2).expect("well-formed two-tri STL");
        assert!(facts_uncapped.open_edge_count.is_some());
    }
}
