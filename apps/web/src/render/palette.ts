import type { AgentStatus, HarnessKind } from "../types";
import type { Phase } from "../state/world";

/**
 * Colour carries state here, never decoration.
 *
 *   hue        = which harness is running the agent
 *   brightness = how hot it is right now
 *   red/amber  = something a human should look at
 *
 * Kept as raw RGB triples so the renderer can vary alpha per stroke without
 * re-parsing colour strings sixty times a second.
 */
export type RGB = readonly [number, number, number];

export const BG = "#04070d";
export const BG_DEEP = "#010307";

export const CYAN: RGB = [53, 224, 255];
export const MINT: RGB = [77, 255, 195];
export const VIOLET: RGB = [180, 124, 255];
export const AMBER: RGB = [255, 181, 71];
export const RED: RGB = [255, 77, 94];
export const STEEL: RGB = [122, 148, 173];
export const WHITE: RGB = [226, 244, 255];

export const HARNESS_COLOR: Record<HarnessKind, RGB> = {
  claude_code: CYAN,
  open_code: MINT,
  agy: VIOLET,
};

export function rgba(c: RGB, a = 1): string {
  return `rgba(${c[0]}, ${c[1]}, ${c[2]}, ${a})`;
}

/** Mix two colours; `t` = 0 gives `a`, 1 gives `b`. */
export function mix(a: RGB, b: RGB, t: number): RGB {
  const k = t < 0 ? 0 : t > 1 ? 1 : t;
  return [
    Math.round(a[0] + (b[0] - a[0]) * k),
    Math.round(a[1] + (b[1] - a[1]) * k),
    Math.round(a[2] + (b[2] - a[2]) * k),
  ];
}

/** The colour an agent node is drawn in. */
export function agentColor(
  harness: HarnessKind,
  status: AgentStatus,
  phase: Phase,
): RGB {
  if (status === "failed") return RED;
  if (status === "killed") return STEEL;
  if (status === "completed") return mix(HARNESS_COLOR[harness], STEEL, 0.55);
  if (phase === "idle") return mix(HARNESS_COLOR[harness], STEEL, 0.45);
  return HARNESS_COLOR[harness];
}

export const PHASE_LABEL: Record<Phase, string> = {
  booting: "BOOT",
  thinking: "REASONING",
  acting: "TOOL",
  speaking: "REPORTING",
  idle: "IDLE",
  done: "COMPLETE",
  failed: "FAULT",
};

export const STATUS_LABEL: Record<AgentStatus, string> = {
  running: "ACTIVE",
  completed: "COMPLETE",
  failed: "FAULT",
  killed: "TERMINATED",
};

export const MONO =
  "'JetBrains Mono', 'SFMono-Regular', 'SF Mono', Menlo, Consolas, monospace";
