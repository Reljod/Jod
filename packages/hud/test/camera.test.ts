import { describe, expect, it } from "vitest";
import { ZOOM_MAX, ZOOM_MIN, easeCamera, fitCamera, zoomAt } from "../src/graph/camera";
import type { Body } from "../src/graph/physics";

function body(id: string, x: number, y: number): Body {
  return { id, x, y, vx: 0, vy: 0, mass: 1, engagement: 0, group: "g" };
}

const VIEW = { w: 1120, h: 750 };

/** Is a world point inside the viewport under this camera? */
function onScreen(p: { x: number; y: number }, cam: ReturnType<typeof fitCamera>, view = VIEW) {
  const sx = (p.x - cam.x) * cam.zoom + view.w / 2;
  const sy = (p.y - cam.y) * cam.zoom + view.h / 2;
  return sx >= 0 && sx <= view.w && sy >= 0 && sy <= view.h;
}

describe("fitCamera", () => {
  it("falls back to the origin with no bodies", () => {
    expect(fitCamera([], VIEW)).toEqual({ x: 0, y: 0, zoom: 1 });
  });

  it("keeps every node on screen, including one far out", () => {
    // The regression this exists for: one agent drifted off the top edge.
    const bodies = [
      body("a", 0, -340),
      body("b", 300, 100),
      body("c", -280, 160),
      body("d", 120, 330),
    ];
    const cam = fitCamera(bodies, VIEW);
    for (const b of bodies) {
      expect(onScreen(b, cam)).toBe(true);
    }
  });

  it("keeps the core on screen even when agents bunch to one side", () => {
    const bodies = [body("a", 900, 900), body("b", 1000, 950)];
    const cam = fitCamera(bodies, VIEW);
    expect(onScreen({ x: 0, y: 0 }, cam)).toBe(true);
  });

  it("zooms out as the fleet spreads", () => {
    const tight = fitCamera([body("a", -50, 0), body("b", 50, 0)], VIEW);
    const wide = fitCamera([body("a", -1400, 0), body("b", 1400, 0)], VIEW);
    expect(wide.zoom).toBeLessThan(tight.zoom);
  });

  it("never zooms past its bounds", () => {
    const huge = fitCamera([body("a", -9e4, -9e4), body("b", 9e4, 9e4)], VIEW);
    const tiny = fitCamera([body("a", 0, 0)], VIEW);
    expect(huge.zoom).toBeGreaterThanOrEqual(0.3);
    expect(tiny.zoom).toBeLessThanOrEqual(1.15);
  });

  it("centres on the midpoint of the extent, counting the core and node radii", () => {
    const r = 56;
    const bodies = [body("a", 100, 40), body("b", 300, 160)];
    const cam = fitCamera(bodies, VIEW, r);

    // The extent spans the core at (0,0) plus each body padded by its radius.
    const xs = [0, ...bodies.flatMap((b) => [b.x - r, b.x + r])];
    const ys = [0, ...bodies.flatMap((b) => [b.y - r, b.y + r])];
    expect(cam.x).toBeCloseTo((Math.min(...xs) + Math.max(...xs)) / 2, 5);
    expect(cam.y).toBeCloseTo((Math.min(...ys) + Math.max(...ys)) / 2, 5);
  });

  it("stays centred on the core when the fleet is symmetric around it", () => {
    const cam = fitCamera([body("a", -200, 0), body("b", 200, 0)], VIEW);
    expect(cam.x).toBeCloseTo(0, 5);
    expect(cam.y).toBeCloseTo(0, 5);
  });

  it("survives a zero-sized viewport without dividing by zero", () => {
    const cam = fitCamera([body("a", 10, 10)], { w: 0, h: 0 });
    expect(Number.isFinite(cam.zoom)).toBe(true);
  });
});

describe("zoomAt", () => {
  /** Where a world point lands on screen under a camera. */
  const project = (p: { x: number; y: number }, cam: { x: number; y: number; zoom: number }) => ({
    x: (p.x - cam.x) * cam.zoom + VIEW.w / 2,
    y: (p.y - cam.y) * cam.zoom + VIEW.h / 2,
  });

  it("holds the world point under the anchor still", () => {
    const cam = { x: 40, y: -30, zoom: 0.8 };
    const anchor = { x: 300, y: 620 };
    const before = { x: (anchor.x - VIEW.w / 2) / cam.zoom + cam.x, y: (anchor.y - VIEW.h / 2) / cam.zoom + cam.y };

    zoomAt(cam, 1.6, anchor, VIEW);

    const after = project(before, cam);
    expect(after.x).toBeCloseTo(anchor.x, 6);
    expect(after.y).toBeCloseTo(anchor.y, 6);
  });

  it("leaves the centre fixed when the anchor is the centre", () => {
    const cam = { x: 40, y: -30, zoom: 0.8 };
    zoomAt(cam, 1.6, { x: VIEW.w / 2, y: VIEW.h / 2 }, VIEW);
    expect(cam.x).toBeCloseTo(40, 6);
    expect(cam.y).toBeCloseTo(-30, 6);
    expect(cam.zoom).toBeCloseTo(1.28, 6);
  });

  it("pulls the anchored point toward the centre when zooming in", () => {
    // Zooming in on a node in the corner has to move the camera toward it.
    const cam = { x: 0, y: 0, zoom: 1 };
    zoomAt(cam, 2, { x: VIEW.w, y: VIEW.h }, VIEW);
    expect(cam.x).toBeGreaterThan(0);
    expect(cam.y).toBeGreaterThan(0);
  });

  it("clamps at both bounds", () => {
    const cam = { x: 0, y: 0, zoom: 1 };
    const anchor = { x: 10, y: 10 };
    for (let i = 0; i < 60; i++) zoomAt(cam, 1.3, anchor, VIEW);
    expect(cam.zoom).toBeCloseTo(ZOOM_MAX, 6);
    for (let i = 0; i < 200; i++) zoomAt(cam, 1 / 1.3, anchor, VIEW);
    expect(cam.zoom).toBeCloseTo(ZOOM_MIN, 6);
  });

  it("keeps the anchor fixed even when the zoom clamps mid-step", () => {
    const cam = { x: 0, y: 0, zoom: ZOOM_MAX / 1.1 };
    const anchor = { x: 200, y: 100 };
    const before = { x: (anchor.x - VIEW.w / 2) / cam.zoom + cam.x, y: (anchor.y - VIEW.h / 2) / cam.zoom + cam.y };

    zoomAt(cam, 10, anchor, VIEW); // asks for far more than the bound allows

    expect(cam.zoom).toBeCloseTo(ZOOM_MAX, 6);
    const after = project(before, cam);
    expect(after.x).toBeCloseTo(anchor.x, 6);
    expect(after.y).toBeCloseTo(anchor.y, 6);
  });

  it("is a no-op once already at the bound", () => {
    const cam = { x: 12, y: -8, zoom: ZOOM_MAX };
    expect(zoomAt(cam, 2, { x: 0, y: 0 }, VIEW)).toBe(1);
    expect(cam).toEqual({ x: 12, y: -8, zoom: ZOOM_MAX });
  });

  it("round-trips: zoom in then out by the same factor restores the camera", () => {
    const cam = { x: 25, y: 60, zoom: 0.9 };
    const anchor = { x: 880, y: 210 };
    zoomAt(cam, 1.45, anchor, VIEW);
    zoomAt(cam, 1 / 1.45, anchor, VIEW);
    expect(cam.x).toBeCloseTo(25, 6);
    expect(cam.y).toBeCloseTo(60, 6);
    expect(cam.zoom).toBeCloseTo(0.9, 6);
  });

  it("can zoom past what auto-fit would choose", () => {
    // The point of a manual camera: go closer than the framed-everything zoom.
    const fitted = fitCamera([body("a", -1400, 0), body("b", 1400, 0)], VIEW);
    const cam = { ...fitted };
    for (let i = 0; i < 12; i++) zoomAt(cam, 1.3, { x: VIEW.w / 2, y: VIEW.h / 2 }, VIEW);
    expect(cam.zoom).toBeGreaterThan(1.15);
  });
});

describe("easeCamera", () => {
  it("converges toward the target without overshooting", () => {
    const cam = { x: 0, y: 0, zoom: 1 };
    const target = { x: 100, y: -50, zoom: 0.5 };
    for (let i = 0; i < 400; i++) easeCamera(cam, target);

    expect(cam.x).toBeCloseTo(100, 1);
    expect(cam.y).toBeCloseTo(-50, 1);
    expect(cam.zoom).toBeCloseTo(0.5, 2);
  });

  it("moves monotonically toward the target", () => {
    const cam = { x: 0, y: 0, zoom: 1 };
    const target = { x: 100, y: 0, zoom: 1 };
    let prev = cam.x;
    for (let i = 0; i < 30; i++) {
      easeCamera(cam, target);
      expect(cam.x).toBeGreaterThan(prev);
      expect(cam.x).toBeLessThanOrEqual(100);
      prev = cam.x;
    }
  });
});
