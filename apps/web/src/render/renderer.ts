import type { Body, Link } from "../graph/physics";
import { isHotContention } from "../graph/model";
import type { AgentNode, World } from "../state/world";
import { eventRate, truncate } from "../state/world";
import { harnessCode, totalTokens } from "../types";
import { hashString, makeRng } from "../util/rng";
import {
  AMBER,
  BG,
  MONO,
  PHASE_LABEL,
  RED,
  RGB,
  STEEL,
  WHITE,
  agentColor,
  mix,
  rgba,
} from "./palette";

export interface Camera {
  x: number;
  y: number;
  zoom: number;
}

export interface RenderInput {
  world: World;
  bodies: Body[];
  links: Link[];
  camera: Camera;
  selectedId: string | null;
  hoverId: string | null;
  nowMs: number;
  nowPerf: number;
}

const PULSE_LIFETIME = 1000;
const NODE_BASE_RADIUS = 26;
const STAR_COUNT = 260;

interface Star {
  x: number;
  y: number;
  r: number;
  a: number;
  depth: number;
}

/**
 * Draws the tactical view.
 *
 * Everything here is hand-rolled 2D canvas rather than a charting or graph
 * library, for one reason: every mark needs to be bound to a real field of
 * `AgentSummary` or the event stream. A general-purpose library would draw
 * pretty circles that mean nothing. Here, ring speed is event rate, arc length
 * is token burn, colour is harness, radius from centre is engagement, and a red
 * arc between two nodes means they are writing to the same directory.
 */
export class TacticalRenderer {
  private ctx: CanvasRenderingContext2D;
  private stars: Star[];
  private w = 0;
  private h = 0;
  private dpr = 1;

  constructor(private canvas: HTMLCanvasElement) {
    const ctx = canvas.getContext("2d", { alpha: false });
    if (!ctx) throw new Error("2D canvas unavailable");
    this.ctx = ctx;

    const rng = makeRng(0xf1e1d);
    this.stars = Array.from({ length: STAR_COUNT }, () => ({
      x: (rng() - 0.5) * 3200,
      y: (rng() - 0.5) * 3200,
      r: 0.3 + rng() * 1.1,
      a: 0.08 + rng() * 0.4,
      depth: 0.25 + rng() * 0.6,
    }));
  }

  /** Current CSS-pixel viewport, for the auto-fit camera. */
  get size(): { w: number; h: number } {
    return { w: this.w, h: this.h };
  }

  resize(w: number, h: number, dpr: number): void {
    this.w = w;
    this.h = h;
    this.dpr = dpr;
    this.canvas.width = Math.floor(w * dpr);
    this.canvas.height = Math.floor(h * dpr);
    this.canvas.style.width = `${w}px`;
    this.canvas.style.height = `${h}px`;
  }

  /** Screen point → world point, for hit testing and dragging. */
  toWorld(sx: number, sy: number, cam: Camera): { x: number; y: number } {
    return {
      x: (sx - this.w / 2) / cam.zoom + cam.x,
      y: (sy - this.h / 2) / cam.zoom + cam.y,
    };
  }

  hitTest(sx: number, sy: number, bodies: Body[], cam: Camera): string | null {
    const p = this.toWorld(sx, sy, cam);
    let best: string | null = null;
    let bestD = Infinity;
    for (const b of bodies) {
      const d = Math.hypot(b.x - p.x, b.y - p.y);
      const r = NODE_BASE_RADIUS * (0.9 + b.mass * 0.22) + 12;
      if (d < r && d < bestD) {
        bestD = d;
        best = b.id;
      }
    }
    return best;
  }

  draw(input: RenderInput): void {
    const { ctx } = this;
    const { camera, nowPerf } = input;

    ctx.save();
    ctx.scale(this.dpr, this.dpr);
    ctx.fillStyle = BG;
    ctx.fillRect(0, 0, this.w, this.h);

    this.drawAmbient(nowPerf, camera);

    ctx.save();
    ctx.translate(this.w / 2, this.h / 2);
    ctx.scale(camera.zoom, camera.zoom);
    ctx.translate(-camera.x, -camera.y);

    this.drawStars(camera);
    this.drawGrid(nowPerf);
    this.drawSweep(nowPerf);
    this.drawContention(input);
    this.drawTethers(input);
    this.drawPulses(input);
    this.drawCore(input);
    this.drawAgents(input);

    ctx.restore();

    this.drawOffscreenMarkers(input);
    this.drawVignette();
    this.drawScanlines(nowPerf);
    ctx.restore();
  }

  // ─── background ──────────────────────────────────────────────────────────

  private drawAmbient(now: number, cam: Camera): void {
    const { ctx } = this;
    const pulse = 0.5 + 0.5 * Math.sin(now / 2600);
    const g = ctx.createRadialGradient(
      this.w / 2,
      this.h / 2,
      0,
      this.w / 2,
      this.h / 2,
      Math.max(this.w, this.h) * 0.75,
    );
    g.addColorStop(0, `rgba(12, 40, 60, ${0.5 + pulse * 0.08})`);
    g.addColorStop(0.45, "rgba(6, 18, 30, 0.35)");
    g.addColorStop(1, "rgba(1, 3, 7, 0)");
    ctx.fillStyle = g;
    ctx.fillRect(0, 0, this.w, this.h);
    void cam;
  }

  private drawStars(cam: Camera): void {
    const { ctx } = this;
    ctx.save();
    for (const s of this.stars) {
      // Parallax: distant stars shift less than the graph as the camera moves.
      const px = s.x + cam.x * (1 - s.depth);
      const py = s.y + cam.y * (1 - s.depth);
      ctx.fillStyle = `rgba(180, 220, 255, ${s.a})`;
      ctx.fillRect(px, py, s.r, s.r);
    }
    ctx.restore();
  }

  private drawGrid(now: number): void {
    const { ctx } = this;
    ctx.save();
    ctx.lineWidth = 1;

    // Range rings, labelled like a radar scope.
    for (let r = 120; r <= 720; r += 120) {
      const breathe = 1 + Math.sin(now / 3400 + r / 220) * 0.004;
      ctx.beginPath();
      ctx.arc(0, 0, r * breathe, 0, Math.PI * 2);
      ctx.strokeStyle = `rgba(53, 224, 255, ${r === 360 ? 0.11 : 0.055})`;
      ctx.stroke();
    }

    // Radial spokes every 30°, with heavier cardinals.
    for (let i = 0; i < 12; i++) {
      const a = (i * Math.PI) / 6;
      ctx.beginPath();
      ctx.moveTo(Math.cos(a) * 90, Math.sin(a) * 90);
      ctx.lineTo(Math.cos(a) * 760, Math.sin(a) * 760);
      ctx.strokeStyle = `rgba(53, 224, 255, ${i % 3 === 0 ? 0.07 : 0.03})`;
      ctx.stroke();
    }

    // Bearing ticks on the outer ring.
    ctx.font = `9px ${MONO}`;
    ctx.fillStyle = "rgba(53, 224, 255, 0.28)";
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    for (let deg = 0; deg < 360; deg += 45) {
      const a = (deg * Math.PI) / 180 - Math.PI / 2;
      ctx.fillText(
        String(deg).padStart(3, "0"),
        Math.cos(a) * 745,
        Math.sin(a) * 745,
      );
    }
    ctx.restore();
  }

  /** The slow radar sweep — pure atmosphere, and the one thing that is. */
  private drawSweep(now: number): void {
    const { ctx } = this;
    const angle = (now / 7000) % (Math.PI * 2);
    ctx.save();
    ctx.rotate(angle);
    const g = ctx.createLinearGradient(0, 0, 760, 0);
    g.addColorStop(0, "rgba(53, 224, 255, 0.14)");
    g.addColorStop(1, "rgba(53, 224, 255, 0)");
    ctx.strokeStyle = g;
    ctx.lineWidth = 2;
    ctx.beginPath();
    ctx.moveTo(0, 0);
    ctx.lineTo(760, 0);
    ctx.stroke();

    ctx.fillStyle = (() => {
      const wedge = ctx.createRadialGradient(0, 0, 0, 0, 0, 760);
      wedge.addColorStop(0, "rgba(53, 224, 255, 0.05)");
      wedge.addColorStop(1, "rgba(53, 224, 255, 0)");
      return wedge;
    })();
    ctx.beginPath();
    ctx.moveTo(0, 0);
    ctx.arc(0, 0, 760, -0.34, 0);
    ctx.closePath();
    ctx.fill();
    ctx.restore();
  }

  // ─── edges ───────────────────────────────────────────────────────────────

  /**
   * Contention arcs: two agents writing into the same working directory.
   * Drawn as a bowed, dashed, travelling arc so it reads as tension rather
   * than as ordinary structure.
   */
  private drawContention(input: RenderInput): void {
    const { ctx } = this;
    const byId = new Map(input.bodies.map((b) => [b.id, b]));

    for (const link of input.links) {
      const a = byId.get(link.a);
      const b = byId.get(link.b);
      if (!a || !b) continue;

      const hot = isHotContention(link);
      const color = hot ? RED : AMBER;
      const mx = (a.x + b.x) / 2;
      const my = (a.y + b.y) / 2;
      // Bow the arc away from the core so overlapping pairs stay separable.
      const nlen = Math.hypot(mx, my) || 1;
      const bow = 42 * link.weight;
      const cx = mx + (mx / nlen) * bow;
      const cy = my + (my / nlen) * bow;

      const throb = hot ? 0.42 + 0.28 * Math.sin(input.nowPerf / 320) : 0.24;

      ctx.save();
      ctx.setLineDash([7, 9]);
      ctx.lineDashOffset = -(input.nowPerf / 34) % 16;
      ctx.strokeStyle = rgba(color, throb);
      ctx.lineWidth = hot ? 1.8 : 1.1;
      ctx.beginPath();
      ctx.moveTo(a.x, a.y);
      ctx.quadraticCurveTo(cx, cy, b.x, b.y);
      ctx.stroke();
      ctx.restore();

      if (hot) {
        // Label the shared path at the apex — the actionable part.
        const node = input.world.agents.get(link.a);
        const label = node ? shortPath(node.summary.cwd) : "shared path";
        const apx = 0.25 * a.x + 0.5 * cx + 0.25 * b.x;
        const apy = 0.25 * a.y + 0.5 * cy + 0.25 * b.y;
        ctx.save();
        ctx.font = `9px ${MONO}`;
        ctx.textAlign = "center";
        ctx.fillStyle = rgba(RED, 0.85);
        ctx.fillText(`⚠ ${label}`, apx, apy - 6);
        ctx.restore();
      }
    }
  }

  /** Command tethers from the core out to each agent. */
  private drawTethers(input: RenderInput): void {
    const { ctx } = this;
    for (const body of input.bodies) {
      const node = input.world.agents.get(body.id);
      if (!node) continue;
      const color = agentColor(node.summary.harness, node.summary.status, node.phase);
      const live = node.summary.status === "running";
      const d = Math.hypot(body.x, body.y) || 1;
      const ux = body.x / d;
      const uy = body.y / d;
      const from = 54;
      const to = d - NODE_BASE_RADIUS * (0.9 + body.mass * 0.22) - 6;
      if (to <= from) continue;

      ctx.save();
      ctx.strokeStyle = rgba(color, live ? 0.2 + node.heat * 0.25 : 0.08);
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(ux * from, uy * from);
      ctx.lineTo(ux * to, uy * to);
      ctx.stroke();
      ctx.restore();
    }
  }

  /** Packets travelling the tethers — one per real event. */
  private drawPulses(input: RenderInput): void {
    const { ctx } = this;
    const byId = new Map(input.bodies.map((b) => [b.id, b]));

    for (const pulse of input.world.pulses) {
      const age = input.nowPerf - pulse.born;
      if (age < 0 || age > PULSE_LIFETIME) continue;
      const body = byId.get(pulse.agentId);
      if (!body) continue;

      const t = age / PULSE_LIFETIME;
      const d = Math.hypot(body.x, body.y) || 1;
      const ux = body.x / d;
      const uy = body.y / d;
      const from = 54;
      const to = d - 30;
      if (to <= from) continue;

      // Outbound packets run core→agent; results run back the other way.
      const travel = pulse.kind === "out" ? t : 1 - t;
      const eased = travel * travel * (3 - 2 * travel);
      const r = from + (to - from) * eased;
      const px = ux * r;
      const py = uy * r;

      const color: RGB =
        pulse.kind === "error"
          ? RED
          : pulse.kind === "speak"
            ? WHITE
            : agentColor(
                input.world.agents.get(pulse.agentId)?.summary.harness ?? "claude_code",
                "running",
                "acting",
              );

      const fade = 1 - t;
      ctx.save();
      ctx.shadowBlur = 12;
      ctx.shadowColor = rgba(color, 0.8 * fade);
      ctx.fillStyle = rgba(color, 0.9 * fade);
      ctx.beginPath();
      ctx.arc(px, py, pulse.kind === "error" ? 3.4 : 2.3, 0, Math.PI * 2);
      ctx.fill();

      // A short comet tail behind the packet.
      const tailR = r - (pulse.kind === "out" ? 16 : -16);
      ctx.strokeStyle = rgba(color, 0.28 * fade);
      ctx.lineWidth = 1.4;
      ctx.beginPath();
      ctx.moveTo(px, py);
      ctx.lineTo(ux * tailR, uy * tailR);
      ctx.stroke();
      ctx.restore();
    }
  }

  // ─── nodes ───────────────────────────────────────────────────────────────

  /** The orchestrator itself. Everything is delegated from here. */
  private drawCore(input: RenderInput): void {
    const { ctx } = this;
    const { world, nowPerf } = input;
    const running = world.report.running;
    const anyFault = world.report.failed > 0;
    const beat = 0.5 + 0.5 * Math.sin(nowPerf / (running > 0 ? 620 : 1600));

    ctx.save();

    const glow = ctx.createRadialGradient(0, 0, 0, 0, 0, 92);
    glow.addColorStop(0, `rgba(53, 224, 255, ${0.2 + beat * 0.12})`);
    glow.addColorStop(1, "rgba(53, 224, 255, 0)");
    ctx.fillStyle = glow;
    ctx.beginPath();
    ctx.arc(0, 0, 92, 0, Math.PI * 2);
    ctx.fill();

    // Counter-rotating rings — the classic HUD tell that a system is live.
    this.reticleRing(0, 0, 46, nowPerf / 4200, 24, rgba([53, 224, 255], 0.5), 7);
    this.reticleRing(0, 0, 38, -nowPerf / 2900, 12, rgba([53, 224, 255], 0.3), 5);

    ctx.strokeStyle = rgba([53, 224, 255], 0.75);
    ctx.lineWidth = 1.6;
    ctx.beginPath();
    ctx.arc(0, 0, 30, 0, Math.PI * 2);
    ctx.stroke();

    // Inner hexagon.
    ctx.beginPath();
    for (let i = 0; i < 6; i++) {
      const a = (i * Math.PI) / 3 + nowPerf / 9000;
      const x = Math.cos(a) * 19;
      const y = Math.sin(a) * 19;
      i === 0 ? ctx.moveTo(x, y) : ctx.lineTo(x, y);
    }
    ctx.closePath();
    ctx.fillStyle = `rgba(53, 224, 255, ${0.1 + beat * 0.08})`;
    ctx.strokeStyle = rgba(anyFault ? RED : [53, 224, 255], 0.9);
    ctx.lineWidth = 1.2;
    ctx.fill();
    ctx.stroke();

    ctx.font = `bold 13px ${MONO}`;
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.fillStyle = rgba(WHITE, 0.95);
    ctx.fillText("JOD", 0, 0);

    ctx.font = `9px ${MONO}`;
    ctx.fillStyle = rgba([53, 224, 255], 0.6);
    ctx.fillText(`${running} ACTIVE`, 0, 60);
    ctx.restore();
  }

  private drawAgents(input: RenderInput): void {
    for (const body of input.bodies) {
      const node = input.world.agents.get(body.id);
      if (node) this.drawAgent(body, node, input);
    }
  }

  private drawAgent(body: Body, node: AgentNode, input: RenderInput): void {
    const { ctx } = this;
    const { nowPerf, nowMs } = input;
    const selected = input.selectedId === body.id;
    const hovered = input.hoverId === body.id;
    const live = node.summary.status === "running";
    const color = agentColor(node.summary.harness, node.summary.status, node.phase);
    const radius = NODE_BASE_RADIUS * (0.9 + body.mass * 0.22);
    const jitter = hashString(body.id) % 1000;

    ctx.save();
    ctx.translate(body.x, body.y);

    // Heat glow.
    if (node.heat > 0.02) {
      const g = ctx.createRadialGradient(0, 0, radius * 0.4, 0, 0, radius * 3.1);
      g.addColorStop(0, rgba(color, 0.2 * node.heat));
      g.addColorStop(1, rgba(color, 0));
      ctx.fillStyle = g;
      ctx.beginPath();
      ctx.arc(0, 0, radius * 3.1, 0, Math.PI * 2);
      ctx.fill();
    }

    // Outer tick ring — spins at the agent's event rate. A stalled agent's
    // ring visibly stops, which is the fastest way to spot a hung run.
    const rate = eventRate(node, nowMs);
    const spin = (nowPerf / 5200) * (0.25 + rate * 1.6) + jitter;
    this.reticleRing(0, 0, radius + 11, spin, 20, rgba(color, live ? 0.42 : 0.16), 5);

    // Token-burn gauge: arc length is this agent's share of the fleet's spend.
    const tokens = totalTokens(node.summary.usage);
    const share = clampNorm(Math.log10(1 + tokens) / 6);
    ctx.beginPath();
    ctx.arc(0, 0, radius + 5, -Math.PI / 2, -Math.PI / 2 + share * Math.PI * 2);
    ctx.strokeStyle = rgba(mix(color, AMBER, 0.4), 0.75);
    ctx.lineWidth = 2.4;
    ctx.lineCap = "round";
    ctx.stroke();

    // Body.
    ctx.beginPath();
    ctx.arc(0, 0, radius, 0, Math.PI * 2);
    ctx.fillStyle = `rgba(4, 10, 18, 0.92)`;
    ctx.fill();
    ctx.strokeStyle = rgba(color, live ? 0.95 : 0.4);
    ctx.lineWidth = selected ? 2.2 : 1.4;
    ctx.stroke();

    // Phase core: a filled disc that pulses while acting.
    const act = node.phase === "acting" ? 0.5 + 0.5 * Math.sin(nowPerf / 190) : 1;
    ctx.beginPath();
    ctx.arc(0, 0, radius * 0.42 * (0.8 + act * 0.2), 0, Math.PI * 2);
    ctx.fillStyle = rgba(color, live ? 0.28 + node.heat * 0.5 : 0.12);
    ctx.fill();

    // Harness code in the middle.
    ctx.font = `bold 9px ${MONO}`;
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.fillStyle = rgba(WHITE, live ? 0.92 : 0.5);
    ctx.fillText(harnessCode(node.summary.harness), 0, 0);

    // Fault chevron.
    if (node.summary.status === "failed" || node.errorCount > 0) {
      ctx.font = `10px ${MONO}`;
      ctx.fillStyle = rgba(RED, 0.9);
      ctx.fillText("!", radius * 0.72, -radius * 0.72);
    }

    this.drawToolSatellites(node, radius, nowPerf, color);

    if (selected || hovered) this.drawBracket(radius + 20, rgba(color, selected ? 0.95 : 0.4));

    // Labels below the node.
    ctx.font = `10px ${MONO}`;
    ctx.textAlign = "center";
    ctx.fillStyle = rgba(WHITE, live ? 0.9 : 0.45);
    ctx.fillText(node.summary.name, 0, radius + 26);

    ctx.font = `8.5px ${MONO}`;
    ctx.fillStyle = rgba(color, 0.75);
    const activity = node.inFlight
      ? `▸ ${node.inFlight.name}`
      : PHASE_LABEL[node.phase];
    ctx.fillText(activity, 0, radius + 38);

    ctx.restore();
  }

  /**
   * Recent tool calls, orbiting their agent. The in-flight one is bright and
   * pulls to the front; completed ones fade and drift back.
   */
  private drawToolSatellites(
    node: AgentNode,
    radius: number,
    now: number,
    color: RGB,
  ): void {
    const { ctx } = this;
    const tools = node.tools.slice(-6);
    if (tools.length === 0) return;
    const orbit = radius + 30;
    const base = hashString(node.summary.id) % 628;

    tools.forEach((tool, i) => {
      const inFlight = tool.endedAt === null;
      const a = base / 100 + (i / tools.length) * Math.PI * 2 + now / (inFlight ? 1400 : 5200);
      const x = Math.cos(a) * orbit;
      const y = Math.sin(a) * orbit;
      const age = tool.endedAt ? Math.min(1, (Date.now() - tool.endedAt) / 12000) : 0;
      const alpha = inFlight ? 0.95 : 0.5 * (1 - age) + 0.08;
      const c = tool.isError ? RED : inFlight ? color : mix(color, STEEL, 0.5);
      const r = inFlight ? 4.2 + Math.sin(now / 160) * 0.9 : 2.4;

      ctx.beginPath();
      ctx.arc(x, y, r, 0, Math.PI * 2);
      ctx.fillStyle = rgba(c, alpha);
      ctx.fill();

      if (inFlight) {
        ctx.beginPath();
        ctx.arc(x, y, r + 4 + Math.sin(now / 200) * 1.6, 0, Math.PI * 2);
        ctx.strokeStyle = rgba(c, 0.4);
        ctx.lineWidth = 1;
        ctx.stroke();
      }
    });
  }

  /** Four corner brackets — the selection idiom of every targeting HUD. */
  private drawBracket(r: number, stroke: string): void {
    const { ctx } = this;
    const arm = r * 0.42;
    ctx.save();
    ctx.strokeStyle = stroke;
    ctx.lineWidth = 1.4;
    for (const [sx, sy] of [
      [-1, -1],
      [1, -1],
      [-1, 1],
      [1, 1],
    ] as const) {
      ctx.beginPath();
      ctx.moveTo(sx * r, sy * r - sy * arm);
      ctx.lineTo(sx * r, sy * r);
      ctx.lineTo(sx * r - sx * arm, sy * r);
      ctx.stroke();
    }
    ctx.restore();
  }

  private reticleRing(
    cx: number,
    cy: number,
    r: number,
    rotation: number,
    ticks: number,
    stroke: string,
    len: number,
  ): void {
    const { ctx } = this;
    ctx.save();
    ctx.translate(cx, cy);
    ctx.rotate(rotation);
    ctx.strokeStyle = stroke;
    ctx.lineWidth = 1.2;
    for (let i = 0; i < ticks; i++) {
      const a = (i / ticks) * Math.PI * 2;
      const long = i % 4 === 0;
      const l = long ? len * 1.7 : len;
      ctx.beginPath();
      ctx.moveTo(Math.cos(a) * r, Math.sin(a) * r);
      ctx.lineTo(Math.cos(a) * (r + l), Math.sin(a) * (r + l));
      ctx.stroke();
    }
    ctx.restore();
  }

  /** Arrow markers for agents that have drifted outside the viewport. */
  private drawOffscreenMarkers(input: RenderInput): void {
    const { ctx } = this;
    const { camera } = input;
    const margin = 34;

    for (const body of input.bodies) {
      const sx = (body.x - camera.x) * camera.zoom + this.w / 2;
      const sy = (body.y - camera.y) * camera.zoom + this.h / 2;
      if (sx > -20 && sx < this.w + 20 && sy > -20 && sy < this.h + 20) continue;

      const node = input.world.agents.get(body.id);
      if (!node) continue;
      const color = agentColor(node.summary.harness, node.summary.status, node.phase);

      const cx = this.w / 2;
      const cy = this.h / 2;
      const angle = Math.atan2(sy - cy, sx - cx);
      const px = cx + Math.cos(angle) * (this.w / 2 - margin);
      const py = cy + Math.sin(angle) * (this.h / 2 - margin);

      ctx.save();
      ctx.translate(px, py);
      ctx.rotate(angle);
      ctx.fillStyle = rgba(color, 0.8);
      ctx.beginPath();
      ctx.moveTo(8, 0);
      ctx.lineTo(-5, 4.5);
      ctx.lineTo(-5, -4.5);
      ctx.closePath();
      ctx.fill();
      ctx.restore();

      ctx.save();
      ctx.font = `8px ${MONO}`;
      ctx.fillStyle = rgba(color, 0.7);
      ctx.textAlign = "center";
      ctx.fillText(truncate(node.summary.name, 12), px, py - 12);
      ctx.restore();
    }
  }

  // ─── overlays ────────────────────────────────────────────────────────────

  private drawVignette(): void {
    const { ctx } = this;
    const g = ctx.createRadialGradient(
      this.w / 2,
      this.h / 2,
      Math.min(this.w, this.h) * 0.32,
      this.w / 2,
      this.h / 2,
      Math.max(this.w, this.h) * 0.78,
    );
    g.addColorStop(0, "rgba(0,0,0,0)");
    g.addColorStop(1, "rgba(0,0,0,0.72)");
    ctx.fillStyle = g;
    ctx.fillRect(0, 0, this.w, this.h);
  }

  /** CRT scanlines plus a slow rolling band. Subtle enough to read through. */
  private drawScanlines(now: number): void {
    const { ctx } = this;
    ctx.save();
    ctx.globalAlpha = 0.045;
    ctx.fillStyle = "#8fd8ff";
    for (let y = 0; y < this.h; y += 3) ctx.fillRect(0, y, this.w, 1);
    ctx.globalAlpha = 1;

    const bandY = ((now / 26) % (this.h + 260)) - 130;
    const band = ctx.createLinearGradient(0, bandY - 130, 0, bandY + 130);
    band.addColorStop(0, "rgba(120, 220, 255, 0)");
    band.addColorStop(0.5, "rgba(120, 220, 255, 0.028)");
    band.addColorStop(1, "rgba(120, 220, 255, 0)");
    ctx.fillStyle = band;
    ctx.fillRect(0, bandY - 130, this.w, 260);
    ctx.restore();
  }
}

function clampNorm(v: number): number {
  return v < 0 ? 0 : v > 1 ? 1 : v;
}

/** Last two path segments — enough to identify a checkout without the noise. */
export function shortPath(p: string): string {
  const parts = p.split("/").filter(Boolean);
  return parts.length <= 2 ? p : `…/${parts.slice(-2).join("/")}`;
}
