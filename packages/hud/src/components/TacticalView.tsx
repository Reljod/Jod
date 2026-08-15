import { useCallback, useEffect, useRef, useState } from "react";
import type { Body, Link } from "../graph/physics";
import { DEFAULT_PARAMS, step } from "../graph/physics";
import { contentionLinks, rankForDisplay, syncBodies } from "../graph/model";
import { easeCamera, fitCamera, zoomAt } from "../graph/camera";
import type { Camera } from "../render/renderer";
import { TacticalRenderer } from "../render/renderer";
import type { WorldStore } from "../state/world";

const NODE_BUDGET = 48;
const PULSE_LIFETIME = 1000;

/** Per notch of a mouse wheel or point of trackpad scroll. */
const WHEEL_SENSITIVITY = 0.0014;
/**
 * A pinch arrives as a ctrl-modified wheel with much smaller deltas than a
 * scroll, so it needs its own multiplier to travel the same distance.
 */
const PINCH_SENSITIVITY = 0.01;
/** One press of the zoom buttons, or of `+` / `-`. */
const STEP_FACTOR = 1.3;

interface Props {
  store: WorldStore;
  selectedId: string | null;
  onSelect(id: string | null): void;
  /** Bumped by the parent to request a re-centre. */
  recentreNonce: number;
}

/**
 * Hosts the canvas and owns the simulation loop.
 *
 * Deliberately outside React's render cycle — the loop reads `store.world`
 * directly each frame, so a burst of a hundred events costs one frame of
 * physics rather than a hundred component renders.
 */
export function TacticalView({ store, selectedId, onSelect, recentreNonce }: Props) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const rendererRef = useRef<TacticalRenderer | null>(null);
  const bodiesRef = useRef(new Map<string, Body>());
  const linksRef = useRef<Link[]>([]);
  const cameraRef = useRef<Camera>({ x: 0, y: 0, zoom: 1 });
  const selectedRef = useRef<string | null>(selectedId);
  const hoverRef = useRef<string | null>(null);
  const dragRef = useRef<{ id: string | null; lastX: number; lastY: number } | null>(null);
  /** Auto-fit yields to the operator the moment they pan or zoom. */
  const manualRef = useRef(false);
  const [hidden, setHidden] = useState(0);
  const [autoFit, setAutoFit] = useState(true);
  const [cursor, setCursor] = useState<"grab" | "grabbing" | "pointer">("grab");

  selectedRef.current = selectedId;

  // ─── mount: renderer, resize observer, animation loop ────────────────────
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const renderer = new TacticalRenderer(canvas);
    rendererRef.current = renderer;

    const parent = canvas.parentElement!;
    const resize = () => {
      const r = parent.getBoundingClientRect();
      renderer.resize(r.width, r.height, Math.min(2, window.devicePixelRatio || 1));
    };
    resize();
    const ro = new ResizeObserver(resize);
    ro.observe(parent);

    let raf = 0;
    let lastPerf = performance.now();

    const frame = (nowPerf: number) => {
      const dtMs = Math.min(64, nowPerf - lastPerf);
      lastPerf = nowPerf;
      const nowMs = Date.now();
      const world = store.world;

      store.tick(nowMs, dtMs);
      store.reapPulses(nowPerf, PULSE_LIFETIME);

      const { visible, hidden: nHidden } = rankForDisplay(world, NODE_BUDGET);
      setHidden((prev) => (prev === nHidden ? prev : nHidden));
      const visibleSet = new Set(visible);

      const bodies = syncBodies(bodiesRef.current, world, nowMs, visibleSet);

      const nodes = visible
        .map((id) => world.agents.get(id))
        .filter((n): n is NonNullable<typeof n> => Boolean(n));
      linksRef.current = contentionLinks(nodes);

      // Physics runs in frame units so a dropped frame slows time rather than
      // destabilising the integrator.
      step(bodies, linksRef.current, DEFAULT_PARAMS, dtMs / 16.67);

      // Frame the whole fleet until the operator takes the camera themselves.
      if (!manualRef.current && bodies.length > 0) {
        easeCamera(cameraRef.current, fitCamera(bodies, renderer.size));
      }

      renderer.draw({
        world,
        bodies,
        links: linksRef.current,
        camera: cameraRef.current,
        selectedId: selectedRef.current,
        hoverId: hoverRef.current,
        nowMs,
        nowPerf,
      });

      raf = requestAnimationFrame(frame);
    };
    raf = requestAnimationFrame(frame);

    return () => {
      cancelAnimationFrame(raf);
      ro.disconnect();
      rendererRef.current = null;
    };
  }, [store]);

  // ─── recentre ────────────────────────────────────────────────────────────
  useEffect(() => {
    if (recentreNonce === 0) return;
    manualRef.current = false;
    setAutoFit(true);
  }, [recentreNonce]);

  /**
   * Pan to the selected agent — but only once the operator has taken manual
   * control. While auto-fit is on the whole fleet is already framed, so moving
   * the camera on select would fight it for no benefit.
   */
  useEffect(() => {
    if (!selectedId || !manualRef.current) return;
    const body = bodiesRef.current.get(selectedId);
    if (!body) return;
    const cam = cameraRef.current;
    const steps = 18;
    let i = 0;
    const id = setInterval(() => {
      i += 1;
      cam.x += (body.x - cam.x) * 0.18;
      cam.y += (body.y - cam.y) * 0.18;
      if (i >= steps) clearInterval(id);
    }, 16);
    return () => clearInterval(id);
  }, [selectedId]);

  // ─── pointer interaction ─────────────────────────────────────────────────

  const local = (e: React.PointerEvent | React.MouseEvent) => {
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    return { x: e.clientX - rect.left, y: e.clientY - rect.top };
  };

  const onPointerDown = useCallback((e: React.PointerEvent<HTMLCanvasElement>) => {
    const renderer = rendererRef.current;
    if (!renderer) return;
    (e.target as HTMLElement).setPointerCapture(e.pointerId);
    const p = local(e);
    const hit = renderer.hitTest(p.x, p.y, [...bodiesRef.current.values()], cameraRef.current);
    dragRef.current = { id: hit, lastX: e.clientX, lastY: e.clientY };
    if (hit) {
      onSelect(hit);
      const body = bodiesRef.current.get(hit);
      if (body) body.pinned = true;
    }
    setCursor("grabbing");
  }, [onSelect]);

  const onPointerMove = useCallback((e: React.PointerEvent<HTMLCanvasElement>) => {
    const renderer = rendererRef.current;
    if (!renderer) return;
    const drag = dragRef.current;
    const cam = cameraRef.current;

    if (drag) {
      const dx = e.clientX - drag.lastX;
      const dy = e.clientY - drag.lastY;
      drag.lastX = e.clientX;
      drag.lastY = e.clientY;

      if (drag.id) {
        // Dragging a node repositions it; physics resumes on release.
        const body = bodiesRef.current.get(drag.id);
        if (body) {
          body.x += dx / cam.zoom;
          body.y += dy / cam.zoom;
        }
      } else {
        cam.x -= dx / cam.zoom;
        cam.y -= dy / cam.zoom;
        if (dx || dy) {
          manualRef.current = true;
          setAutoFit(false);
        }
      }
      return;
    }

    const p = local(e);
    const hit = renderer.hitTest(p.x, p.y, [...bodiesRef.current.values()], cam);
    hoverRef.current = hit;
    setCursor(hit ? "pointer" : "grab");
  }, []);

  const endDrag = useCallback(() => {
    const drag = dragRef.current;
    if (drag?.id) {
      const body = bodiesRef.current.get(drag.id);
      if (body) body.pinned = false;
    }
    dragRef.current = null;
    setCursor("grab");
  }, []);

  // ─── zoom ────────────────────────────────────────────────────────────────

  /** Zoom about a screen point, or about the viewport centre when given none. */
  const zoom = useCallback((factor: number, anchor?: { x: number; y: number }) => {
    const renderer = rendererRef.current;
    if (!renderer) return;
    const view = renderer.size;
    zoomAt(cameraRef.current, factor, anchor ?? { x: view.w / 2, y: view.h / 2 }, view);
    manualRef.current = true;
    setAutoFit(false);
  }, []);

  const resumeAutoFit = useCallback(() => {
    manualRef.current = false;
    setAutoFit(true);
  }, []);

  /**
   * Wheel and pinch, as a native listener rather than React's `onWheel`.
   *
   * React registers `wheel` passively at the root, so a handler passed as a
   * prop cannot call `preventDefault`. Without it the browser acts on the same
   * gesture: a trackpad pinch zooms the whole page, and a two-finger scroll
   * scrolls the document or triggers a back-navigation — all while the canvas
   * zooms underneath. Only a listener attached with `{ passive: false }` can
   * claim the gesture.
   */
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;

    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      // `ctrlKey` on a wheel event means a pinch, on every platform — the
      // browser synthesises it for the gesture, no key is held.
      const pinch = e.ctrlKey || e.metaKey;
      const sensitivity = pinch ? PINCH_SENSITIVITY : WHEEL_SENSITIVITY;
      const rect = canvas.getBoundingClientRect();
      zoom(Math.exp(-e.deltaY * sensitivity), {
        x: e.clientX - rect.left,
        y: e.clientY - rect.top,
      });
    };

    canvas.addEventListener("wheel", onWheel, { passive: false });
    return () => canvas.removeEventListener("wheel", onWheel);
  }, [zoom]);

  /** Keyboard zoom, scoped to the canvas so it never eats a keystroke meant for a prompt. */
  const onKeyDown = useCallback((e: React.KeyboardEvent<HTMLCanvasElement>) => {
    if (e.key === "+" || e.key === "=") zoom(STEP_FACTOR);
    else if (e.key === "-" || e.key === "_") zoom(1 / STEP_FACTOR);
    else if (e.key === "0") resumeAutoFit();
    else return;
    e.preventDefault();
  }, [zoom, resumeAutoFit]);

  const onDoubleClick = useCallback(() => onSelect(null), [onSelect]);

  return (
    <div className="tactical">
      <canvas
        ref={canvasRef}
        tabIndex={0}
        style={{ cursor }}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={endDrag}
        onPointerCancel={endDrag}
        onKeyDown={onKeyDown}
        onDoubleClick={onDoubleClick}
      />
      <div className="tactical-zoom">
        <button onClick={() => zoom(STEP_FACTOR)} title="Zoom in (+)" aria-label="Zoom in">+</button>
        <button onClick={() => zoom(1 / STEP_FACTOR)} title="Zoom out (−)" aria-label="Zoom out">−</button>
        <button
          onClick={resumeAutoFit}
          title="Fit the whole fleet (0)"
          aria-label="Fit the whole fleet"
          disabled={autoFit}
        >
          ⤢
        </button>
      </div>
      {!autoFit && (
        <button className="tactical-autofit" onClick={resumeAutoFit}>
          MANUAL CAMERA — RESUME AUTO-FIT
        </button>
      )}
      {hidden > 0 && (
        <div className="tactical-truncation" title="Ranked by liveness, then recency">
          +{hidden} agent{hidden === 1 ? "" : "s"} not plotted
        </div>
      )}
      <div className="tactical-legend">
        <span><i className="sw sw-claude" /> CLAUDE CODE</span>
        <span><i className="sw sw-open" /> OPENCODE</span>
        <span><i className="sw sw-agy" /> AGY</span>
        <span className="sep" />
        <span><i className="sw sw-contend" /> SHARED CWD</span>
      </div>
    </div>
  );
}
