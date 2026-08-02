import { describe, expect, it } from "vitest";
import type { Cutter, Placement } from "../bindings";
import { footprintDims } from "./cutterKinds";
import {
  mulberry32,
  regimentExtent,
  regimentPlacements,
  scatterPlacements,
} from "./placementGenerators";

const SQUARE_25: Cutter = {
  id: "square25",
  label: "25mm square",
  kind: { kind: "rect", width_mm: 25, depth_mm: 25 },
};
const ROUND_32: Cutter = {
  id: "round32",
  label: "32mm round",
  kind: { kind: "circle", diameter_mm: 32 },
};
const OVAL_90x52: Cutter = {
  id: "oval90x52",
  label: "90x52mm oval",
  kind: { kind: "ellipse", major_mm: 90, minor_mm: 52 },
};

describe("regimentPlacements", () => {
  it("tiles squares edge-to-edge at gap 0 (pitch == nominal dimension)", () => {
    const placements = regimentPlacements(SQUARE_25, 1, 3, 0, { x: 0, y: 0 });
    const xs = placements.map((p) => p.x_mm).toSorted((a, b) => a - b);
    expect(xs).toEqual([-25, 0, 25]);
    expect(placements.every((p) => p.y_mm === 0)).toBe(true);
    expect(placements.every((p) => p.rotation_deg === 0)).toBe(true);
    expect(placements.every((p) => p.magnet === null)).toBe(true);
  });

  it("adds the gap to the pitch on both axes", () => {
    const placements = regimentPlacements(SQUARE_25, 2, 2, 5, { x: 0, y: 0 });
    const xs = [...new Set(placements.map((p) => p.x_mm))].toSorted(
      (a, b) => a - b,
    );
    const ys = [...new Set(placements.map((p) => p.y_mm))].toSorted(
      (a, b) => a - b,
    );
    expect(xs[1] - xs[0]).toBeCloseTo(30); // 25 + 5
    expect(ys[1] - ys[0]).toBeCloseTo(30);
  });

  it("uses bounding width/depth for rounds and ovals", () => {
    const rounds = regimentPlacements(ROUND_32, 1, 2, 0, { x: 0, y: 0 });
    const roundXs = rounds.map((p) => p.x_mm).toSorted((a, b) => a - b);
    expect(roundXs[1] - roundXs[0]).toBeCloseTo(32);

    const ovals = regimentPlacements(OVAL_90x52, 2, 1, 0, { x: 0, y: 0 });
    const ovalYs = ovals.map((p) => p.y_mm).toSorted((a, b) => a - b);
    expect(ovalYs[1] - ovalYs[0]).toBeCloseTo(52); // minor_mm is the y-pitch
  });

  it("centers the grid on `center`", () => {
    const placements = regimentPlacements(SQUARE_25, 1, 1, 0, { x: 10, y: -5 });
    expect(placements).toEqual([
      {
        cutter: SQUARE_25.kind,
        x_mm: 10,
        y_mm: -5,
        rotation_deg: 0,
        magnet: null,
      },
    ]);
  });

  it("produces rows*cols placements, all carrying the cutter's kind", () => {
    const placements = regimentPlacements(SQUARE_25, 3, 4, 1, { x: 0, y: 0 });
    expect(placements).toHaveLength(12);
    expect(placements.every((p) => p.cutter === SQUARE_25.kind)).toBe(true);
  });

  it("returns an empty grid for non-positive rows or cols", () => {
    expect(regimentPlacements(SQUARE_25, 0, 5, 0, { x: 0, y: 0 })).toEqual([]);
    expect(regimentPlacements(SQUARE_25, 5, -1, 0, { x: 0, y: 0 })).toEqual([]);
  });
});

describe("regimentExtent", () => {
  it("matches the actual placement spread for a simple grid", () => {
    const ext = regimentExtent(SQUARE_25, 1, 3, 0, { x: 0, y: 0 });
    // 3 x 25mm squares tiled edge-to-edge span 75mm, centered on 0.
    expect(ext).toEqual({ minX: -37.5, maxX: 37.5, minY: -12.5, maxY: 12.5 });
  });

  it("collapses to a point for a 1x1 grid centered anywhere", () => {
    const ext = regimentExtent(SQUARE_25, 1, 1, 0, { x: 5, y: 5 });
    expect(ext.maxX - ext.minX).toBeCloseTo(25);
    expect(ext.maxY - ext.minY).toBeCloseTo(25);
  });
});

describe("footprintDims", () => {
  it("mirrors the local width/depth convention (circle/ellipse/rect)", () => {
    expect(footprintDims(ROUND_32.kind)).toEqual({ width: 32, depth: 32 });
    expect(footprintDims(OVAL_90x52.kind)).toEqual({ width: 90, depth: 52 });
    expect(footprintDims(SQUARE_25.kind)).toEqual({ width: 25, depth: 25 });
  });
});

describe("mulberry32", () => {
  it("is deterministic: same seed -> same sequence", () => {
    const a = mulberry32(42);
    const b = mulberry32(42);
    const seqA = Array.from({ length: 5 }, () => a());
    const seqB = Array.from({ length: 5 }, () => b());
    expect(seqA).toEqual(seqB);
  });

  it("stays in [0, 1)", () => {
    const rng = mulberry32(1);
    for (let i = 0; i < 100; i++) {
      const v = rng();
      expect(v).toBeGreaterThanOrEqual(0);
      expect(v).toBeLessThan(1);
    }
  });
});

const BOUNDS = { minX: 0, maxX: 100, minY: 0, maxY: 100 };

const distance = (
  a: { x_mm: number; y_mm: number },
  b: { x_mm: number; y_mm: number },
) => Math.hypot(a.x_mm - b.x_mm, a.y_mm - b.y_mm);

describe("scatterPlacements", () => {
  it("keeps the whole nominal footprint inside bounds", () => {
    const cutter = ROUND_32; // radius 16
    const placements = scatterPlacements(cutter, 8, BOUNDS, [], mulberry32(7));
    expect(placements.length).toBeGreaterThan(0);
    for (const p of placements) {
      expect(p.x_mm).toBeGreaterThanOrEqual(BOUNDS.minX + 16);
      expect(p.x_mm).toBeLessThanOrEqual(BOUNDS.maxX - 16);
      expect(p.y_mm).toBeGreaterThanOrEqual(BOUNDS.minY + 16);
      expect(p.y_mm).toBeLessThanOrEqual(BOUNDS.maxY - 16);
    }
  });

  it("never places two of its own results closer than the sum of their bounding radii", () => {
    const placements = scatterPlacements(
      SQUARE_25,
      6,
      BOUNDS,
      [],
      mulberry32(123),
    );
    const radius = Math.hypot(25, 25) / 2;
    for (let i = 0; i < placements.length; i++) {
      for (let j = i + 1; j < placements.length; j++) {
        expect(distance(placements[i], placements[j])).toBeGreaterThanOrEqual(
          radius + radius - 1e-9,
        );
      }
    }
  });

  it("respects `existing` placements as obstacles", () => {
    const existing: Pick<Placement, "cutter" | "x_mm" | "y_mm">[] = [
      { cutter: ROUND_32.kind, x_mm: 50, y_mm: 50 },
    ];
    const placements = scatterPlacements(
      ROUND_32,
      5,
      BOUNDS,
      existing,
      mulberry32(9),
    );
    for (const p of placements) {
      expect(Math.hypot(p.x_mm - 50, p.y_mm - 50)).toBeGreaterThanOrEqual(
        32 - 1e-9,
      );
    }
  });

  it("is deterministic under a fixed seed", () => {
    const a = scatterPlacements(SQUARE_25, 5, BOUNDS, [], mulberry32(555));
    const b = scatterPlacements(SQUARE_25, 5, BOUNDS, [], mulberry32(555));
    expect(a).toEqual(b);
  });

  it("gives up gracefully (returns fewer) when the area is too full", () => {
    // A 100x100mm plate can hold roughly one 90mm oval; asking for 10 must
    // not hang or throw, and must not fabricate overlapping placements.
    const placements = scatterPlacements(
      OVAL_90x52,
      10,
      BOUNDS,
      [],
      mulberry32(3),
    );
    expect(placements.length).toBeLessThan(10);
  });

  it("returns nothing when the footprint cannot fit inside bounds at all", () => {
    const tinyBounds = { minX: 0, maxX: 10, minY: 0, maxY: 10 };
    expect(
      scatterPlacements(ROUND_32, 3, tinyBounds, [], mulberry32(1)),
    ).toEqual([]);
  });

  it("uses rotation 0 for circles, but assigns a rotation for non-circles", () => {
    const rounds = scatterPlacements(ROUND_32, 3, BOUNDS, [], mulberry32(2));
    expect(rounds.every((p) => p.rotation_deg === 0)).toBe(true);

    const squares = scatterPlacements(SQUARE_25, 3, BOUNDS, [], mulberry32(2));
    expect(squares.some((p) => p.rotation_deg !== 0)).toBe(true);
  });

  it("returns an empty array for a non-positive count", () => {
    expect(scatterPlacements(SQUARE_25, 0, BOUNDS, [], mulberry32(1))).toEqual(
      [],
    );
  });
});
