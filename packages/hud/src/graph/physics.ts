/**
 * A small force simulation, written by hand rather than pulled from a library.
 *
 * The layout is not decorative — every force encodes something true about the
 * fleet, so where a node sits is readable:
 *
 *   · distance from the core  = disengagement. A hot agent is pulled in tight;
 *                               one that has gone quiet drifts outward.
 *   · a link between agents   = they share a working directory, which under
 *                               this repo's charter means they can collide.
 *   · clustering              = same harness.
 *
 * Pure and deterministic given the same inputs, so the tests can assert on
 * settled positions instead of eyeballing a canvas.
 */

export interface Body {
  id: string;
  x: number;
  y: number;
  vx: number;
  vy: number;
  /** Heavier bodies move less. Driven by token burn. */
  mass: number;
  /** 0..1 — how engaged the agent is; shortens its tether to the core. */
  engagement: number;
  /** Bodies sharing a group weakly attract (same harness). */
  group: string;
  /** Held in place by the operator (dragging), so forces do not fight them. */
  pinned?: boolean;
}

export interface Link {
  a: string;
  b: string;
  /** 0..1 — how strongly they bind. Contention strength. */
  weight: number;
}

export interface SimParams {
  /** Node-node separation. */
  repulsion: number;
  /** Core tether at zero engagement. */
  tetherLength: number;
  /** How much full engagement shortens the tether. */
  tetherPull: number;
  tetherStiffness: number;
  linkStiffness: number;
  linkLength: number;
  groupCohesion: number;
  damping: number;
  /** Clamp so a burst of forces can never fling a node off-screen. */
  maxSpeed: number;
}

export const DEFAULT_PARAMS: SimParams = {
  repulsion: 52000,
  tetherLength: 300,
  tetherPull: 130,
  tetherStiffness: 0.0075,
  linkStiffness: 0.012,
  linkLength: 170,
  groupCohesion: 0.0016,
  damping: 0.86,
  maxSpeed: 42,
};

const MIN_DIST = 26;

/**
 * Advance the simulation one step. Mutates `bodies` in place.
 *
 * `dt` is in frames (1 = a nominal 60fps tick), not milliseconds, so a dropped
 * frame slows the animation rather than exploding the integrator.
 */
export function step(
  bodies: Body[],
  links: Link[],
  params: SimParams = DEFAULT_PARAMS,
  dt = 1,
  centre = { x: 0, y: 0 },
): void {
  const n = bodies.length;
  if (n === 0) return;
  const clamped = Math.min(dt, 3);

  // Pairwise repulsion — O(n²), which is free at fleet scale (tens of agents).
  for (let i = 0; i < n; i++) {
    const a = bodies[i];
    for (let j = i + 1; j < n; j++) {
      const b = bodies[j];
      let dx = b.x - a.x;
      let dy = b.y - a.y;
      let d2 = dx * dx + dy * dy;
      if (d2 < 1e-6) {
        // Perfectly coincident nodes have no direction to separate along;
        // nudge them apart deterministically using their index.
        dx = (i - j) * 0.01 + 0.01;
        dy = (j - i) * 0.007 + 0.007;
        d2 = dx * dx + dy * dy;
      }
      const d = Math.sqrt(d2);
      const eff = Math.max(d, MIN_DIST);
      const f = params.repulsion / (eff * eff);
      const ux = dx / d;
      const uy = dy / d;
      a.vx -= (ux * f * clamped) / a.mass;
      a.vy -= (uy * f * clamped) / a.mass;
      b.vx += (ux * f * clamped) / b.mass;
      b.vy += (uy * f * clamped) / b.mass;
    }
  }

  const index = new Map<string, Body>();
  for (const b of bodies) index.set(b.id, b);

  // Contention links: agents sharing a working directory pull together.
  for (const link of links) {
    const a = index.get(link.a);
    const b = index.get(link.b);
    if (!a || !b) continue;
    const dx = b.x - a.x;
    const dy = b.y - a.y;
    const d = Math.hypot(dx, dy) || 1;
    const f = (d - params.linkLength) * params.linkStiffness * link.weight;
    const ux = dx / d;
    const uy = dy / d;
    a.vx += (ux * f * clamped) / a.mass;
    a.vy += (uy * f * clamped) / a.mass;
    b.vx -= (ux * f * clamped) / b.mass;
    b.vy -= (uy * f * clamped) / b.mass;
  }

  // Same-harness cohesion, applied toward each group's centroid.
  const centroids = groupCentroids(bodies);
  for (const b of bodies) {
    const c = centroids.get(b.group);
    if (!c || c.count < 2) continue;
    b.vx += (c.x - b.x) * params.groupCohesion * clamped;
    b.vy += (c.y - b.y) * params.groupCohesion * clamped;
  }

  // Core tether — the semantic force. Engagement shortens the rest length.
  for (const b of bodies) {
    const dx = b.x - centre.x;
    const dy = b.y - centre.y;
    const d = Math.hypot(dx, dy) || 1;
    const rest = params.tetherLength - params.tetherPull * clamp01(b.engagement);
    const f = (d - rest) * params.tetherStiffness;
    b.vx -= (dx / d) * f * clamped;
    b.vy -= (dy / d) * f * clamped;
  }

  // Integrate.
  for (const b of bodies) {
    if (b.pinned) {
      b.vx = 0;
      b.vy = 0;
      continue;
    }
    b.vx *= params.damping;
    b.vy *= params.damping;
    const speed = Math.hypot(b.vx, b.vy);
    if (speed > params.maxSpeed) {
      b.vx = (b.vx / speed) * params.maxSpeed;
      b.vy = (b.vy / speed) * params.maxSpeed;
    }
    b.x += b.vx * clamped;
    b.y += b.vy * clamped;
  }
}

interface Centroid {
  x: number;
  y: number;
  count: number;
}

export function groupCentroids(bodies: Body[]): Map<string, Centroid> {
  const acc = new Map<string, Centroid>();
  for (const b of bodies) {
    const c = acc.get(b.group) ?? { x: 0, y: 0, count: 0 };
    c.x += b.x;
    c.y += b.y;
    c.count += 1;
    acc.set(b.group, c);
  }
  for (const c of acc.values()) {
    if (c.count > 0) {
      c.x /= c.count;
      c.y /= c.count;
    }
  }
  return acc;
}

export function clamp01(v: number): number {
  return v < 0 ? 0 : v > 1 ? 1 : v;
}

/** Total kinetic energy — used to tell whether the layout has settled. */
export function energy(bodies: Body[]): number {
  let sum = 0;
  for (const b of bodies) sum += b.mass * (b.vx * b.vx + b.vy * b.vy);
  return sum;
}

/**
 * Deterministic opening position on a golden-angle spiral.
 *
 * Seeding by index rather than at random keeps a reload visually stable, and
 * keeps the tests free of `Math.random`.
 */
export function seedPosition(index: number, radius = 260): { x: number; y: number } {
  const golden = Math.PI * (3 - Math.sqrt(5));
  const angle = index * golden;
  const r = radius * Math.sqrt((index + 0.5) / Math.max(1, index + 1)) + (index % 3) * 18;
  return { x: Math.cos(angle) * r, y: Math.sin(angle) * r };
}
