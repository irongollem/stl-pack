import { describe, expect, it } from "vitest";
import { linearToHex } from "./color";
import {
  isKnobDefault,
  LOOK_GROUPS,
  LOOK_KNOBS,
  overridesToNested,
  sanitizeOverrides,
} from "./renderLookSchema";

describe("renderLookSchema", () => {
  it("has unique dot-paths across all groups", () => {
    const paths = LOOK_GROUPS.flatMap((g) => g.knobs).map((k) => k.path);
    expect(new Set(paths).size).toBe(paths.length);
    expect(LOOK_KNOBS.size).toBe(paths.length);
  });

  it("keeps CLI-flag territory out of the schema", () => {
    // These are sent as --color/--res/--samples, which outrank --config;
    // a knob for them would be dead. See the header comment.
    for (const path of ["base_color", "res", "samples"]) {
      expect(LOOK_KNOBS.has(path)).toBe(false);
    }
  });

  it("sanitizes: keeps valid, clamps out-of-range, drops unknown/malformed", () => {
    const { overrides, dropped } = sanitizeOverrides({
      "key.energy": 5000,
      "marmoset.fill_energy_mult": 99, // clamps to max 2
      "key.loc": [0, 0, "up"], // malformed vec3
      "not.a.knob": 1,
      roughness: Number.NaN,
    });
    expect(overrides["key.energy"]).toBe(5000);
    expect(overrides["marmoset.fill_energy_mult"]).toBe(2);
    expect(dropped.toSorted()).toEqual(["key.loc", "not.a.knob", "roughness"]);
  });

  it("sanitizes non-objects to an empty diff", () => {
    for (const raw of [null, "garbage", 42, ["key.energy", 5000]]) {
      expect(sanitizeOverrides(raw).overrides).toEqual({});
    }
  });

  it("drops values that clamp back onto the default", () => {
    // key_energy_mult default 0.82, min 0.1 — 0.0000000001 clamps to 0.1,
    // stays; but exactly the default must NOT be stored as a tweak
    const { overrides } = sanitizeOverrides({ "rich.key_energy_mult": 0.82 });
    expect(overrides).toEqual({});
  });

  it("compares colors by hex so the 8-bit round-trip is not a tweak", () => {
    const knob = LOOK_KNOBS.get("key.color");
    if (!knob) throw new Error("key.color missing from schema");
    const roundTripped = [1, 0.8199, 0.5501] as [number, number, number];
    expect(linearToHex(roundTripped)).toBe(
      linearToHex(knob.default as [number, number, number]),
    );
    expect(isKnobDefault(knob, roundTripped)).toBe(true);
  });

  it("nests dot-paths into the shape render_mini.py merges onto LOOK", () => {
    expect(
      overridesToNested({
        roughness: 0.4,
        "key.energy": 1500,
        "key.size": 8,
        "marmoset.sss_weight_mult": 0.15,
      }),
    ).toEqual({
      roughness: 0.4,
      key: { energy: 1500, size: 8 },
      marmoset: { sss_weight_mult: 0.15 },
    });
  });
});
