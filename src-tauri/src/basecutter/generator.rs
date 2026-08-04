//! Parametric landscape generator — see docs/BASECUTTER.md "The landscape
//! generator (phase 6)". A single-shot bake, not an N-cut batch: one
//! Blender launch produces one heightfield STL, deterministic from a seed.
//!
//! PRESETS LIVE HERE, NOT IN THE SCRIPT: gen_landscape.py only knows
//! parameters — a new preset is a new row in `get_landscape_presets`,
//! never a script change.

use crate::error::AppError;
use crate::models::BlenderInfo;
use crate::models::events::{
    LandscapeGenCancelledStatus, LandscapeGenFailedStatus, LandscapeGenFinishedStatus,
    LandscapeGenStartedStatus, LandscapeGenStatus,
};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use tokio::sync::Notify;
use uuid::Uuid;

/// The Blender script ships INSIDE the binary (see
/// engine::materialize_embedded_script for the always-overwrite rationale).
const GEN_LANDSCAPE_SCRIPT: &str = include_str!("../../resources/gen_landscape.py");

/// Grid step floor — 0.1mm is resin-grade lateral detail (the userbase
/// prints resin); the script's own MAX_GRID_VERTS budget is what guards
/// against a fine step on a huge plate, so this floor only exists to stop
/// a typo like 0.01 from freezing a bake. Must match gen_landscape.py's
/// MIN_RESOLUTION_MM.
pub const MIN_RESOLUTION_MM: f64 = 0.1;

fn default_resolution_mm() -> f64 {
    0.75
}
fn default_carrier_mm() -> f64 {
    2.0
}
fn default_feature_scale() -> f64 {
    1.0
}
fn default_amount() -> f64 {
    1.0
}

/// Base terrain: stacked-octave noise, optionally ridged for sharp crests.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct NoiseLayer {
    #[serde(default)]
    pub enabled: bool,
    /// Frequency multiplier — bigger scale = smaller, more numerous
    /// features (matches Blender's own Noise Texture node convention).
    #[serde(default)]
    pub scale: f64,
    #[serde(default)]
    pub octaves: u32,
    /// abs/1-abs transform per octave for sharp mountain crests instead of
    /// rolling hills.
    #[serde(default)]
    pub ridged: bool,
    #[serde(default = "default_amount")]
    pub amount: f64,
}

impl Default for NoiseLayer {
    fn default() -> Self {
        Self {
            enabled: false,
            scale: 0.05,
            octaves: 4,
            ridged: false,
            amount: 1.0,
        }
    }
}

/// Windswept sand: a directional sine wave, its phase distorted by a slow
/// noise field when `waviness` > 0 so ripples meander instead of ruling
/// dead-straight lines.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct RipplesLayer {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub wavelength_mm: f64,
    #[serde(default)]
    pub direction_deg: f64,
    #[serde(default = "default_amount")]
    pub amount: f64,
    #[serde(default)]
    pub waviness: f64,
}

impl Default for RipplesLayer {
    fn default() -> Self {
        Self {
            enabled: false,
            wavelength_mm: 8.0,
            direction_deg: 0.0,
            amount: 1.0,
            waviness: 0.3,
        }
    }
}

/// Cobblestones: a hashed, jittered 2D Voronoi — domed stones separated by
/// a recessed mortar gap.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct StonesLayer {
    #[serde(default)]
    pub enabled: bool,
    pub cell_mm: f64,
    pub gap_mm: f64,
    /// 0 = flat-topped setts, 1 = fully rounded cobbles.
    pub dome: f64,
    /// Per-stone height variance (0 = every stone the same height).
    pub jitter: f64,
    /// 0 = even cobbles; towards 1, a slow field drowns low cells into open
    /// lakes and fuses gaps between strongly-crusted neighbors — uneven
    /// crust masses (the lava look) instead of a tiled street.
    #[serde(default)]
    pub cluster: f64,
    /// 0 = clean Voronoi edges; towards 1, ~1-2mm wobble makes plate
    /// outlines ragged/broken (needs a fine enough grid to resolve).
    #[serde(default)]
    pub rough: f64,
    #[serde(default = "default_amount")]
    pub amount: f64,
}

impl Default for StonesLayer {
    fn default() -> Self {
        Self {
            enabled: false,
            // Minis are ~1:56: a 4mm cell is a ~22cm cobble. The first
            // 12mm tuning read as two giant flagstones on a 25mm base.
            cell_mm: 4.0,
            gap_mm: 0.5,
            dome: 0.6,
            jitter: 0.15,
            cluster: 0.0,
            rough: 0.0,
            amount: 1.0,
        }
    }
}

/// N seeded rocks — lumpy elliptical footprints with a per-boulder
/// superellipse height profile (steep-shouldered, flat-topped plateau, not
/// a gaussian dome) and noise-displaced surface grain, combined by max (not
/// sum) so overlapping boulders read as touching stones rather than a
/// stacked tower (see gen_landscape.py's _boulders_layer docstring — that's
/// where all the shape tuning lives, this struct is just count/size/amount).
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct BouldersLayer {
    #[serde(default)]
    pub enabled: bool,
    pub count: u32,
    pub min_mm: f64,
    pub max_mm: f64,
    #[serde(default = "default_amount")]
    pub amount: f64,
}

impl Default for BouldersLayer {
    fn default() -> Self {
        Self {
            enabled: false,
            count: 6,
            min_mm: 8.0,
            max_mm: 20.0,
            amount: 1.0,
        }
    }
}

/// Lava/river channels: low-frequency noise, absolute-valued so its
/// zero-crossings become winding channel centerlines and its peaks become
/// raised crusted banks.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct FlowLayer {
    #[serde(default)]
    pub enabled: bool,
    pub channel_width_mm: f64,
    pub meander_scale: f64,
    /// Sharpens the channel-to-bank transition (a power curve), not an
    /// overall scale — `amount` is still the one knob every layer shares
    /// for that.
    pub bank_height: f64,
    #[serde(default = "default_amount")]
    pub amount: f64,
}

impl Default for FlowLayer {
    fn default() -> Self {
        Self {
            enabled: false,
            channel_width_mm: 10.0,
            meander_scale: 0.3,
            bank_height: 1.0,
            amount: 1.0,
        }
    }
}

/// Parabolic crown across the plate's width — cobblestone streets are
/// highest at the centerline, sloping to the gutters at the edges.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct CamberLayer {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_amount")]
    pub amount: f64,
}

impl Default for CamberLayer {
    fn default() -> Self {
        Self {
            enabled: false,
            amount: 1.0,
        }
    }
}

/// Emissive glow spec for `MaterialPalette.glow` — lava-only today (the
/// crust needs a stones layer to key `glow_w` off of, see
/// gen_landscape.py's `_paint_landscape`), but the shape itself doesn't
/// assume that; any future preset can opt in the same way.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct GlowSpec {
    /// "#rrggbb", sRGB — same hex convention as the other palette fields.
    pub color: String,
    pub strength: f64,
}

/// Vertex-color/material palette for the landscape's GLB twin (VTT GLB
/// export design doc: "Palette contract"). Hex strings are sRGB; the
/// script converts to linear only where a shader node's `default_value`
/// needs it (the "Col" corner attribute itself takes sRGB directly via
/// `.color_srgb`). `glow` is only set for terrains that actually want an
/// emissive material slot (stlpack_glow) — everything else gets just
/// stlpack_terrain + stlpack_base.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct MaterialPalette {
    /// Dominant terrain color.
    pub ground: String,
    /// Stones/boulders/relief-high tint, blended in by accent_w.
    pub accent: String,
    /// Skirt/bottom/plinth "plastic" color — uniform, no vertex paint.
    pub base: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glow: Option<GlowSpec>,
}

impl Default for MaterialPalette {
    fn default() -> Self {
        // Neutral grey — used when a preset/caller doesn't specify a
        // palette at all (also gen_landscape.py's own DEFAULT_PALETTE
        // fallback for params files predating this feature).
        Self {
            ground: "#8a8a8a".to_string(),
            accent: "#a0a0a0".to_string(),
            base: "#232227".to_string(),
            glow: None,
        }
    }
}

/// All style layers, every one individually optional and summable. Field-
/// level `#[serde(default)]` (each layer type's own `Default`, which sets
/// `enabled: false`) means a preset JSON can omit whole layers outright.
#[derive(Serialize, Deserialize, Clone, Debug, Default, Type)]
pub struct LandscapeLayers {
    #[serde(default)]
    pub noise: NoiseLayer,
    #[serde(default)]
    pub ripples: RipplesLayer,
    #[serde(default)]
    pub stones: StonesLayer,
    #[serde(default)]
    pub boulders: BouldersLayer,
    #[serde(default)]
    pub flow: FlowLayer,
    #[serde(default)]
    pub camber: CamberLayer,
}

/// The frontend-facing (and preset-carried) parameter set. Deliberately has
/// no `out` field — `start_landscape_generation` derives the output path
/// (app data dir) and injects it into the wire JSON the same way
/// `job::job_json_with_cut_footprints` injects "cut" for base_cut.py.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct LandscapeParams {
    pub seed: u32,
    pub width_mm: f64,
    pub depth_mm: f64,
    #[serde(default = "default_resolution_mm")]
    pub resolution_mm: f64,
    /// One "zoom" for the terrain itself, distinct from resolution_mm's
    /// mesh density: multiplies every layer's characteristic length (stone
    /// cells+gaps, ripple wavelength, noise feature size, boulder sizes,
    /// channel width) so "same terrain, chunkier/finer" is one knob, not
    /// five consistent edits.
    #[serde(default = "default_feature_scale")]
    pub feature_scale: f64,
    #[serde(default = "default_carrier_mm")]
    pub carrier_mm: f64,
    pub relief_mm: f64,
    #[serde(default)]
    pub layers: LandscapeLayers,
    /// Vertex-color/material palette for the GLB twin. `#[serde(default)]`
    /// so old preset JSON / saved custom params without a "palette" key
    /// still deserialize (neutral grey — see `MaterialPalette::default`).
    #[serde(default)]
    pub palette: MaterialPalette,
}

/// A named, ready-to-generate parameter set (docs/BASECUTTER.md: "Presets
/// are parameter sets" — the cutter-library move again, a new terrain
/// style is a new row here, not a new pipeline).
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct GeneratorPreset {
    pub id: String,
    pub label: String,
    pub params: LandscapeParams,
}

/// The four seed presets (docs/BASECUTTER.md phase 6), tuned for a
/// 120x80mm plate by actually running gen_landscape.py and eyeballing the
/// bake — not just numbers that happened to compile.
///
/// - **cobblestone-street**: stones (4mm cells / 0.5mm mortar) + camber
///   (the street crown) + a little base noise so the plate isn't
///   perfectly flat between stones. Baked at 0.4mm grid so the joints
///   resolve.
/// - **sandy**: directional ripples (windswept dune ridges) over soft,
///   low-amplitude rolling noise (the dune body itself).
/// - **rocky**: ridged noise (fine jagged detail) + a handful of chunky
///   boulders, kept at much lower amplitude and higher frequency than the
///   boulders (0.18/0.1 vs. the boulders' 1.0/14-30mm) so the two
///   features read as distinct rather than the noise swamping the
///   boulder shapes.
/// - **lava-flow**: the flow channel field + a light ridged-noise crust so
///   the banks aren't perfectly smooth.
/// - **forest-floor**: bare dirt under trees, not a heightfield showpiece
///   — it exists to be SCATTERED onto, so the terrain itself stays quiet.
///   One noise layer at `scale: 0.045` (~22mm wavelength, about a mini's
///   base) with `octaves: 5` for the earthy surface grain off that one
///   layer. `ridged: false` (dirt isn't sharp ridges); `relief_mm: 3.5`
///   sits between cobblestone's subtle texture and sandy's dune body.
pub fn seed_presets() -> Vec<GeneratorPreset> {
    vec![
        GeneratorPreset {
            id: "cobblestone-street".to_string(),
            label: "Cobblestone street".to_string(),
            params: LandscapeParams {
                seed: 1,
                width_mm: 120.0,
                depth_mm: 80.0,
                // Finer than the default grid: 4mm cobbles with 0.5mm
                // mortar need ~0.4mm sampling or the joints alias.
                resolution_mm: 0.4,
                feature_scale: 1.0,
                carrier_mm: 2.0,
                relief_mm: 1.6,
                layers: LandscapeLayers {
                    // Scale check: minis are ~1:56, so a 4mm cell is a
                    // ~22cm cobble — reads as cobbles on a 25mm base
                    // (~6 across), not the two giant flagstones the first
                    // 12mm tuning produced (caught by the first GUI cut).
                    // Low dome = flat-topped setts; high dome at this cell
                    // size read as golf-ball dimples.
                    stones: StonesLayer {
                        enabled: true,
                        cell_mm: 4.0,
                        gap_mm: 0.5,
                        dome: 0.35,
                        jitter: 0.25,
                        cluster: 0.0,
                        rough: 0.0,
                        amount: 1.0,
                    },
                    camber: CamberLayer {
                        enabled: true,
                        amount: 0.35,
                    },
                    noise: NoiseLayer {
                        enabled: true,
                        scale: 0.2,
                        octaves: 2,
                        ridged: false,
                        amount: 0.1,
                    },
                    ..Default::default()
                },
                // Grey mortar / lighter grey stone — a plain street, not
                // dirt or sand (design doc's palette table).
                palette: MaterialPalette {
                    ground: "#55504a".to_string(),
                    accent: "#8f8a84".to_string(),
                    base: "#232227".to_string(),
                    glow: None,
                },
            },
        },
        GeneratorPreset {
            id: "sandy".to_string(),
            label: "Sandy dunes".to_string(),
            params: LandscapeParams {
                seed: 2,
                width_mm: 120.0,
                depth_mm: 80.0,
                resolution_mm: 0.75,
                feature_scale: 1.0,
                carrier_mm: 2.0,
                relief_mm: 4.0,
                layers: LandscapeLayers {
                    // Dunes dominate, ripples are surface texture riding
                    // them — inverted from the first tuning, which read as
                    // uniform corduroy (ripples 2x the dune amplitude).
                    ripples: RipplesLayer {
                        enabled: true,
                        wavelength_mm: 6.0,
                        direction_deg: 20.0,
                        amount: 0.5,
                        waviness: 0.9,
                    },
                    noise: NoiseLayer {
                        enabled: true,
                        scale: 0.03,
                        octaves: 2,
                        ridged: false,
                        amount: 1.4,
                    },
                    ..Default::default()
                },
                // Sahara yellow (design doc's palette table).
                palette: MaterialPalette {
                    ground: "#d7b269".to_string(),
                    accent: "#b98f4e".to_string(),
                    base: "#232227".to_string(),
                    glow: None,
                },
            },
        },
        GeneratorPreset {
            id: "rocky".to_string(),
            label: "Rocky ground".to_string(),
            params: LandscapeParams {
                seed: 3,
                width_mm: 120.0,
                depth_mm: 80.0,
                resolution_mm: 0.75,
                feature_scale: 1.0,
                carrier_mm: 2.0,
                relief_mm: 10.0,
                layers: LandscapeLayers {
                    noise: NoiseLayer {
                        enabled: true,
                        scale: 0.1,
                        octaves: 4,
                        ridged: true,
                        amount: 0.18,
                    },
                    boulders: BouldersLayer {
                        enabled: true,
                        count: 7,
                        min_mm: 14.0,
                        max_mm: 30.0,
                        amount: 1.0,
                    },
                    ..Default::default()
                },
                // Grey stone (design doc's palette table).
                palette: MaterialPalette {
                    ground: "#83868a".to_string(),
                    accent: "#a5a8ac".to_string(),
                    base: "#232227".to_string(),
                    glow: None,
                },
            },
        },
        GeneratorPreset {
            id: "lava-flow".to_string(),
            label: "Lava flow".to_string(),
            params: LandscapeParams {
                seed: 4,
                width_mm: 120.0,
                depth_mm: 80.0,
                resolution_mm: 0.4,
                feature_scale: 1.0,
                carrier_mm: 2.0,
                relief_mm: 4.5,
                layers: LandscapeLayers {
                    // The lava-base look (per the reference boards): chunky
                    // raised CRUST ISLANDS with wide, deep channels between
                    // them — the channels are where painters put the glow.
                    // That's the Voronoi layer with inverted proportions:
                    // big plates, 4.5mm channel gaps instead of mortar
                    // lines, hard flat-ish tops, strong per-plate height
                    // jitter so the crust reads broken, not tiled. The
                    // smooth-flow layer alone read as mud tongues.
                    stones: StonesLayer {
                        enabled: true,
                        cell_mm: 16.0,
                        gap_mm: 4.0,
                        dome: 0.15,
                        jitter: 0.5,
                        // The two knobs that stop it reading as cobbles:
                        // clustering drowns whole cells into lakes and
                        // fuses crusted neighbors into large masses;
                        // roughness breaks the plate outlines ragged.
                        cluster: 0.8,
                        rough: 0.75,
                        amount: 1.0,
                    },
                    // Ridged roughness: crust texture on the plates, faint
                    // flow ripple in the channels (summed everywhere, kept
                    // small so the channels still read liquid).
                    noise: NoiseLayer {
                        enabled: true,
                        scale: 0.14,
                        octaves: 3,
                        ridged: true,
                        amount: 0.3,
                    },
                    ..Default::default()
                },
                // Black crust, emissive orange in the channel gaps — the
                // only preset with a glow entry (design doc's palette
                // table; glow is gated on the stones layer in
                // gen_landscape.py's _paint_landscape, which lava-flow has).
                palette: MaterialPalette {
                    ground: "#1b191d".to_string(),
                    accent: "#322e35".to_string(),
                    base: "#232227".to_string(),
                    glow: Some(GlowSpec {
                        color: "#ff4d00".to_string(),
                        strength: 4.0,
                    }),
                },
            },
        },
        GeneratorPreset {
            id: "forest-floor".to_string(),
            label: "Forest floor".to_string(),
            params: LandscapeParams {
                seed: 5,
                width_mm: 120.0,
                depth_mm: 80.0,
                // Fine enough to resolve the ~1.4mm top octave (see the
                // doc comment above) with a few samples per wavelength —
                // still tiny against MAX_GRID_VERTS (well under 100k verts
                // on a 120x80 plate).
                resolution_mm: 0.4,
                feature_scale: 1.0,
                carrier_mm: 2.0,
                // A few mm of relief — soil, not dunes.
                relief_mm: 3.5,
                layers: LandscapeLayers {
                    // One layer does both jobs: scale 0.045 (~22mm
                    // wavelength, about a mini-base's own size) is the
                    // gentle rolling undulation; the extra octaves ride on
                    // top of the SAME layer at progressively smaller
                    // wavelength/weaker amplitude (fixed lacunarity 2.0 /
                    // persistence 0.5 in _fractal), landing around 1.4mm
                    // at octave 5 — that's the earthy surface grain, no
                    // second layer needed. Base wavelength matters here,
                    // not just octave count: a 50mm base (tried first)
                    // spread the same relative grain weight too thin
                    // across a close-up-sized patch to actually read (see
                    // the doc comment above).
                    noise: NoiseLayer {
                        enabled: true,
                        scale: 0.045,
                        octaves: 5,
                        ridged: false,
                        amount: 1.0,
                    },
                    // stones/boulders/ripples/flow/camber all stay off
                    // (Default::default()): no cobbles, no boulders, no
                    // ripples, no lava, no street crown — this terrain
                    // exists to be scattered onto, not to be the show.
                    ..Default::default()
                },
                // Brown earth (design doc's palette table).
                palette: MaterialPalette {
                    ground: "#5d4936".to_string(),
                    accent: "#77614a".to_string(),
                    base: "#232227".to_string(),
                    glow: None,
                },
            },
        },
    ]
}

#[tauri::command]
#[specta::specta]
pub fn get_landscape_presets() -> Vec<GeneratorPreset> {
    seed_presets()
}

/// Write the embedded generator script where Blender can read it. Always
/// overwrites, so the file on disk can never drift from the built app.
pub fn materialize_gen_landscape_script(app_handle: &AppHandle) -> Result<PathBuf, AppError> {
    crate::render::engine::materialize_embedded_script(
        app_handle,
        "gen_landscape.py",
        GEN_LANDSCAPE_SCRIPT,
    )
}

/// One parsed line of gen_landscape.py's stdout protocol (see its
/// docstring). Kept pure/process-free, same as basecutter::job's
/// `parse_token`, so the grammar is unit-testable without spawning Blender.
#[derive(Debug, Clone, PartialEq)]
pub enum LandscapeToken {
    Generating {
        seed: u32,
    },
    Generated {
        out: String,
        /// The GLB twin's path. `Option` so the parser stays tolerant of
        /// a params/script mismatch across an
        /// upgrade window — never observed from THIS script version, which
        /// always reports it, but the wire format shouldn't hard-fail on a
        /// missing key it doesn't strictly need to function.
        glb: Option<String>,
        dims_mm: [f64; 3],
        verts: u32,
        manifold: bool,
    },
    GenerationFailed {
        reason: String,
    },
}

pub fn parse_landscape_token(line: &str) -> Option<LandscapeToken> {
    #[derive(Deserialize)]
    struct GeneratingPayload {
        seed: u32,
    }
    #[derive(Deserialize)]
    struct GeneratedPayload {
        out: String,
        #[serde(default)]
        glb: Option<String>,
        dims_mm: [f64; 3],
        verts: u32,
        manifold: bool,
    }
    #[derive(Deserialize)]
    struct FailedPayload {
        reason: String,
    }

    let line = line.trim();
    if let Some(json) = line.strip_prefix("GENERATING ") {
        let p: GeneratingPayload = serde_json::from_str(json).ok()?;
        return Some(LandscapeToken::Generating { seed: p.seed });
    }
    if let Some(json) = line.strip_prefix("GENERATED ") {
        let p: GeneratedPayload = serde_json::from_str(json).ok()?;
        return Some(LandscapeToken::Generated {
            out: p.out,
            glb: p.glb,
            dims_mm: p.dims_mm,
            verts: p.verts,
            manifold: p.manifold,
        });
    }
    if let Some(json) = line.strip_prefix("GENERATION_FAILED ") {
        let p: FailedPayload = serde_json::from_str(json).ok()?;
        return Some(LandscapeToken::GenerationFailed { reason: p.reason });
    }
    None
}

/// Clamp resolution to the floor and inject the computed output path — the
/// wire JSON gen_landscape.py actually reads. Mirrors
/// `job::job_json_with_cut_footprints`'s "Rust derives, script consumes"
/// split.
fn params_json_with_out(params: &LandscapeParams, out_path: &Path) -> Result<serde_json::Value, AppError> {
    let mut clamped = params.clone();
    if clamped.resolution_mm < MIN_RESOLUTION_MM {
        clamped.resolution_mm = MIN_RESOLUTION_MM;
    }
    let mut value = serde_json::to_value(&clamped)
        .map_err(|e| AppError::JsonError(format!("Failed to encode landscape params: {}", e)))?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "out".to_string(),
            serde_json::Value::String(out_path.to_string_lossy().into_owned()),
        );
    }
    Ok(value)
}

/// Write the job params JSON into `dir` so Blender can read it via
/// `--params`.
pub fn write_params_file(
    dir: &Path,
    params: &LandscapeParams,
    out_path: &Path,
    job_id: &str,
) -> Result<PathBuf, AppError> {
    let path = dir.join(format!("gen_landscape_params_{job_id}.json"));
    let value = params_json_with_out(params, out_path)?;
    let json = serde_json::to_string_pretty(&value)
        .map_err(|e| AppError::JsonError(format!("Failed to encode landscape params: {}", e)))?;
    std::fs::write(&path, json)
        .map_err(|e| AppError::IoError(format!("Failed to write landscape params file: {}", e)))?;
    Ok(path)
}

/// Assemble the headless generation invocation — same `--background
/// --factory-startup --python-exit-code 1 --python <script> --` convention
/// as base_cut.py/render_mini.py, `--params <json>` instead of `--job`.
pub fn build_gen_landscape_command(
    blender: &BlenderInfo,
    script: &Path,
    params_path: &Path,
) -> tokio::process::Command {
    let mut cmd = crate::render::engine::new_command(Path::new(&blender.path));
    cmd.arg("--background")
        .arg("--factory-startup")
        .arg("--python-exit-code")
        .arg("1")
        .arg("--python")
        .arg(script)
        .arg("--")
        .arg("--params")
        .arg(params_path);
    cmd
}

/// The `LandscapeToken::Generated` payload's fields: out path, glb path,
/// dims_mm, vert count, manifold.
type GeneratedLandscape = (String, Option<String>, [f64; 3], u32, bool);

/// Spawn Blender against `params_path` and parse its stdout into
/// `LandscapeToken`s, invoking `on_token` for each as it arrives. Returns
/// the `Generated` payload's fields on success, or `(error, stdout_tail)`.
///
/// Unlike `job::spawn_and_parse`'s validation gate, there is nothing to
/// abort mid-run here — one script invocation makes exactly one mesh, so
/// every token is handled the same way regardless of content; failure is
/// entirely a non-zero exit (the script's own `sys.exit(1)` after
/// `GENERATION_FAILED`, or an uncaught exception via `--python-exit-code
/// 1`). The `GenerationFailed` token's `reason` is captured so the error
/// message can quote it instead of just "Blender exited with exit status 1".
pub async fn spawn_and_parse<F>(
    blender: &BlenderInfo,
    script: &Path,
    params_path: &Path,
    cancel_token: &Notify,
    mut on_token: F,
) -> Result<GeneratedLandscape, (AppError, String)>
where
    F: FnMut(&LandscapeToken),
{
    let cmd = build_gen_landscape_command(blender, script, params_path);
    let mut generated: Option<GeneratedLandscape> = None;
    let mut failure_reason: Option<String> = None;

    let merge_tail = |out: String, err: String| {
        if err.is_empty() {
            out
        } else {
            format!("{}\n{}", out, err)
        }
    };

    let run_result = crate::render::engine::run_blender_lines(cmd, Some(cancel_token), |line| {
        if let Some(token) = parse_landscape_token(line) {
            match &token {
                LandscapeToken::Generated {
                    out,
                    glb,
                    dims_mm,
                    verts,
                    manifold,
                } => generated = Some((out.clone(), glb.clone(), *dims_mm, *verts, *manifold)),
                LandscapeToken::GenerationFailed { reason } => {
                    failure_reason = Some(reason.clone())
                }
                LandscapeToken::Generating { .. } => {}
            }
            on_token(&token);
        }
        ControlFlow::Continue(())
    })
    .await;

    use crate::render::engine::BlenderRunError::*;
    let run = match run_result {
        Ok(run) => run,
        Err(SpawnFailed(e)) => {
            return Err((
                AppError::IoError(format!("Failed to launch Blender: {}", e)),
                String::new(),
            ))
        }
        Err(StdoutCaptureFailed) => {
            return Err((
                AppError::IoError("Failed to capture Blender stdout".to_string()),
                String::new(),
            ))
        }
        Err(ReadFailed { source, stdout_tail, stderr_tail }) => {
            return Err((
                AppError::IoError(format!("Failed reading Blender output: {}", source)),
                merge_tail(stdout_tail, stderr_tail),
            ))
        }
        Err(WaitFailed { source, stdout_tail, stderr_tail }) => {
            return Err((
                AppError::IoError(format!("Failed waiting for Blender: {}", source)),
                merge_tail(stdout_tail, stderr_tail),
            ))
        }
        Err(Cancelled { stdout_tail, stderr_tail }) => {
            return Err((
                AppError::UserCancelled("Landscape generation cancelled".to_string()),
                merge_tail(stdout_tail, stderr_tail),
            ))
        }
        Err(AbortedByCaller { stdout_tail, stderr_tail }) => {
            return Err((
                AppError::FileProcessingError("Landscape generation aborted".to_string()),
                merge_tail(stdout_tail, stderr_tail),
            ))
        }
    };

    if !run.status.success() {
        let message = failure_reason
            .map(|reason| format!("Landscape generation failed: {}", reason))
            .unwrap_or_else(|| format!("Blender exited with {}", run.status));
        return Err((
            AppError::FileProcessingError(message),
            merge_tail(run.stdout_tail, run.stderr_tail),
        ));
    }

    generated.ok_or_else(|| {
        (
            AppError::FileProcessingError(
                "Blender exited cleanly but never reported GENERATED".to_string(),
            ),
            merge_tail(run.stdout_tail, run.stderr_tail),
        )
    })
}

/// (job id, its cancel token) — the shape held by `ACTIVE_LANDSCAPE_GEN`,
/// named so clippy's complex-type lint doesn't fire on the static below.
type ActiveJob = (String, Arc<Notify>);

/// The single running landscape-generation job, if any — mirrors
/// basecutter::commands::ACTIVE_BASE_CUT (only one at a time; generation
/// and cutting are different activities but share the one Blender process
/// slot, so each gets its own simple guard rather than a shared map).
static ACTIVE_LANDSCAPE_GEN: Lazy<Mutex<Option<ActiveJob>>> = Lazy::new(|| Mutex::new(None));

fn landscape_output_dir(app_handle: &AppHandle) -> Result<PathBuf, AppError> {
    let dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| AppError::ConfigError(format!("No app data dir: {}", e)))?
        .join("landscapes");
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::IoError(format!("Failed to create landscapes dir: {}", e)))?;
    Ok(dir)
}

/// `preset_id` crosses the Tauri IPC bridge from an untrusted frontend and
/// is used below as `out_dir.join(format!("{slug}-{seed}.stl"))` — an
/// absolute path, a `..` component, or a drive/UNC prefix would make
/// `PathBuf::join` discard `out_dir` entirely and write the STL wherever
/// the caller chose instead. Rather than sanitizing the string, reject
/// anything that isn't one of the actual preset ids `seed_presets()` knows
/// about: `preset_id` is only ever meant to be a chip id, never
/// user-composed text (that's what `LandscapeParams` is for), so an
/// allow-list is both the stronger guard and the more honest one.
fn validate_preset_id(preset_id: Option<&str>) -> Result<(), AppError> {
    match preset_id {
        None => Ok(()),
        Some(id) => {
            if seed_presets().iter().any(|preset| preset.id == id) {
                Ok(())
            } else {
                Err(AppError::InvalidInput(format!(
                    "Unknown landscape preset id '{id}'"
                )))
            }
        }
    }
}

/// `preset_id` is the preset chip's id ("cobblestone-street", ...) when
/// starting from a chip, None for a from-scratch custom bake — only used to
/// name the output file (docs/BASECUTTER.md:
/// "landscapes/<preset-or-custom>-<seed>.stl").
#[tauri::command]
#[specta::specta]
pub async fn start_landscape_generation(
    app_handle: AppHandle,
    params: LandscapeParams,
    preset_id: Option<String>,
) -> Result<String, AppError> {
    if params.width_mm <= 0.0 || params.depth_mm <= 0.0 {
        return Err(AppError::InvalidInput(
            "Landscape width/depth must be positive".to_string(),
        ));
    }
    if params.relief_mm < 0.0 {
        return Err(AppError::InvalidInput(
            "Relief height must not be negative".to_string(),
        ));
    }
    validate_preset_id(preset_id.as_deref())?;

    let blender = crate::render::engine::detect_blender_cached().await?;
    let script = materialize_gen_landscape_script(&app_handle)?;

    let slug = preset_id.as_deref().unwrap_or("custom");
    let out_dir = landscape_output_dir(&app_handle)?;
    let out_path = out_dir.join(format!("{slug}-{}.stl", params.seed));

    let job_id = Uuid::new_v4().to_string();
    let cancel_token = Arc::new(Notify::new());
    {
        let mut active = ACTIVE_LANDSCAPE_GEN.lock().map_err(|e| {
            AppError::ConfigError(format!("Failed to access landscape-gen registry: {}", e))
        })?;
        if active.is_some() {
            return Err(AppError::InvalidInput(
                "A landscape generation job is already running".to_string(),
            ));
        }
        *active = Some((job_id.clone(), Arc::clone(&cancel_token)));
    }

    LandscapeGenStatus::Started(LandscapeGenStartedStatus {
        job_id: job_id.clone(),
        seed: params.seed,
    })
    .emit(&app_handle)
    .ok();

    let job_id_clone = job_id.clone();
    tokio::spawn(async move {
        run_landscape_gen_job(
            app_handle,
            job_id_clone,
            blender,
            script,
            params,
            out_path,
            cancel_token,
        )
        .await;
    });

    Ok(job_id)
}

#[tauri::command]
#[specta::specta]
pub async fn cancel_landscape_generation(job_id: String) -> Result<(), AppError> {
    let active = ACTIVE_LANDSCAPE_GEN.lock().map_err(|e| {
        AppError::ConfigError(format!("Failed to access landscape-gen registry: {}", e))
    })?;
    match active.as_ref() {
        Some((active_id, token)) if *active_id == job_id => {
            // notify_one(), not notify_waiters() — same reasoning as
            // basecutter::commands::cancel_base_cut: a cancel landing before
            // the spawned task reaches spawn_and_parse's select loop must
            // not be dropped on the floor.
            token.notify_one();
            Ok(())
        }
        _ => Err(AppError::NotFoundError(format!(
            "No active landscape generation job with ID: {}",
            job_id
        ))),
    }
}

async fn run_landscape_gen_job(
    app_handle: AppHandle,
    job_id: String,
    blender: BlenderInfo,
    script: PathBuf,
    params: LandscapeParams,
    out_path: PathBuf,
    cancel_token: Arc<Notify>,
) {
    let script_dir = script
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let result = match write_params_file(&script_dir, &params, &out_path, &job_id) {
        Ok(params_path) => {
            let outcome = spawn_and_parse(&blender, &script, &params_path, &cancel_token, |_| {}).await;
            std::fs::remove_file(&params_path).ok();
            outcome
        }
        Err(e) => Err((e, String::new())),
    };

    if let Ok(mut active) = ACTIVE_LANDSCAPE_GEN.lock() {
        if active.as_ref().is_some_and(|(id, _)| id == &job_id) {
            *active = None;
        }
    }

    match result {
        Ok((out, glb, dims_mm, _verts, manifold)) => {
            LandscapeGenStatus::Finished(LandscapeGenFinishedStatus {
                job_id,
                out_path: out,
                glb_path: glb,
                dims_mm,
                manifold,
            })
            .emit(&app_handle)
            .ok();
        }
        Err((AppError::UserCancelled(_), _stdout_tail)) => {
            LandscapeGenStatus::Cancelled(LandscapeGenCancelledStatus { job_id })
                .emit(&app_handle)
                .ok();
        }
        Err((e, stdout_tail)) => {
            LandscapeGenStatus::Failed(LandscapeGenFailedStatus {
                job_id,
                message: e.to_string(),
                stdout_tail,
            })
            .emit(&app_handle)
            .ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landscape_params_serialize_to_the_script_shape() {
        let params = LandscapeParams {
            seed: 7,
            width_mm: 120.0,
            depth_mm: 80.0,
            resolution_mm: 0.75,
            feature_scale: 1.0,
            carrier_mm: 2.0,
            relief_mm: 6.0,
            layers: LandscapeLayers {
                noise: NoiseLayer {
                    enabled: true,
                    scale: 0.05,
                    octaves: 4,
                    ridged: false,
                    amount: 1.0,
                },
                ..Default::default()
            },
            palette: MaterialPalette::default(),
        };
        let json = serde_json::to_value(&params).unwrap();
        assert_eq!(json["seed"], 7);
        assert_eq!(json["width_mm"], 120.0);
        assert_eq!(json["layers"]["noise"]["enabled"], true);
        assert_eq!(json["layers"]["noise"]["scale"], 0.05);
        assert_eq!(json["layers"]["ripples"]["enabled"], false);
        assert_eq!(json["layers"]["stones"]["enabled"], false);
        assert_eq!(json["layers"]["boulders"]["enabled"], false);
        assert_eq!(json["layers"]["flow"]["enabled"], false);
        assert_eq!(json["layers"]["camber"]["enabled"], false);
        // No "out" on the frontend-facing type — that's injected only into
        // the wire JSON (see params_json_with_out / write_params_file).
        assert!(json.get("out").is_none());
        // Palette serializes into the exact shape gen_landscape.py's
        // DEFAULT_PALETTE/_paint_landscape expect.
        assert_eq!(json["palette"]["ground"], "#8a8a8a");
        assert_eq!(json["palette"]["accent"], "#a0a0a0");
        assert_eq!(json["palette"]["base"], "#232227");
        // glow is None -> the KEY is omitted entirely (skip_serializing_if),
        // not serialized as null: gen_landscape.py's palette.get("glow")
        // and Rust's serde(default) both treat "absent" as the no-glow
        // case, but an explicit null would still be a present key.
        assert!(json["palette"].get("glow").is_none());
    }

    #[test]
    fn palette_with_glow_serializes_the_glow_key() {
        let glow_params = LandscapeParams {
            seed: 4,
            width_mm: 120.0,
            depth_mm: 80.0,
            resolution_mm: 0.75,
            feature_scale: 1.0,
            carrier_mm: 2.0,
            relief_mm: 6.0,
            layers: LandscapeLayers::default(),
            palette: MaterialPalette {
                ground: "#1b191d".to_string(),
                accent: "#322e35".to_string(),
                base: "#232227".to_string(),
                glow: Some(GlowSpec {
                    color: "#ff4d00".to_string(),
                    strength: 4.0,
                }),
            },
        };
        let json = serde_json::to_value(&glow_params).unwrap();
        assert_eq!(json["palette"]["glow"]["color"], "#ff4d00");
        assert_eq!(json["palette"]["glow"]["strength"], 4.0);
    }

    #[test]
    fn a_preset_json_can_omit_disabled_layers_entirely() {
        let json = serde_json::json!({
            "seed": 1,
            "width_mm": 120.0,
            "depth_mm": 80.0,
            "relief_mm": 6.0,
            "layers": { "noise": { "enabled": true, "scale": 0.05, "octaves": 4, "amount": 1.0 } }
        });
        let params: LandscapeParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.resolution_mm, default_resolution_mm());
        assert_eq!(params.carrier_mm, default_carrier_mm());
        assert!(params.layers.noise.enabled);
        assert!(!params.layers.stones.enabled);
        assert!(!params.layers.boulders.enabled);
        // A params JSON predating this feature (no "palette" key at all)
        // must not fail to deserialize — falls back to the neutral default.
        assert_eq!(params.palette.ground, "#8a8a8a");
        assert_eq!(params.palette.accent, "#a0a0a0");
        assert_eq!(params.palette.base, "#232227");
        assert!(params.palette.glow.is_none());
    }

    #[test]
    fn params_json_with_out_injects_the_out_key_and_clamps_resolution() {
        let params = LandscapeParams {
            seed: 1,
            width_mm: 120.0,
            depth_mm: 80.0,
            resolution_mm: 0.1, // below the floor
            feature_scale: 1.0,
            carrier_mm: 2.0,
            relief_mm: 6.0,
            layers: LandscapeLayers::default(),
            palette: MaterialPalette::default(),
        };
        let value = params_json_with_out(&params, Path::new("/out/landscape.stl")).unwrap();
        assert_eq!(value["out"], "/out/landscape.stl");
        assert_eq!(value["resolution_mm"], MIN_RESOLUTION_MM);
    }

    #[test]
    fn get_landscape_presets_has_the_five_seed_presets() {
        let presets = seed_presets();
        let ids: Vec<&str> = presets.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["cobblestone-street", "sandy", "rocky", "lava-flow", "forest-floor"]
        );
        for preset in &presets {
            assert!(preset.params.width_mm > 0.0);
            assert!(preset.params.depth_mm > 0.0);
            assert!(preset.params.relief_mm > 0.0);
        }
    }

    /// Every seed preset must ship its own palette, not fall through to the
    /// neutral `MaterialPalette::default()` — a preset with the default
    /// palette would mean someone forgot to fill in the design doc's
    /// palette table row for it. lava-flow is additionally required to
    /// carry a glow entry (the black-crust/emissive-channel look).
    #[test]
    fn every_seed_preset_has_a_non_default_palette() {
        let default_palette = MaterialPalette::default();
        for preset in seed_presets() {
            let palette = &preset.params.palette;
            assert!(
                palette.ground != default_palette.ground
                    || palette.accent != default_palette.accent
                    || palette.base != default_palette.base
                    || palette.glow.is_some(),
                "preset '{}' has the neutral default palette — needs its own row from the design doc's palette table",
                preset.id
            );
        }
        let lava = seed_presets()
            .into_iter()
            .find(|p| p.id == "lava-flow")
            .unwrap();
        let glow = lava.params.palette.glow.expect("lava-flow must have a glow entry");
        assert_eq!(glow.color, "#ff4d00");
        assert_eq!(glow.strength, 4.0);
        for preset in seed_presets() {
            if preset.id != "lava-flow" {
                assert!(
                    preset.params.palette.glow.is_none(),
                    "only lava-flow should carry a glow entry, found one on '{}'",
                    preset.id
                );
            }
        }
    }

    #[test]
    fn validate_preset_id_accepts_none_and_known_ids() {
        assert!(validate_preset_id(None).is_ok());
        for preset in seed_presets() {
            assert!(validate_preset_id(Some(&preset.id)).is_ok());
        }
    }

    #[test]
    fn validate_preset_id_rejects_a_traversal_attempt() {
        for bad in ["../../evil", "/etc/passwd", "C:evil", "a/b", "a\\b", "not-a-real-preset"] {
            let err = validate_preset_id(Some(bad))
                .expect_err(&format!("'{bad}' must be rejected as a preset id"));
            assert!(
                err.to_string().contains("Unknown landscape preset id"),
                "error should explain why '{bad}' was rejected: {err}"
            );
        }
    }

    #[test]
    fn cobblestone_preset_enables_stones_and_camber() {
        let preset = seed_presets()
            .into_iter()
            .find(|p| p.id == "cobblestone-street")
            .unwrap();
        assert!(preset.params.layers.stones.enabled);
        assert!(preset.params.layers.camber.enabled);
        assert!(!preset.params.layers.boulders.enabled);
    }

    #[test]
    fn sandy_preset_enables_ripples() {
        let preset = seed_presets()
            .into_iter()
            .find(|p| p.id == "sandy")
            .unwrap();
        assert!(preset.params.layers.ripples.enabled);
        assert!(!preset.params.layers.stones.enabled);
    }

    #[test]
    fn rocky_preset_enables_ridged_noise_and_boulders() {
        let preset = seed_presets()
            .into_iter()
            .find(|p| p.id == "rocky")
            .unwrap();
        assert!(preset.params.layers.noise.enabled);
        assert!(preset.params.layers.noise.ridged);
        assert!(preset.params.layers.boulders.enabled);
    }

    /// Lava is CLUSTERED, ROUGH crust plates — not the smooth flow field
    /// (which read as mud tongues) and not an even Voronoi (which read as
    /// cobbles). Pins the two knobs that make it lava; exact values are
    /// tuning, the non-zero-ness is design.
    #[test]
    fn lava_flow_preset_is_clustered_ragged_crust() {
        let preset = seed_presets()
            .into_iter()
            .find(|p| p.id == "lava-flow")
            .unwrap();
        assert!(preset.params.layers.stones.enabled);
        assert!(preset.params.layers.stones.cluster > 0.5);
        assert!(preset.params.layers.stones.rough > 0.5);
    }

    /// Forest floor is quiet dirt, not a heightfield showpiece — pins that
    /// only noise is on (no stones/boulders/ripples/flow/camber) and that
    /// it stacks enough octaves to carry both the gentle rolling scale and
    /// the fine earthy grain from the one layer (see seed_presets' doc
    /// comment for why no second layer or new param was needed).
    #[test]
    fn forest_floor_preset_enables_only_soft_multi_octave_noise() {
        let preset = seed_presets()
            .into_iter()
            .find(|p| p.id == "forest-floor")
            .unwrap();
        assert!(preset.params.layers.noise.enabled);
        assert!(!preset.params.layers.noise.ridged);
        assert!(
            preset.params.layers.noise.octaves >= 4,
            "forest floor needs enough octaves to carry fine grain on top of the rolling base"
        );
        assert!(!preset.params.layers.stones.enabled);
        assert!(!preset.params.layers.boulders.enabled);
        assert!(!preset.params.layers.ripples.enabled);
        assert!(!preset.params.layers.flow.enabled);
        // relief stays modest — soil, not dunes or mountains.
        assert!(preset.params.relief_mm <= 5.0);
    }

    #[test]
    fn get_landscape_presets_matches_seed_presets() {
        assert_eq!(
            get_landscape_presets().len(),
            seed_presets().len()
        );
    }

    #[test]
    fn parse_landscape_token_handles_every_token_type() {
        assert_eq!(
            parse_landscape_token(r#"GENERATING {"seed": 7}"#),
            Some(LandscapeToken::Generating { seed: 7 })
        );
        // Without a "glb" key (old script/params-file mismatch) the parser
        // must not choke — glb comes back None.
        assert_eq!(
            parse_landscape_token(
                r#"GENERATED {"out": "/l.stl", "dims_mm": [120.0, 80.0, 8.0], "verts": 100, "manifold": true}"#
            ),
            Some(LandscapeToken::Generated {
                out: "/l.stl".to_string(),
                glb: None,
                dims_mm: [120.0, 80.0, 8.0],
                verts: 100,
                manifold: true,
            })
        );
        // With "glb" (the current script always sends it) it's captured.
        assert_eq!(
            parse_landscape_token(
                r#"GENERATED {"out": "/l.stl", "glb": "/l.glb", "dims_mm": [120.0, 80.0, 8.0], "verts": 100, "manifold": true}"#
            ),
            Some(LandscapeToken::Generated {
                out: "/l.stl".to_string(),
                glb: Some("/l.glb".to_string()),
                dims_mm: [120.0, 80.0, 8.0],
                verts: 100,
                manifold: true,
            })
        );
        // The script also reports the EFFECTIVE grid step (it may coarsen a
        // too-fine request to fit the vertex budget); the parser must accept
        // the extra field even though nothing consumes it yet.
        assert!(parse_landscape_token(
            r#"GENERATED {"out": "/l.stl", "glb": "/l.glb", "dims_mm": [120.0, 80.0, 8.0], "verts": 100, "manifold": true, "resolution_mm": 0.245}"#
        )
        .is_some());
        assert_eq!(
            parse_landscape_token(r#"GENERATION_FAILED {"reason": "bad params"}"#),
            Some(LandscapeToken::GenerationFailed {
                reason: "bad params".to_string(),
            })
        );
    }

    #[test]
    fn parse_landscape_token_rejects_garbage_and_blender_noise() {
        assert_eq!(parse_landscape_token(""), None);
        assert_eq!(parse_landscape_token("   "), None);
        assert_eq!(parse_landscape_token("GENERATING not-json"), None);
        assert_eq!(parse_landscape_token("random log line from Blender"), None);
        assert_eq!(
            parse_landscape_token("Blender 5.1.2 (hash abcdef1234 built 2025-01-01)"),
            None
        );
        assert_eq!(parse_landscape_token(r#"GENERATED {"out": "/l.stl"}"#), None);
    }

    #[test]
    fn gen_landscape_command_has_expected_shape() {
        let blender = BlenderInfo {
            path: "/usr/bin/blender".to_string(),
            version: "Blender 5.1.2".to_string(),
        };
        let cmd = build_gen_landscape_command(
            &blender,
            Path::new("/tmp/gen_landscape.py"),
            Path::new("/tmp/params.json"),
        );
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            vec![
                "--background",
                "--factory-startup",
                "--python-exit-code",
                "1",
                "--python",
                "/tmp/gen_landscape.py",
                "--",
                "--params",
                "/tmp/params.json",
            ]
        );
    }

    #[test]
    fn write_params_file_writes_readable_json_with_out() {
        let dir = std::env::temp_dir().join(format!("stlpack_landscape_unit_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let params = LandscapeParams {
            seed: 5,
            width_mm: 120.0,
            depth_mm: 80.0,
            resolution_mm: 0.75,
            feature_scale: 1.0,
            carrier_mm: 2.0,
            relief_mm: 6.0,
            layers: LandscapeLayers::default(),
            palette: MaterialPalette::default(),
        };
        let out_path = dir.join("landscape.stl");
        let path = write_params_file(&dir, &params, &out_path, "abc123").unwrap();
        assert!(path.is_file());
        let contents = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(value["seed"], 5);
        assert_eq!(value["out"], out_path.to_string_lossy().into_owned());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// End-to-end: bake every seed preset for real through spawn_and_parse
    /// (NOT the tauri command layer), assert each comes back GENERATED +
    /// manifold, and print dims/verts so a human can sanity-check them.
    ///
    /// Run with: cargo test -- --ignored bakes_every_seed_preset
    #[tokio::test]
    #[ignore = "requires a local Blender install and ~30s"]
    async fn bakes_every_seed_preset() {
        let blender = crate::render::engine::detect_blender()
            .await
            .expect("Blender not found — install it or set BLENDER_BIN");
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/gen_landscape.py");
        let dir = std::env::temp_dir().join(format!("stlpack_landscape_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        for preset in seed_presets() {
            let out_path = dir.join(format!("{}.stl", preset.id));
            let params_path =
                write_params_file(&dir, &preset.params, &out_path, &preset.id).expect("write params file");

            let cancel_token = Notify::new();
            let mut tokens: Vec<LandscapeToken> = Vec::new();
            let result = spawn_and_parse(&blender, &script, &params_path, &cancel_token, |token| {
                tokens.push(token.clone());
            })
            .await;

            let (out, glb, dims_mm, verts, manifold) = match result {
                Ok(v) => v,
                Err((e, tail)) => panic!("preset '{}' failed: {e}\nstdout tail:\n{tail}", preset.id),
            };
            println!(
                "preset '{}': out={out} glb={glb:?} dims_mm={:?} verts={verts} manifold={manifold}",
                preset.id, dims_mm
            );
            assert!(manifold, "preset '{}' produced a non-manifold mesh", preset.id);
            assert!(Path::new(&out).is_file(), "expected an STL at {:?}", out);
            assert!(
                tokens.iter().any(|t| matches!(t, LandscapeToken::Generating { .. })),
                "preset '{}': expected a GENERATING token",
                preset.id
            );

            // The GLB twin (VTT GLB export design doc convention 4): same
            // stem, next to the STL, real glTF-binary content.
            let glb_path = glb.unwrap_or_else(|| panic!("preset '{}': GENERATED had no glb", preset.id));
            assert!(
                Path::new(&glb_path).is_file(),
                "expected a GLB twin at {:?}",
                glb_path
            );
            let bytes = std::fs::read(&glb_path).expect("read glb twin");
            assert!(
                bytes.len() >= 4 && &bytes[0..4] == b"glTF",
                "preset '{}': {:?} doesn't start with the glTF magic",
                preset.id,
                glb_path
            );
        }

        std::fs::remove_dir_all(&dir).ok();
    }
}
