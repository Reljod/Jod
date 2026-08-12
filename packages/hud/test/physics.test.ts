import { describe, expect, it } from "vitest";
import {
  DEFAULT_PARAMS,
  energy,
  groupCentroids,
  seedPosition,
  step,
  type Body,
} from "../src/graph/physics";

function body(id: string, x: number, y: number, over: Partial<Body> = {}): Body {
  return { id, x, y, vx: 0, vy: 0, mass: 1, engagement: 0, group: "g", ...over };
}

function settle(bodies: Body[], links: Parameters<typeof step>[1] = [], n = 600): void {
  for (let i = 0; i < n; i++) step(bodies, links, DEFAULT_PARAMS, 1);
}

describe("step", () => {
  it("separates two coincident bodies instead of dividing by zero", () => {
    const a = body("a", 0, 0);
    const b = body("b", 0, 0);
    step([a, b], [], DEFAULT_PARAMS, 1);

    const d = Math.hypot(a.x - b.x, a.y - b.y);
    expect(Number.isFinite(d)).toBe(true);
    expect(d).toBeGreaterThan(0);
  });

  it("pushes overlapping bodies apart", () => {
    const a = body("a", -4, 0);
    const b = body("b", 4, 0);
    const before = Math.hypot(a.x - b.x, a.y - b.y);
    settle([a, b], [], 200);

    expect(Math.hypot(a.x - b.x, a.y - b.y)).toBeGreaterThan(before);
  });

  it("settles: kinetic energy decays toward zero", () => {
    const bodies = [body("a", 30, 10), body("b", -20, 40), body("c", 5, -60)];
    settle(bodies, [], 100);
    const mid = energy(bodies);
    settle(bodies, [], 900);

    expect(energy(bodies)).toBeLessThan(mid);
    expect(energy(bodies)).toBeLessThan(1);
  });

  it("seats an engaged agent closer to the core than a disengaged one", () => {
    // The load-bearing claim of the layout: radius reads as disengagement.
    const hot = [body("hot", 400, 0, { engagement: 1 })];
    const cold = [body("cold", 400, 0, { engagement: 0 })];
    settle(hot);
    settle(cold);

    const rHot = Math.hypot(hot[0].x, hot[0].y);
    const rCold = Math.hypot(cold[0].x, cold[0].y);

    expect(rHot).toBeLessThan(rCold);
    // And by roughly the tether budget it was given.
    expect(rCold - rHot).toBeGreaterThan(DEFAULT_PARAMS.tetherPull * 0.5);
  });

  it("binds linked bodies closer than unlinked ones", () => {
    const linked = [body("a", -300, 0), body("b", 300, 0)];
    const loose = [body("a", -300, 0), body("b", 300, 0)];
    settle(linked, [{ a: "a", b: "b", weight: 1 }]);
    settle(loose, []);

    const dLinked = Math.hypot(linked[0].x - linked[1].x, linked[0].y - linked[1].y);
    const dLoose = Math.hypot(loose[0].x - loose[1].x, loose[0].y - loose[1].y);
    expect(dLinked).toBeLessThan(dLoose);
  });

  it("never exceeds the speed clamp, even under an extreme force", () => {
    const a = body("a", 0, 0);
    const b = body("b", 0.0001, 0);
    for (let i = 0; i < 40; i++) {
      step([a, b], [], DEFAULT_PARAMS, 1);
      expect(Math.hypot(a.vx, a.vy)).toBeLessThanOrEqual(DEFAULT_PARAMS.maxSpeed + 1e-6);
      expect(Math.hypot(b.vx, b.vy)).toBeLessThanOrEqual(DEFAULT_PARAMS.maxSpeed + 1e-6);
    }
  });

  it("leaves a pinned body exactly where the operator put it", () => {
    const pinned = body("p", 120, 40, { pinned: true });
    const other = body("o", 130, 45);
    settle([pinned, other], [], 200);

    expect(pinned.x).toBe(120);
    expect(pinned.y).toBe(40);
  });

  it("is deterministic — same input, same settled positions", () => {
    const run = () => {
      const bodies = [body("a", 10, 20), body("b", -30, 5), body("c", 60, -40)];
      settle(bodies, [{ a: "a", b: "b", weight: 1 }], 300);
      return bodies.map((b) => [b.x, b.y]);
    };
    expect(run()).toEqual(run());
  });

  it("tolerates a link naming a body that is not in the list", () => {
    const bodies = [body("a", 10, 0)];
    expect(() => step(bodies, [{ a: "a", b: "ghost", weight: 1 }])).not.toThrow();
    expect(Number.isFinite(bodies[0].x)).toBe(true);
  });

  it("does nothing with no bodies", () => {
    expect(() => step([], [])).not.toThrow();
  });

  it("slows rather than destabilises when frames are dropped", () => {
    // dt is clamped, so a 10-frame stall cannot fling a node off-screen.
    const bodies = [body("a", 5, 0), body("b", -5, 0)];
    step(bodies, [], DEFAULT_PARAMS, 10);
    for (const b of bodies) {
      expect(Math.abs(b.x)).toBeLessThan(1000);
      expect(Number.isFinite(b.x)).toBe(true);
    }
  });
});

describe("groupCentroids", () => {
  it("averages each group independently", () => {
    const c = groupCentroids([
      { ...body("a", 0, 0), group: "x" },
      { ...body("b", 10, 20), group: "x" },
      { ...body("c", 100, 100), group: "y" },
    ]);
    expect(c.get("x")).toEqual({ x: 5, y: 10, count: 2 });
    expect(c.get("y")).toEqual({ x: 100, y: 100, count: 1 });
  });
});

describe("seedPosition", () => {
  it("is deterministic and spreads nodes apart", () => {
    expect(seedPosition(3)).toEqual(seedPosition(3));
    const a = seedPosition(0);
    const b = seedPosition(1);
    expect(Math.hypot(a.x - b.x, a.y - b.y)).toBeGreaterThan(1);
  });
});
