//! The HUD's public surface — everything a shell needs and nothing more.
//!
//! Two shells consume this package: `apps/web` in a browser and `apps/desktop`
//! inside a Tauri window. They differ in exactly one thing — how they reach
//! `jod-api` — so that is the only seam exposed here. Everything else (the
//! graph, the canvas, the panels, the world reducer) is shared verbatim, which
//! is the point of the package: one HUD, not two that drift.

export { default as Hud } from "./App";
export type { HudProps } from "./App";

export type {
  Transport,
  TransportHandlers,
  TransportFactory,
  LinkState,
  Scope,
} from "./transport";
export { EMPTY_REPORT } from "./transport";
export { HttpTransport, probeApi } from "./transport/http";
export { SimTransport } from "./transport/sim";
export { createTransport, modeFromLocation } from "./transport/factory";
export type { TransportMode } from "./transport/factory";

export { WorldStore } from "./state/world";
export type { AgentNode } from "./state/world";

export type {
  AgentEnvelope,
  AgentEvent,
  AgentSummary,
  AgentStatus,
  HarnessInfo,
  HarnessKind,
  PermissionPolicy,
  Report,
  Resume,
  SpawnRequest,
  StoredRun,
  Usage,
} from "./types";
