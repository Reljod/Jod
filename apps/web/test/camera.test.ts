import { describe, expect, it } from "vitest";
import { easeCamera, fitCamera } from "../src/graph/camera";
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
