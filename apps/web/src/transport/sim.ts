import type {
  AgentEnvelope,
  AgentEvent,
  AgentStatus,
  AgentSummary,
  HarnessInfo,
  HarnessKind,
  Report,
  SpawnRequest,
  StoredRun,
  Usage,
} from "../types";
import type { Transport, TransportHandlers } from "./index";
import { makeRng } from "../util/rng";

/**
 * Drives the HUD with a synthetic but structurally honest fleet.
 *
 * This is not decoration. Until the API layer lands, this is the only way to
 * exercise the parts of the display that only appear under load — cwd
 * contention between agents, a run that fails mid-tool, cost accumulating
 * unevenly across harnesses. It emits exactly the `AgentEnvelope` stream the
 * real orchestrator emits, so nothing above it can tell the difference.
 *
 * Seeded on purpose: the same seed replays the same fleet, which makes a visual
 * regression something you can actually look at twice.
 */

interface Beat {
  after: number; // ms since the previous beat
  event: AgentEvent;
}

interface Blueprint {
  name: string;
  harness: HarnessKind;
  cwd: string;
  model: string;
  prompt: string;
  beats: Beat[];
  /** How the run ends. `null` keeps it running forever. */
  ending: AgentStatus | null;
}

const think = (text: string, after = 900): Beat => ({
  after,
  event: { kind: "thinking", text },
});
const say = (text: string, after = 1200): Beat => ({
  after,
  event: { kind: "message", text },
});
const call = (name: string, input: unknown, after = 700): Beat => ({
  after,
  event: { kind: "tool_call", name, input },
});
const result = (name: string, summary: string, after = 900, is_error = false): Beat => ({
  after,
  event: { kind: "tool_result", name, summary, is_error },
});

const usage = (i: number, o: number, cr: number, cost: number): Usage => ({
  input_tokens: i,
  output_tokens: o,
  cache_read_tokens: cr,
  cost_usd: cost,
});

// Two of these deliberately share ~/repo/Jod — that collision is what the
// contention edges in the tactical view are for.
const BLUEPRINTS: Blueprint[] = [
  {
    name: "vps-provision",
    harness: "claude_code",
    cwd: "/home/reljod/repo/Jod",
    model: "claude-opus-5",
    prompt: "Provision the jod-cloud box and harden sshd.",
    ending: "running",
    beats: [
      think("Checking which provider rows are still purchasable."),
      call("Bash", { command: "ssh jod-cloud 'uname -a'" }),
      result("Bash", "Linux jod-cloud 6.8.0-51-generic x86_64"),
      say("Box is reachable. Auditing the sshd config before I touch it."),
      call("Read", { file_path: "/etc/ssh/sshd_config" }),
      result("Read", "122 lines — PasswordAuthentication yes"),
      think("Password auth is on. That is the finding."),
      call("Edit", { file_path: "/etc/ssh/sshd_config" }),
      result("Edit", "PasswordAuthentication no"),
      call("Bash", { command: "sshd -t && systemctl reload sshd" }, 1400),
      result("Bash", "config ok; reloaded"),
      say("sshd hardened. Moving to the firewall rules."),
      call("Bash", { command: "ufw status numbered" }),
      result("Bash", "Status: inactive"),
      think("Firewall is off entirely. Enabling with 22/443 only."),
    ],
  },
  {
    name: "memory-store",
    harness: "open_code",
    cwd: "/home/reljod/repo/Jod",
    model: "gpt-5-codex",
    prompt: "Add bitemporal fact storage to core/src/store.rs.",
    ending: "completed",
    beats: [
      think("Reading the existing schema before adding a table."),
      call("Read", { file_path: "core/src/store.rs" }),
      result("Read", "774 lines"),
      call("Grep", { pattern: "CREATE TABLE" }),
      result("Grep", "4 matches"),
      say("Adding a facts table with valid_from/valid_to and an FTS index."),
      call("Edit", { file_path: "core/src/store.rs" }, 1500),
      result("Edit", "+68 −2"),
      call("Bash", { command: "cargo test -p jod-core store::" }, 2600),
      result("Bash", "test result: FAILED. 1 failed", 1200, true),
      think("supersede() left the old row's valid_to null. Fixing the bound."),
      call("Edit", { file_path: "core/src/store.rs" }),
      result("Edit", "+4 −1"),
      call("Bash", { command: "cargo test -p jod-core store::" }, 2800),
      result("Bash", "test result: ok. 19 passed"),
      {
        after: 900,
        event: {
          kind: "finished",
          text: "Facts table added; 19 store tests green.",
          exit_code: 0,
          is_error: false,
          usage: usage(48200, 6100, 121000, 0.41),
        },
      },
    ],
  },
  {
    name: "harness-agy",
    harness: "agy",
    cwd: "/home/reljod/repo/agy-adapter",
    model: "agy-default",
    prompt: "Verify the AGY adapter end to end.",
    ending: "running",
    beats: [
      think("Locating the agy binary and confirming the flag surface."),
      call("Bash", { command: "which agy && agy --help" }),
      result("Bash", "/usr/local/bin/agy — --continue, --conversation <id>"),
      say("Flags match the Resume enum. Running a live one-shot."),
      call("Bash", { command: "agy -p 'reply OK' --json" }, 2200),
      result("Bash", '{"type":"assistant","text":"OK"}'),
      think("JSONL shape differs from Claude's. Mapping it onto AgentEvent."),
      call("Write", { file_path: "core/src/harness/agy.rs" }, 1600),
      result("Write", "398 lines written"),
      call("Bash", { command: "cargo test -p jod-core harness::agy" }, 2400),
      result("Bash", "test result: ok. 11 passed"),
      say("AGY normalises cleanly. Wiring it into HarnessKind::ALL."),
      call("Edit", { file_path: "core/src/harness/mod.rs" }),
      result("Edit", "+9 −3"),
      think("Third harness is live. Checking nothing switched exhaustively on two."),
      call("Grep", { pattern: "HarnessKind::" }),
      result("Grep", "23 matches across 6 files"),
    ],
  },
  {
    name: "pr-shepherd",
    harness: "claude_code",
    cwd: "/home/reljod/repo/Jod",
    model: "claude-sonnet-5",
    prompt: "Sweep open PRs and merge what the gate clears.",
    ending: "failed",
    beats: [
      call("Bash", { command: "gh pr list --state open --json number,title" }),
      result("Bash", "6 open PRs"),
      think("Categorising each against the auto-merge regex."),
      call("Bash", { command: "./bin/merge_pr.sh 28 --dry-run" }, 1500),
      result("Bash", "category=docs → auto-mergeable"),
      say("PR #28 is prose-only. Merging it unread per the charter."),
      call("Bash", { command: "./bin/merge_pr.sh 28 --ready" }, 2000),
      result("Bash", "merged"),
      call("Bash", { command: "./bin/merge_pr.sh 31 --dry-run" }, 1400),
      result("Bash", "category=ci → REFUSED: touches enforcement machinery", 1000, true),
      think("Refusal is the correct outcome. Not overriding it."),
      {
        after: 1100,
        event: {
          kind: "error",
          message: "merge_pr.sh exited 3 — PR #31 requires a human reviewer",
        },
      },
      {
        after: 700,
        event: {
          kind: "finished",
          text: "1 merged, 1 refused. #31 left open for a human.",
          exit_code: 3,
          is_error: true,
          usage: usage(31400, 4200, 88000, 0.27),
        },
      },
    ],
  },
  {
    name: "domain-finance",
    harness: "open_code",
    cwd: "/home/reljod/repo/Jod/domains/finance",
    model: "gpt-5-codex",
    prompt: "Draft the finance domain's system-of-record decision.",
    ending: "running",
    beats: [
      call("Read", { file_path: "domains/README.md" }),
      result("Read", "41 lines"),
      think("Finance is the one domain still marked TBD."),
      call("Glob", { pattern: "domains/**/*.md" }),
      result("Glob", "12 files"),
      say("Comparing Notion, a ledger file, and Actual Budget on reversibility."),
      call("WebSearch", { query: "plain-text accounting vs hosted ledger 2026" }, 2600),
      result("WebSearch", "9 results"),
      think("Plain text wins on auditability but loses on mobile capture."),
      call("Write", { file_path: "domains/finance/README.md" }, 1800),
      result("Write", "88 lines written"),
      say("Draft is down. Flagging the mobile-capture tradeoff for Reljod."),
    ],
  },
];

const HARNESS_LABEL: Record<HarnessKind, string> = {
  claude_code: "Claude Code",
  open_code: "OpenCode",
  agy: "AGY",
};

function summaryFor(bp: Blueprint, id: string, createdAt: number): AgentSummary {
  const session = `${bp.harness}-${id.slice(0, 8)}`;
  return {
    id,
    name: bp.name,
    harness: bp.harness,
    harness_label: HARNESS_LABEL[bp.harness],
    status: "running",
    cwd: bp.cwd,
    model: bp.model,
    permission: bp.harness === "agy" ? "ask" : "accept_edits",
    tmux_session: `jod-${bp.name}`,
    attach_command: `tmux attach -t jod-${bp.name}`,
    switch_command: `tmux switch-client -t jod-${bp.name}`,
    session_closed: false,
    created_at_ms: createdAt,
    session_id: session,
    usage: {},
    event_count: 0,
    last_message: null,
    stream_path: `~/.jod/streams/${id}.jsonl`,
  };
}

export class SimTransport implements Transport {
  readonly label = "SIM";

  private handlers: TransportHandlers | null = null;
  private agents = new Map<string, AgentSummary>();
  private streams = new Map<string, AgentEnvelope[]>();
  private timers = new Set<ReturnType<typeof setTimeout>>();
  private seq = new Map<string, number>();
  private rng: () => number;
  private nextId = 1;

  constructor(
    private readonly reason: string,
    seed = 0x5eed,
  ) {
    this.rng = makeRng(seed);
  }

  start(handlers: TransportHandlers): void {
    this.handlers = handlers;
    handlers.onLink({ phase: "simulated", reason: this.reason });

    // Stagger the fleet so the graph assembles rather than pops into place.
    BLUEPRINTS.forEach((bp, i) => {
      this.later(() => this.launch(bp), 350 + i * 1100);
    });
  }

  stop(): void {
    for (const t of this.timers) clearTimeout(t);
    this.timers.clear();
    this.handlers = null;
  }

  private later(fn: () => void, ms: number): void {
    const t = setTimeout(() => {
      this.timers.delete(t);
      if (this.handlers) fn();
    }, ms);
    this.timers.add(t);
  }

  private id(): string {
    return `sim-${String(this.nextId++).padStart(4, "0")}-${Math.floor(this.rng() * 1e6)
      .toString(16)
      .padStart(5, "0")}`;
  }

  private launch(bp: Blueprint): void {
    const id = this.id();
    const now = Date.now();
    const agent = summaryFor(bp, id, now);
    this.agents.set(id, agent);
    this.streams.set(id, []);
    this.seq.set(id, -1);

    this.emit(id, {
      kind: "started",
      session_id: agent.session_id,
      model: bp.model,
    });
    this.pushReport();

    // Walk the script, jittered so no two agents ever tick in lockstep.
    let t = 0;
    for (const beat of bp.beats) {
      t += beat.after * (0.75 + this.rng() * 0.6);
      this.later(() => this.emit(id, beat.event), t);
    }

    // A blueprint that ends "running" keeps working: loop its tail forever so
    // the HUD has permanent live traffic to render.
    if (bp.ending === "running") {
      const tail = bp.beats.slice(-6);
      let loop = t + 2200;
      for (let cycle = 0; cycle < 40; cycle++) {
        for (const beat of tail) {
          loop += beat.after * (0.8 + this.rng() * 0.9);
          this.later(() => this.emit(id, beat.event), loop);
        }
      }
    }
  }

  /** Stamp an event with envelope fields, fold it into state, publish it. */
  private emit(agentId: string, event: AgentEvent): void {
    const agent = this.agents.get(agentId);
    if (!agent) return;

    // `seq` starts at 0, matching the API — so the very first event an agent
    // emits (`started`) is seq 0. Seeding from 1 here would mean the simulation
    // never exercised the off-by-one that an exclusive `after_seq` cursor and a
    // `?? 0` dedupe default both get wrong.
    const seq = (this.seq.get(agentId) ?? -1) + 1;
    this.seq.set(agentId, seq);
    const envelope: AgentEnvelope = {
      ...event,
      agent_id: agentId,
      at_ms: Date.now(),
      seq,
    } as AgentEnvelope;

    // Mirror core/src/service.rs::apply so the roster stays consistent with
    // what the real orchestrator would have computed from the same stream.
    agent.event_count = seq + 1;
    if (event.kind === "message") agent.last_message = event.text;
    if (event.kind === "started" && event.model) agent.model = event.model;
    if (event.kind === "finished") {
      agent.status = event.is_error ? "failed" : "completed";
      agent.session_closed = true;
      agent.usage = event.usage;
      if (event.text) agent.last_message = event.text;
    } else {
      // Accumulate a plausible token burn per event so cost animates.
      const u = agent.usage;
      u.input_tokens = (u.input_tokens ?? 0) + Math.floor(400 + this.rng() * 2600);
      u.output_tokens = (u.output_tokens ?? 0) + Math.floor(60 + this.rng() * 520);
      u.cache_read_tokens = (u.cache_read_tokens ?? 0) + Math.floor(this.rng() * 9000);
      u.cost_usd = Number(((u.cost_usd ?? 0) + this.rng() * 0.012).toFixed(4));
    }

    const stream = this.streams.get(agentId);
    if (stream) {
      stream.push(envelope);
      if (stream.length > 4000) stream.splice(0, stream.length - 4000);
    }

    this.handlers?.onEnvelope(envelope);
    if (event.kind === "finished") this.pushReport();
  }

  private pushReport(): void {
    this.handlers?.onReport(this.report());
  }

  private report(): Report {
    const agents = [...this.agents.values()];
    const tally = (s: AgentStatus) => agents.filter((a) => a.status === s).length;
    return {
      running: tally("running"),
      completed: tally("completed"),
      failed: tally("failed"),
      killed: tally("killed"),
      total_cost_usd: agents.reduce((n, a) => n + (a.usage.cost_usd ?? 0), 0),
      agents,
    };
  }

  async spawn(request: SpawnRequest): Promise<AgentSummary> {
    const bp: Blueprint = {
      name: request.name,
      harness: request.harness,
      cwd: request.cwd,
      model: request.model ?? "claude-opus-5",
      prompt: request.prompt,
      ending: "running",
      beats: [
        think(`Reading the task: ${request.prompt.slice(0, 90)}`),
        call("Glob", { pattern: "**/*" }),
        result("Glob", "scanning workspace"),
        say("Working the request now."),
        call("Read", { file_path: `${request.cwd}/AGENTS.md` }),
        result("Read", "charter loaded"),
        think("Planning the first reversible step."),
      ],
    };
    this.launch(bp);
    const created = [...this.agents.values()].at(-1)!;
    return created;
  }

  async kill(agentId: string): Promise<void> {
    const agent = this.agents.get(agentId);
    if (!agent || agent.status !== "running") return;
    agent.status = "killed";
    agent.session_closed = true;
    this.emit(agentId, {
      kind: "error",
      message: "killed by operator",
    });
    this.pushReport();
  }

  /** Mirrors the API: an omitted cursor returns seq 0 onward, exclusive otherwise. */
  async events(agentId: string, sinceSeq?: number): Promise<AgentEnvelope[]> {
    const stream = this.streams.get(agentId) ?? [];
    return sinceSeq === undefined ? [...stream] : stream.filter((e) => e.seq > sinceSeq);
  }

  /** No auth in simulation — nothing real can happen, so it is always writable. */
  async authenticate(): Promise<"write"> {
    return "write";
  }

  async harnesses(): Promise<HarnessInfo[]> {
    return [
      {
        id: "claude_code",
        label: "Claude Code",
        available: true,
        path: "/usr/local/bin/claude",
      },
      { id: "open_code", label: "OpenCode", available: true, path: "/usr/local/bin/opencode" },
      { id: "agy", label: "AGY", available: true, path: "/usr/local/bin/agy" },
    ];
  }

  async history(limit: number): Promise<StoredRun[]> {
    return [...this.agents.values()].slice(0, limit).map((a) => ({
      id: a.id,
      name: a.name,
      harness: a.harness,
      status: a.status,
      cwd: a.cwd,
      session_id: a.session_id,
      tmux_session: a.tmux_session,
      created_at_ms: a.created_at_ms,
      summary: a as unknown as StoredRun["summary"],
    }));
  }
}
