//! A tiny, pure binary-STL bounding-box reader — no Blender, no full mesh
//! parse. Reads only the vertex coordinates needed for a bounding box: an
//! 80-byte header (ignored), a little-endian u32 triangle count, then that
//! many 50-byte records (12 bytes normal + 3x12 bytes vertex + 2 bytes
//! attribute byte count, all little-endian f32/u16).
//!
//! ASCII STL is deliberately NOT supported: it has no fixed record size to
//! bound the read against, and every STL this app itself exports is
//! binary — an ASCII file here is already an out-of-band asset, rejected
//! the same as any other unparseable file.

/// A bounding box in the STL's own coordinate space (mm, by this app's
/// universal convention — see scatter_landscape.py's module docstring).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StlBBox {
    pub min: (f32, f32, f32),
    pub max: (f32, f32, f32),
}

impl StlBBox {
    /// The planar footprint: the LARGER of the X/Y extents, matching the
    /// single `footprint_mm: f64` field the curation manifest records per
    /// piece (no separate width/depth pair).
    pub fn footprint_mm(&self) -> f64 {
        let dx = (self.max.0 - self.min.0) as f64;
        let dy = (self.max.1 - self.min.1) as f64;
        dx.max(dy)
    }

    /// Z extent — this app's Z-up convention, so height is always the
    /// bbox's Z span regardless of how the piece is centered/floored.
    pub fn height_mm(&self) -> f64 {
        (self.max.2 - self.min.2) as f64
    }
}

const HEADER_LEN: usize = 80;
const COUNT_LEN: usize = 4;
const RECORD_LEN: usize = 50; // 12 (normal) + 3*12 (verts) + 2 (attr byte count)

/// Parse a binary STL's header + triangle records into a bounding box.
/// Pure function of the bytes — no filesystem access, so it's directly
/// unit-testable against hand-built byte arrays (no fixture file needed).
///
/// Rejects (with a human-readable reason, not a panic):
///   - anything shorter than the 84-byte header+count
///   - a byte length that doesn't match `84 + triangle_count * 50` exactly
///     (covers both a truncated/corrupt file and an ASCII STL, which has no
///     reason to land on this exact formula)
///   - a triangle count of zero (an STL with no geometry has no bbox)
pub fn parse_binary_stl_bbox(bytes: &[u8]) -> Result<StlBBox, String> {
    if bytes.len() < HEADER_LEN + COUNT_LEN {
        return Err(format!(
            "file is only {} bytes — too short for a binary STL's 84-byte header+count",
            bytes.len()
        ));
    }
    let count_bytes: [u8; 4] = bytes[HEADER_LEN..HEADER_LEN + COUNT_LEN]
        .try_into()
        .expect("slice of exactly 4 bytes");
    let tri_count = u32::from_le_bytes(count_bytes) as usize;
    if tri_count == 0 {
        return Err("binary STL header reports zero triangles".to_string());
    }

    let expected_len = HEADER_LEN + COUNT_LEN + tri_count * RECORD_LEN;
    if bytes.len() != expected_len {
        return Err(format!(
            "byte length {} does not match the {} triangles the header declares \
             (expected exactly {} bytes) — not a well-formed binary STL, or an ASCII STL",
            bytes.len(),
            tri_count,
            expected_len
        ));
    }

    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    let mut offset = HEADER_LEN + COUNT_LEN;
    for _ in 0..tri_count {
        offset += 12; // skip the facet normal
        for _vertex in 0..3 {
            for axis in 0..3 {
                let coord_bytes: [u8; 4] = bytes[offset..offset + 4]
                    .try_into()
                    .expect("slice of exactly 4 bytes");
                let value = f32::from_le_bytes(coord_bytes);
                if value < min[axis] {
                    min[axis] = value;
                }
                if value > max[axis] {
                    max[axis] = value;
                }
                offset += 4;
            }
        }
        offset += 2; // attribute byte count
    }

    Ok(StlBBox {
        min: (min[0], min[1], min[2]),
        max: (max[0], max[1], max[2]),
    })
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

    #[test]
    fn parses_a_single_triangle_bbox() {
        let bytes = build_binary_stl(&[[(0.0, 0.0, 0.0), (4.0, 0.0, 0.0), (0.0, 3.0, 1.5)]]);
        let bbox = parse_binary_stl_bbox(&bytes).expect("well-formed single-triangle STL");
        assert_eq!(bbox.min, (0.0, 0.0, 0.0));
        assert_eq!(bbox.max, (4.0, 3.0, 1.5));
        assert_eq!(bbox.footprint_mm(), 4.0); // max(4.0, 3.0)
        assert_eq!(bbox.height_mm(), 1.5);
    }

    #[test]
    fn parses_multiple_triangles_and_unions_their_bboxes() {
        let bytes = build_binary_stl(&[
            [(-2.0, -1.0, 0.0), (2.0, -1.0, 0.0), (0.0, 1.0, 0.0)],
            [(0.0, 0.0, 0.0), (0.0, 0.0, 5.0), (1.0, 1.0, 5.0)],
        ]);
        let bbox = parse_binary_stl_bbox(&bytes).expect("well-formed two-triangle STL");
        assert_eq!(bbox.min, (-2.0, -1.0, 0.0));
        assert_eq!(bbox.max, (2.0, 1.0, 5.0));
        assert_eq!(bbox.footprint_mm(), 4.0); // max(4.0 wide, 2.0 deep)
        assert_eq!(bbox.height_mm(), 5.0);
    }

    #[test]
    fn footprint_is_the_larger_of_x_and_y_not_a_diagonal() {
        // A long, narrow piece (e.g. a bone) — footprint reads as the long
        // axis, matching how the curated manifest itself records "picked"
        // sizes (see this module's doc comment).
        let bytes = build_binary_stl(&[[(0.0, 0.0, 0.0), (16.0, 0.0, 0.0), (0.0, 3.0, 0.0)]]);
        let bbox = parse_binary_stl_bbox(&bytes).unwrap();
        assert_eq!(bbox.footprint_mm(), 16.0);
    }

    #[test]
    fn rejects_a_file_too_short_for_the_header() {
        let err = parse_binary_stl_bbox(&[0u8; 10]).unwrap_err();
        assert!(err.contains("too short"), "got: {err}");
    }

    #[test]
    fn rejects_a_zero_triangle_header() {
        let mut bytes = vec![0u8; HEADER_LEN];
        bytes.extend_from_slice(&0u32.to_le_bytes());
        let err = parse_binary_stl_bbox(&bytes).unwrap_err();
        assert!(err.contains("zero triangles"), "got: {err}");
    }

    #[test]
    fn rejects_a_byte_length_mismatch_truncated_binary() {
        let mut bytes = build_binary_stl(&[[(0.0, 0.0, 0.0), (1.0, 0.0, 0.0), (0.0, 1.0, 0.0)]]);
        bytes.truncate(bytes.len() - 10); // corrupt: header claims 1 tri, body is short
        let err = parse_binary_stl_bbox(&bytes).unwrap_err();
        assert!(err.contains("does not match"), "got: {err}");
    }

    #[test]
    fn rejects_an_ascii_stl() {
        let ascii = b"solid test\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\n\
                       vertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid test\n";
        let err = parse_binary_stl_bbox(ascii).unwrap_err();
        // Not a crash, and (since ASCII bytes coincidentally never satisfy
        // the exact-length formula) rejected as a shape mismatch.
        assert!(!err.is_empty());
    }
}
