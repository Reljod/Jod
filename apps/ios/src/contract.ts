/**
 * The API contract, re-exported — not re-declared.
 *
 * `apps/web/src/types.ts` is already the checked mirror of the `core/` crate's
 * serde representation, verified against `core/src/{event,service,store}.rs`
 * directly. A second hand-maintained copy here would be a shadow copy of a
 * shadow copy, and the charter's rule is one system of record: the first time
 * the two drifted, one of the two clients would be silently wrong about the
 * wire format and nothing would fail until runtime.
 *
 * So this file is a pointer, and the only thing it may ever contain is
 * re-exports. Types that are genuinely this app's own — transcript entries,
 * session state — live in `session.ts`, because the orchestrator has no opinion
 * about them.
 *
 * Note for whoever moves these directories: this relative path and the
 * `include` in `tsconfig.json` are the two places that know where the shared
 * contract lives.
 */

export type {
  AgentEnvelope,
  AgentEvent,
  AgentEventKind,
  AgentStatus,
  AgentSummary,
  HarnessInfo,
  HarnessKind,
  Member,
  MemberStatus,
  PermissionPolicy,
  Report,
  Resume,
  SpawnRequest,
  StoredRun,
  TeamTask,
  TeamView,
  Usage,
  Fact,
} from "@jod/hud/types";

export {
  HARNESS_KINDS,
  harnessCode,
  isLive,
  resumeLabel,
  taskIsClaimed,
  taskIsDone,
  totalTokens,
} from "@jod/hud/types";
