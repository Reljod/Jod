import type { Body } from "./physics";

export interface Viewport {
  w: number;
  h: number;
}

export interface CameraTarget {
  x: number;
  y: number;
  zoom: number;
}

/** Keep the core visible even when every agent has drifted to one side. */
const INCLUDE_CORE = true;
const PADDING = 96;
const MIN_ZOOM = 0.3;
const MAX_ZOOM = 1.15;

/**
 * Where the camera should sit so the whole fleet is on screen.
 *
 * The layout is force-directed, so the graph's extent changes as agents spawn,
 * heat up and drift out. Tuning the force constants until things happen to fit
 * one viewport would break on the next window size; framing the actual bounding
 * box does not. The operator can still pan and zoom, and doing so switches this
 * off until they ask to recentre.
 */
export function fitCamera(bodies: Body[], view: Viewport, nodeRadius = 56): CameraTarget {
  if (bodies.length === 0 || view.w === 0 || view.h === 0) {
    return { x: 0, y: 0, zoom: 1 };
  }

  let minX = INCLUDE_CORE ? 0 : Infinity;
  let maxX = INCLUDE_CORE ? 0 : -Infinity;
  let minY = INCLUDE_CORE ? 0 : Infinity;
  let maxY = INCLUDE_CORE ? 0 : -Infinity;

  for (const b of bodies) {
    // Nodes are drawn with labels and orbiting satellites around them, so the
    // extent that must fit is wider than the body's own coordinate.
    minX = Math.min(minX, b.x - nodeRadius);
    maxX = Math.max(maxX, b.x + nodeRadius);
    minY = Math.min(minY, b.y - nodeRadius);
    maxY = Math.max(maxY, b.y + nodeRadius);
  }

  const width = Math.max(1, maxX - minX) + PADDING * 2;
  const height = Math.max(1, maxY - minY) + PADDING * 2;

  const zoom = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, Math.min(view.w / width, view.h / height)));

  return { x: (minX + maxX) / 2, y: (minY + maxY) / 2, zoom };
}

/** Ease a camera toward a target. `k` is the per-frame fraction closed. */
export function easeCamera(cam: CameraTarget, target: CameraTarget, k = 0.045): void {
  cam.x += (target.x - cam.x) * k;
  cam.y += (target.y - cam.y) * k;
  cam.zoom += (target.zoom - cam.zoom) * k;
}
