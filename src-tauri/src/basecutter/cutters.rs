//! Cutter data model — see docs/BASECUTTER.md "Cutters are data, not code"
//! and "The plinth". This module owns the shapes, the seed library, and the
//! nominal->cut derivation. Nothing here may assume the `CutterKind` list
//! stays closed — it's meant to grow.

use serde::{Deserialize, Serialize};
use specta::Type;

/// Cutter footprint shapes. Internally tagged on `kind` with lowercase
/// variant names so the JSON matches `base_cut.py`'s job format verbatim
/// (`{"kind": "circle", "diameter_mm": 32.0}` — see its top docstring and
/// docs/BASECUTTER.md "Pinned interfaces"). Open for extension: a
/// user-traced outline is a later variant, not a rewrite of this type.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum CutterKind {
    Circle { diameter_mm: f64 },
    Ellipse { major_mm: f64, minor_mm: f64 },
    Rect { width_mm: f64, depth_mm: f64 },
}

/// A magnet as it will be pocketed into a plinth's boss. Drawn from the
/// user's magnet inventory (app settings), never from a hardcoded
/// base-size->magnet table — pairing is a suggestion rule over inventory.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct MagnetSpec {
    pub diameter_mm: f64,
    pub height_mm: f64,
    pub count: u32,
}

/// Tapered plinth profile. Defaults are caliper-measured off a real 32 mm
/// round base (32 -> 30 mm over 3.7 mm tall, 1.2 mm wall — see
/// docs/BASECUTTER.md "The plinth"), not arbitrary round numbers.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct PlinthParams {
    pub height_mm: f64,
    pub taper_deg: f64,
    /// Open-bottom shell (wall + top plate) vs. a solid plug. Hollow saves
    /// material and prints support-free; solid stays available as a flag.
    pub hollow: bool,
    pub wall_mm: f64,
    pub top_mm: f64,
    /// Pocket-to-magnet fit: FDM prints holes 0.1-0.25 mm undersized, resin
    /// differs again, so this stays a user-visible, per-job knob.
    pub magnet_clearance_mm: f64,
}

impl Default for PlinthParams {
    fn default() -> Self {
        Self {
            height_mm: 3.7,
            taper_deg: 15.0,
            hollow: true,
            wall_mm: 1.2,
            top_mm: 1.2,
            magnet_clearance_mm: 0.15,
        }
    }
}

/// One cut instance: a cutter positioned on the landscape. `name` is a
/// user-facing label echoed back in progress events so they can name the
/// base, not just its index.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct Placement {
    pub cutter: CutterKind,
    pub x_mm: f64,
    pub y_mm: f64,
    pub rotation_deg: f64,
    /// None = no magnet pocket. Suggested from the inventory, overridable
    /// per placement.
    pub magnet: Option<MagnetSpec>,
    pub name: Option<String>,
}

/// A standard-library cutter (or a later user-defined one). Dimensions are
/// the NOMINAL (bottom-face, table-touching) footprint — the smaller cut
/// footprint is always derived from these via `top_face_of`, never stored.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct Cutter {
    pub id: String,
    pub label: String,
    pub kind: CutterKind,
}

/// The single owner of the nominal->cut derivation (docs/BASECUTTER.md "The
/// plinth"): a real base is widest at the table and slopes inward going up,
/// so the cut footprint (the plug's top face) is smaller than the nominal
/// size by twice the taper inset. One function, not several copies of the
/// same formula, so every consumer computes it identically.
pub fn top_face_of(kind: &CutterKind, plinth: &PlinthParams) -> CutterKind {
    let inset = plinth.height_mm * plinth.taper_deg.to_radians().tan();
    let shrink = 2.0 * inset;
    match kind {
        CutterKind::Circle { diameter_mm } => CutterKind::Circle {
            diameter_mm: diameter_mm - shrink,
        },
        CutterKind::Ellipse { major_mm, minor_mm } => CutterKind::Ellipse {
            major_mm: major_mm - shrink,
            minor_mm: minor_mm - shrink,
        },
        CutterKind::Rect { width_mm, depth_mm } => CutterKind::Rect {
            width_mm: width_mm - shrink,
            depth_mm: depth_mm - shrink,
        },
    }
}

/// Formats a millimetre value the way base ids/labels read in the hobby:
/// whole numbers unadorned ("32"), halves kept ("28.5") — never the trailing
/// zeros plain float formatting would add.
fn fmt_mm(mm: f64) -> String {
    if mm.fract() == 0.0 {
        format!("{}", mm as i64)
    } else {
        format!("{mm}")
    }
}

/// The seed cutter library (docs/BASECUTTER.md "Seed library") — the common
/// wargaming base sizes, seeded as data so a custom size is later just a new
/// row, never a code change. Ids are kebab-case and meant to stay stable:
/// placements may end up referencing them by id, so don't renumber existing
/// entries. Sizes are seed data, not gospel — verify against off-the-shelf
/// bases before this table freezes (see the doc's caveat).
pub fn seed_library() -> Vec<Cutter> {
    let rounds: &[(&str, f64)] = &[
        ("25", 25.0),
        ("28-5", 28.5),
        ("32", 32.0),
        ("40", 40.0),
        ("50", 50.0),
        ("60", 60.0),
        ("80", 80.0),
        ("90", 90.0),
        ("100", 100.0),
        ("130", 130.0),
        ("160", 160.0),
    ];
    let ovals: &[(f64, f64)] = &[
        (60.0, 35.0),
        (75.0, 42.0),
        (90.0, 52.0),
        (105.0, 70.0),
        (120.0, 92.0),
        (170.0, 105.0),
    ];
    let squares: &[f64] = &[20.0, 25.0, 30.0, 40.0, 50.0];
    // (width, depth, use-case label — generic terms, not a system's trademark)
    let rects: &[(f64, f64, &str)] = &[(25.0, 50.0, "cavalry"), (50.0, 100.0, "chariot")];

    let mut lib = Vec::with_capacity(rounds.len() + ovals.len() + squares.len() + rects.len());

    for (id_suffix, diameter_mm) in rounds {
        lib.push(Cutter {
            id: format!("round-{id_suffix}"),
            label: format!("{} mm round", fmt_mm(*diameter_mm)),
            kind: CutterKind::Circle {
                diameter_mm: *diameter_mm,
            },
        });
    }
    for (major_mm, minor_mm) in ovals {
        lib.push(Cutter {
            id: format!("oval-{}x{}", fmt_mm(*major_mm), fmt_mm(*minor_mm)),
            label: format!("{}x{} mm oval", fmt_mm(*major_mm), fmt_mm(*minor_mm)),
            kind: CutterKind::Ellipse {
                major_mm: *major_mm,
                minor_mm: *minor_mm,
            },
        });
    }
    for side_mm in squares {
        lib.push(Cutter {
            id: format!("square-{}", fmt_mm(*side_mm)),
            label: format!("{} mm square", fmt_mm(*side_mm)),
            kind: CutterKind::Rect {
                width_mm: *side_mm,
                depth_mm: *side_mm,
            },
        });
    }
    for (width_mm, depth_mm, use_case) in rects {
        lib.push(Cutter {
            id: format!("rect-{}x{}", fmt_mm(*width_mm), fmt_mm(*depth_mm)),
            label: format!(
                "{}x{} mm rect ({use_case})",
                fmt_mm(*width_mm),
                fmt_mm(*depth_mm)
            ),
            kind: CutterKind::Rect {
                width_mm: *width_mm,
                depth_mm: *depth_mm,
            },
        });
    }
    lib
}

#[tauri::command]
#[specta::specta]
pub fn get_cutter_library() -> Vec<Cutter> {
    seed_library()
}

/// The caliper-measured plinth profile, as a command so the frontend's
/// defaults come from this one Rust value instead of a hand-copied
/// literal that could drift from it.
#[tauri::command]
#[specta::specta]
pub fn get_plinth_defaults() -> PlinthParams {
    PlinthParams::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the JSON shape base_cut.py expects: internally tagged on
    /// "kind", lowercase variant name, fields flattened alongside the tag.
    #[test]
    fn cutter_kind_serializes_with_kind_tag() {
        let circle = CutterKind::Circle { diameter_mm: 32.0 };
        let json = serde_json::to_value(&circle).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"kind": "circle", "diameter_mm": 32.0})
        );

        let back: CutterKind = serde_json::from_value(json).unwrap();
        match back {
            CutterKind::Circle { diameter_mm } => assert_eq!(diameter_mm, 32.0),
            other => panic!("expected Circle, got {other:?}"),
        }
    }

    #[test]
    fn ellipse_and_rect_serialize_with_kind_tag() {
        let ellipse = CutterKind::Ellipse {
            major_mm: 60.0,
            minor_mm: 35.0,
        };
        assert_eq!(
            serde_json::to_value(&ellipse).unwrap(),
            serde_json::json!({"kind": "ellipse", "major_mm": 60.0, "minor_mm": 35.0})
        );

        let rect = CutterKind::Rect {
            width_mm: 25.0,
            depth_mm: 50.0,
        };
        assert_eq!(
            serde_json::to_value(&rect).unwrap(),
            serde_json::json!({"kind": "rect", "width_mm": 25.0, "depth_mm": 50.0})
        );
    }

    #[test]
    fn plinth_defaults_match_the_measured_base() {
        let plinth = PlinthParams::default();
        assert_eq!(plinth.height_mm, 3.7);
        assert_eq!(plinth.taper_deg, 15.0);
        assert!(plinth.hollow);
        assert_eq!(plinth.wall_mm, 1.2);
        assert_eq!(plinth.top_mm, 1.2);
        assert_eq!(plinth.magnet_clearance_mm, 0.15);
    }

    #[test]
    fn seed_library_size_and_spot_checks() {
        let lib = seed_library();
        assert_eq!(lib.len(), 24, "11 rounds + 6 ovals + 5 squares + 2 rects");

        let round32 = lib
            .iter()
            .find(|c| c.id == "round-32")
            .expect("round-32 present");
        assert_eq!(round32.label, "32 mm round");
        match round32.kind {
            CutterKind::Circle { diameter_mm } => assert_eq!(diameter_mm, 32.0),
            ref other => panic!("round-32 should be a circle, got {other:?}"),
        }

        let round28_5 = lib
            .iter()
            .find(|c| c.id == "round-28-5")
            .expect("round-28-5 present");
        assert_eq!(round28_5.label, "28.5 mm round");

        let oval = lib
            .iter()
            .find(|c| c.id == "oval-60x35")
            .expect("oval-60x35 present");
        match oval.kind {
            CutterKind::Ellipse { major_mm, minor_mm } => {
                assert_eq!(major_mm, 60.0);
                assert_eq!(minor_mm, 35.0);
            }
            ref other => panic!("oval-60x35 should be an ellipse, got {other:?}"),
        }

        let square = lib
            .iter()
            .find(|c| c.id == "square-25")
            .expect("square-25 present");
        assert_eq!(square.label, "25 mm square");
        match square.kind {
            CutterKind::Rect {
                width_mm,
                depth_mm,
            } => {
                assert_eq!(width_mm, 25.0);
                assert_eq!(depth_mm, 25.0);
            }
            ref other => panic!("square-25 should be a rect, got {other:?}"),
        }

        let cavalry = lib
            .iter()
            .find(|c| c.id == "rect-25x50")
            .expect("rect-25x50 present");
        assert_eq!(cavalry.label, "25x50 mm rect (cavalry)");
        match cavalry.kind {
            CutterKind::Rect {
                width_mm,
                depth_mm,
            } => {
                assert_eq!(width_mm, 25.0);
                assert_eq!(depth_mm, 50.0);
            }
            ref other => panic!("rect-25x50 should be a rect, got {other:?}"),
        }
    }

    #[test]
    fn seed_library_has_no_trademarked_names() {
        let banned = ["gw", "games workshop", "citadel", "warhammer", "old world"];
        for cutter in seed_library() {
            let hay = format!("{} {}", cutter.id, cutter.label).to_lowercase();
            for word in banned {
                assert!(!hay.contains(word), "{} contains banned word {word}", hay);
            }
        }
    }

    /// 32 - 2*3.7*tan(15deg) = 32 - 1.98282... = 30.017 mm, matching the
    /// caliper measurement in docs/BASECUTTER.md ("32 mm at the table,
    /// 30 mm on top" was rounded for the prose; the derivation is exact).
    #[test]
    fn top_face_of_circle_matches_measured_taper() {
        let cutter = CutterKind::Circle { diameter_mm: 32.0 };
        let plinth = PlinthParams::default();
        match top_face_of(&cutter, &plinth) {
            CutterKind::Circle { diameter_mm } => {
                assert!(
                    (diameter_mm - 30.017).abs() < 0.01,
                    "got {diameter_mm}, want 30.017 +/- 0.01"
                );
            }
            other => panic!("expected Circle, got {other:?}"),
        }
    }

    #[test]
    fn top_face_of_rect_shrinks_both_axes_equally() {
        let cutter = CutterKind::Rect {
            width_mm: 25.0,
            depth_mm: 50.0,
        };
        let plinth = PlinthParams::default();
        let inset = plinth.height_mm * plinth.taper_deg.to_radians().tan();
        match top_face_of(&cutter, &plinth) {
            CutterKind::Rect {
                width_mm,
                depth_mm,
            } => {
                assert!((width_mm - (25.0 - 2.0 * inset)).abs() < 1e-9);
                assert!((depth_mm - (50.0 - 2.0 * inset)).abs() < 1e-9);
            }
            other => panic!("expected Rect, got {other:?}"),
        }
    }

    #[test]
    fn top_face_of_ellipse_shrinks_both_axes_equally() {
        let cutter = CutterKind::Ellipse {
            major_mm: 60.0,
            minor_mm: 35.0,
        };
        let plinth = PlinthParams::default();
        let inset = plinth.height_mm * plinth.taper_deg.to_radians().tan();
        match top_face_of(&cutter, &plinth) {
            CutterKind::Ellipse { major_mm, minor_mm } => {
                assert!((major_mm - (60.0 - 2.0 * inset)).abs() < 1e-9);
                assert!((minor_mm - (35.0 - 2.0 * inset)).abs() < 1e-9);
            }
            other => panic!("expected Ellipse, got {other:?}"),
        }
    }

    #[test]
    fn get_cutter_library_matches_seed_library() {
        assert_eq!(get_cutter_library().len(), seed_library().len());
    }

    #[test]
    fn get_plinth_defaults_matches_the_measured_base() {
        let defaults = get_plinth_defaults();
        let expected = PlinthParams::default();
        assert_eq!(defaults.height_mm, expected.height_mm);
        assert_eq!(defaults.taper_deg, expected.taper_deg);
        assert_eq!(defaults.hollow, expected.hollow);
        assert_eq!(defaults.wall_mm, expected.wall_mm);
        assert_eq!(defaults.top_mm, expected.top_mm);
        assert_eq!(defaults.magnet_clearance_mm, expected.magnet_clearance_mm);
    }
}
