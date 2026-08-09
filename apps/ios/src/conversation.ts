/**
 * The app's behaviour, with no React in it.
 *
 * `session.ts` is a pure reducer and `client.ts` is pure transport; this is the
 * part that decides *when* to call them — and therefore the part where the bugs
 * that matter actually live: sending a second prompt while one is in flight,
 * losing the resume cursor, double-rendering a replayed event, going quiet
 * after the phone slept.
 *
 * It is a plain observable store for the same reason `apps/web` uses one: the
 * component tree should be a projection of state, not the owner of it. React
 * subscribes through `useSyncExternalStore` and nothing here imports React, so
 * every rule below is testable by driving the store directly.
 */

import type { AgentEnvelope, AgentSummary, HarnessKind, PermissionPolicy } from "./contract";
import { JodClient, UnauthorizedError, type Scope, type SpawnBody } from "./client";
import { HELP, parse as parseSlash, type Slash } from "./commands";
import {
  browserOriginMemory,
  normaliseOrigin,
  resolveBase,
  type OriginMemory,
} from "./origin";
import {
  abandonTurn,
  applyEvent,
  attachAgent,
  beginTurn,
  clearTranscript,
  defaultName,
  newConversation,
  newSession,
  push,
  resumeSession,
  resolveSession,
  setAgents,
  setFollowing,
  setHarness,
  setInput,
  setModel,
  setPane,
  setTeam,
  setTeams,
  takeInput,
  toAgentLines,
  toggleDetails,
  toggleThinking,
  togglePane,
  type Pane,
  type SessionState,
} from "./session";

/** Where the app is with respect to the daemon. */
export type Link =
  | { phase: "probing" }
  /**
   * We do not know where the daemon *is*. Only the packaged app reaches this:
   * served from `tauri://localhost`, it has no same-origin API to fall back on.
   */
  | { phase: "address"; reason: string }
  /** Reachable, but this device has no valid session. Show the token gate. */
  | { phase: "auth"; reason: string }
  | { phase: "live"; scope: Scope }
  | { phase: "offline"; reason: string };

/**
 * Where the last known scope is kept between launches.
 *
 * **Only the scope, never the token.** The session cookie is `HttpOnly` and
 * survives a restart, but nothing in JavaScript can read it — so on relaunch
 * the app knows it is still authenticated and has no idea whether it is allowed
 * to write. Without this it would fall back to read-only and the composer would
 * be dead until the user pasted a token again, every single time.
 *
 * A scope is not a credential: it grants nothing, and the daemon re-checks it
 * on every request. If this is stale the spawn is refused and the refusal is
 * shown, which is the same thing that happens to a tampered value.
 */
export interface ScopeMemory {
  read(): Scope | null;
  write(scope: Scope): void;
}

/** `localStorage`, where it exists and is allowed to be used. */
export function browserScopeMemory(key = "jod.scope"): ScopeMemory {
  return {
    read() {
      try {
        const value = globalThis.localStorage?.getItem(key);
        return value === "write" || value === "read" ? value : null;
      } catch {
        // Safari throws rather than returning null with storage blocked.
        return null;
      }
    },
    write(scope) {
      try {
        globalThis.localStorage?.setItem(key, scope);
      } catch {
        // Not being able to remember is a worse session, not a broken one.
      }
    },
  };
}

export interface ConversationOptions {
  client: JodClient;
  harness?: HarnessKind;
  model?: string | null;
  /** Left undefined so the daemon picks its first allowed root. */
  cwd?: string;
  permission?: PermissionPolicy;
  /**
   * The team to watch, if any. `null` leaves the sheet saying so rather than
   * showing an empty board — the same rule as `jod tui --team`.
   */
  team?: string | null;
  /** Injected so tests are not at the mercy of a global. */
  scopeMemory?: ScopeMemory;
  originMemory?: OriginMemory;
  /** `location.protocol`. Injected for the same reason. */
  protocol?: string;
}

export interface ConversationView {
  session: SessionState;
  link: Link;
  /** False while a read-only session is in force, or the link is not live. */
  canSend: boolean;
}

type Listener = () => void;

export class Conversation {
  private readonly client: JodClient;
  private readonly cwd: string | undefined;
  private readonly permission: PermissionPolicy | undefined;
  private readonly scopeMemory: ScopeMemory;
  private readonly originMemory: OriginMemory;
  private readonly protocol: string | undefined;

  private state: SessionState;
  private link: Link = { phase: "probing" };
  private scope: Scope = "read";

  private listeners = new Set<Listener>();
  private view: ConversationView;

  /** Closes the live stream. Null when nothing is being followed. */
  private detach: (() => void) | null = null;
  /**
   * Highest `seq` already folded in, per agent.
   *
   * Starts life absent rather than at 0, because `seq` 0 is a real event — the
   * `started` one, carrying `session_id`. Treating "nothing seen" as 0 would
   * drop it and silently break threading on every single run.
   */
  private lastSeq = new Map<string, number>();

  constructor(options: ConversationOptions) {
    this.client = options.client;
    this.cwd = options.cwd;
    this.permission = options.permission;
    this.scopeMemory = options.scopeMemory ?? browserScopeMemory();
    this.originMemory = options.originMemory ?? browserOriginMemory();
    this.protocol = options.protocol ?? globalThis.location?.protocol;
    // What this device was last told it could do. The daemon re-checks it on
    // every request, so a stale value costs a refusal, not an escalation.
    this.scope = this.scopeMemory.read() ?? "read";
    // The harness lives in the session, not here: `/harness` changes it
    // mid-conversation and a spawn must use what is current, exactly as the
    // TUI reads it off `app` rather than off its start-up options.
    this.state = newSession(options.harness ?? "claude_code", options.model ?? null);
    this.state = { ...this.state, team: options.team ?? null };
    this.view = this.snapshot();
  }

  // ─── store plumbing ──────────────────────────────────────────────────────

  subscribe = (listener: Listener): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  /**
   * The current view.
   *
   * Cached and only replaced when something actually changed, because
   * `useSyncExternalStore` compares by identity and a fresh object every call
   * is an infinite render loop.
   */
  getSnapshot = (): ConversationView => this.view;

  private snapshot(): ConversationView {
    return {
      session: this.state,
      link: this.link,
      canSend: this.link.phase === "live" && this.scope === "write",
    };
  }

  private emit(): void {
    this.view = this.snapshot();
    for (const listener of this.listeners) listener();
  }

  private update(next: SessionState): void {
    this.state = next;
    this.emit();
  }

  private notice(text: string): void {
    this.update(push(this.state, { kind: "notice", text }));
  }

  // ─── connection ──────────────────────────────────────────────────────────

  /**
   * Find out whether this device already has a usable session.
   *
   * The cookie is `HttpOnly`, so the only honest way to ask is to make a
   * request and see. `/v1/harnesses` is the cheapest authenticated route, and
   * its answer is worth having anyway — it is what tells the user whether the
   * harness they are about to delegate to is even installed on the box.
   */
  async probe(): Promise<void> {
    // Before anything can be asked, we have to know where to ask. In the
    // browser that is the current origin; in the packaged app there is no such
    // thing, and firing a relative fetch there throws a URL SyntaxError that
    // reads like nonsense ("The string did not match the expected pattern").
    const base = resolveBase(this.originMemory, this.protocol);
    if (base === null) {
      this.setLink({
        phase: "address",
        reason: "where is the daemon?",
      });
      return;
    }
    this.client.setBase(base);

    this.setLink({ phase: "probing" });
    try {
      const harnesses = await this.client.harnesses();
      // A session that answers is at least a read session. Scope is only known
      // for certain from `POST /v1/session`, so anything not already upgraded
      // to write stays read — fail safe, and the composer stays disabled
      // rather than firing a request that 403s.
      this.setLink({ phase: "live", scope: this.scope });
      const missing = harnesses.filter((h) => !h.available).map((h) => h.label);
      if (missing.length === harnesses.length && harnesses.length > 0) {
        this.notice("no harness is installed on the daemon — nothing can start");
      }
      await this.refreshRoster();
      await this.refreshTeam();
    } catch (e) {
      if (e instanceof UnauthorizedError) {
        this.setLink({ phase: "auth", reason: "this device needs a token" });
        return;
      }
      this.setLink({ phase: "offline", reason: describe(e) });
    }
  }

  /** Exchange a bearer token for a session cookie, then go live. */
  async connect(token: string): Promise<void> {
    if (token.trim() === "") {
      this.setLink({ phase: "auth", reason: "a token is required" });
      return;
    }
    // The link deliberately stays in `auth` for the duration of the exchange.
    // Flipping it to `probing` would unmount the gate, flash the main shell
    // behind it, and then mount a *fresh* gate on failure — losing the field
    // the user is looking at. The gate shows its own progress instead.
    try {
      const info = await this.client.authenticate(token);
      this.scope = info.scope;
      this.scopeMemory.write(info.scope);
      this.setLink({ phase: "live", scope: info.scope });
      if (info.scope !== "write") {
        this.notice("this token is read-only — you can watch, but not delegate");
      }
      await this.refreshRoster();
      await this.refreshTeam();
    } catch (e) {
      if (e instanceof UnauthorizedError) {
        this.setLink({ phase: "auth", reason: "that token was refused" });
        return;
      }
      this.setLink({ phase: "offline", reason: describe(e) });
    }
  }

  private setLink(link: Link): void {
    this.link = link;
    this.emit();
  }

  /**
   * Record where the daemon lives, then carry on as if we had always known.
   *
   * Rejects anything that is not an http(s) origin rather than storing it and
   * failing later with a URL error the user cannot interpret.
   */
  async setOrigin(input: string): Promise<void> {
    const origin = normaliseOrigin(input);
    if (origin === null) {
      this.setLink({
        phase: "address",
        reason: "that is not an address — try jod-cloud:8787",
      });
      return;
    }
    this.originMemory.write(origin);
    this.client.setBase(origin);
    await this.probe();
  }

  // ─── the turn ────────────────────────────────────────────────────────────

  /**
   * Send whatever is in the composer.
   *
   * The order is the TUI's, and the first step is the one that matters most: a
   * **slash line is an instruction to Jod, not to the agent**, so it is parsed
   * and run before any of the refusals below. Switching the model for the next
   * turn is exactly the sort of thing you do while waiting for this one, and a
   * read-only session can still clear its own transcript.
   *
   * Then three refusals: nothing typed, a turn already running, and — new here,
   * because a phone can hold a read-only token — no authority to write.
   */
  async send(): Promise<void> {
    const slash = parseSlash(this.state.input.trim());
    if (slash !== null) {
      this.update(setInput(this.state, ""));
      await this.applySlash(slash);
      return;
    }
    if (this.state.busy) {
      this.notice("still working — wait for this turn to finish");
      return;
    }
    const { state, prompt } = takeInput(this.state);
    if (prompt === null) return;
    if (!this.view.canSend) {
      this.notice(
        this.link.phase === "live"
          ? "this session is read-only"
          : "not connected to the daemon",
      );
      return;
    }
    // Busy *before* the request goes out, and the prompt on screen with it.
    // Waiting for the daemon to answer would leave the composer live across the
    // round trip, and a second tap in that window starts a second agent.
    this.update(beginTurn(state, prompt));
    await this.delegate(prompt);
  }

  /**
   * Carry out a slash command. Mirrors `apply_slash` in `cli/src/tui/mod.rs`.
   *
   * Almost everything here is a pure transformation of session state, which is
   * why the reducer owns it and this only chooses. The two exceptions need the
   * daemon: `/team` has to read a board written by other processes, and
   * `/sessions` is worth nothing without a fresh roster.
   */
  async applySlash(slash: Slash): Promise<void> {
    switch (slash.kind) {
      case "help":
        for (const [usage, what] of HELP) {
          this.notice(`${usage.padEnd(18)} ${what}`);
        }
        return;

      case "harness":
        this.update(setHarness(this.state, slash.harness));
        return;

      case "model":
        this.update(setModel(this.state, slash.model));
        return;

      case "thinking":
        this.update(toggleThinking(this.state));
        return;

      case "details":
        this.update(toggleDetails(this.state));
        return;

      case "new":
        this.update(newConversation(this.state));
        return;

      case "sessions":
        // The sheet is the list, so open it — and refresh first, because the
        // ids it is about to be read off have to be current.
        await this.refreshRoster();
        this.update(setPane(this.state, "agents"));
        this.notice(
          "pick an id from the sheet, then /resume <id> — the shown prefix is enough",
        );
        return;

      case "resume": {
        const resolved = resolveSession(this.state, slash.id);
        switch (resolved.kind) {
          case "session":
            this.update(resumeSession(this.state, resolved.session));
            this.notice(`continuing ${resolved.session}`);
            return;
          case "verbatim":
            this.update(resumeSession(this.state, resolved.typed));
            // Say when it matched nothing on screen. A typo is otherwise
            // indistinguishable from a real resume until the harness rejects
            // it several seconds later.
            this.notice(
              this.state.agents.length === 0
                ? `continuing ${resolved.typed}`
                : `continuing ${resolved.typed} — not one of the agents listed, passing it on as typed`,
            );
            return;
          case "no_session":
            this.notice(
              `${resolved.agent} has not reported a conversation yet — resuming it would start a fresh one`,
            );
            return;
          case "ambiguous":
            this.notice(`${slash.id} matches ${resolved.count} agents — type more of it`);
            return;
        }
        return;
      }

      case "agents":
        this.update(togglePane(this.state, "agents"));
        if (this.state.pane === "agents") await this.refreshRoster();
        return;

      case "team":
        this.update(togglePane(this.state, "team"));
        if (this.state.pane === "team") await this.refreshTeam();
        return;

      case "clear":
        this.update(clearTranscript(this.state));
        return;

      case "exit":
        // An app cannot quit itself on iOS, and the TUI's `/exit` is not really
        // about quitting: it is about leaving while the work carries on. That
        // is exactly what this does — stop following the stream, keep the
        // agent running on the box.
        this.stop();
        this.notice("stopped watching — the agent keeps running on the box");
        return;

      case "needs_argument":
        this.notice(`usage: ${slash.usage}`);
        return;

      case "unknown":
        this.notice(`${slash.what} is not a command — /help lists them`);
        return;
    }
  }

  private async delegate(prompt: string): Promise<void> {
    const body: SpawnBody = {
      prompt,
      // From the session, not from the options: `/harness` and `/model` change
      // these mid-conversation and a spawn must use what is current.
      harness: this.state.harness,
      name: defaultName(prompt),
      // The resume cursor is the whole reason this is a conversation and not a
      // series of unrelated one-shots. It advances to the exact session the
      // harness reported on the previous turn.
      resume: this.state.resume,
    };
    if (this.cwd !== undefined) body.cwd = this.cwd;
    if (this.state.model) body.model = this.state.model;
    if (this.permission !== undefined) body.permission = this.permission;

    let agent: AgentSummary;
    try {
      agent = await this.client.spawn(body);
    } catch (e) {
      // The turn never started, so release the composer — otherwise a refused
      // spawn wedges the app until it is restarted.
      this.update(abandonTurn(this.state));
      if (e instanceof UnauthorizedError) {
        // 401 and 403 are different failures and must not be treated alike.
        // 401 means the session is gone and only a new token fixes it. 403
        // means the session is fine but lacks the authority — bouncing to the
        // gate there would ask for a token that is already correct, and the
        // remembered scope was simply wrong. Correct it and say why.
        if (e.status === 403) {
          this.scope = "read";
          this.scopeMemory.write("read");
          this.setLink({ phase: "live", scope: "read" });
          this.notice(`could not start: ${e.message}`);
          return;
        }
        this.setLink({ phase: "auth", reason: "the session expired" });
        this.notice("could not start: the session expired");
        return;
      }
      this.notice(`could not start: ${describe(e)}`);
      return;
    }

    this.update(attachAgent(this.state, agent.id));
    this.follow(agent.id);
    void this.refreshRoster();
  }

  /**
   * Follow one delegation.
   *
   * The per-agent stream replays history and then goes live on one connection,
   * so there is no window between "read what happened" and "start listening" —
   * and therefore nothing to miss. Any previous stream is closed first: two
   * live streams would double-apply every event.
   */
  follow(agentId: string, resumeFrom?: number): void {
    this.detach?.();
    const after = resumeFrom ?? this.lastSeq.get(agentId);
    this.detach = this.client.stream(
      agentId,
      {
        onEnvelope: (envelope) => this.ingest(envelope),
        onLagged: (missed) => this.recover(agentId, missed),
        onError: ({ unauthorized }) => {
          if (!unauthorized) return; // transient; EventSource retries itself
          this.detach?.();
          this.detach = null;
          this.setLink({ phase: "auth", reason: "the session expired" });
        },
      },
      after,
    );
  }

  /** Stop following. Called when the view goes away; the agent keeps running. */
  stop(): void {
    this.detach?.();
    this.detach = null;
  }

  /**
   * Fold one envelope in, exactly once.
   *
   * The stream is resumable, which means the server may legally replay an event
   * this client has already drawn — on reconnect, or after a `lagged` recovery.
   * Deduping on `seq` is therefore the contract, not a workaround.
   */
  private ingest(envelope: AgentEnvelope): void {
    if (envelope.agent_id !== this.state.currentAgentId) return;

    const seen = this.lastSeq.get(envelope.agent_id);
    if (seen !== undefined && envelope.seq <= seen) return;
    this.lastSeq.set(envelope.agent_id, envelope.seq);

    const before = this.state.busy;
    this.update(applyEvent(this.state, envelope));

    // A turn that just ended changes the roster, and nothing else will say so.
    if (before && !this.state.busy) void this.refreshRoster();
  }

  /**
   * The daemon dropped events. Re-read them rather than pretending.
   *
   * A HUD that knows it is stale is worth more than one that confidently shows
   * the wrong thing, so this is surfaced in the transcript as well as repaired.
   */
  private async recover(agentId: string, missed: number): Promise<void> {
    this.notice(`the daemon dropped ${missed} events — re-reading`);
    try {
      const page = await this.client.events(agentId, this.lastSeq.get(agentId));
      for (const envelope of page.events) this.ingest(envelope);
    } catch (e) {
      this.notice(`could not re-read: ${describe(e)}`);
    }
  }

  /**
   * Reconnect after the phone was asleep.
   *
   * iOS suspends a backgrounded app and its sockets go with it, so coming back
   * means: catch up over REST from the last event actually rendered, then
   * re-open the stream from there.
   */
  async resumeAfterBackground(): Promise<void> {
    const agentId = this.state.currentAgentId;
    if (!agentId) {
      await this.refreshRoster();
      return;
    }
    try {
      const page = await this.client.events(agentId, this.lastSeq.get(agentId));
      for (const envelope of page.events) this.ingest(envelope);
      this.follow(agentId);
      await this.refreshRoster();
    } catch (e) {
      if (e instanceof UnauthorizedError) {
        this.setLink({ phase: "auth", reason: "the session expired" });
        return;
      }
      this.setLink({ phase: "offline", reason: describe(e) });
    }
  }

  // ─── the roster ──────────────────────────────────────────────────────────

  async refreshRoster(): Promise<void> {
    try {
      const agents = await this.client.agents();
      this.update(setAgents(this.state, toAgentLines(agents)));
    } catch {
      // A roster that failed to load is a worse reason to interrupt someone
      // than it is a thing worth saying. The transcript is the app.
    }
  }

  /**
   * Re-read the team from the daemon.
   *
   * Members and tasks are written by the teammates themselves, in their own
   * processes on the box, so the only way to know the current state is to ask —
   * there is no local copy that could be authoritative. This is the TUI's
   * `refresh_team`, with an HTTP hop where it had a SQLite handle.
   *
   * With no team named, the sheet says so; there is nothing to fetch. A failed
   * read leaves the last known board on screen rather than emptying it, for the
   * same reason the roster does: stale beats blank.
   */
  async refreshTeam(): Promise<void> {
    await this.discoverTeams();
    const team = this.state.team;
    if (team === null) return;
    try {
      const view = await this.client.team(team);
      this.update(setTeam(this.state, team, view.members, view.tasks));
    } catch (e) {
      // A team the daemon has never heard of is worth saying once: it means the
      // name is wrong, and an empty sheet would read as "nobody has joined yet".
      if (this.state.members.length === 0 && this.state.tasks.length === 0) {
        this.notice(`could not read team ${team}: ${describe(e)}`);
      }
    }
  }

  /**
   * Learn which teams exist, and adopt one if the answer is unambiguous.
   *
   * `jod tui` takes its team from `--team <name>`; a phone has no command line,
   * so the names are asked for. Exactly one team is adopted without asking —
   * there is nothing to choose. Several are left for the sheet to offer, because
   * guessing which board someone wants to watch is worse than showing the list.
   *
   * A configured team always wins: it was named deliberately.
   */
  private async discoverTeams(): Promise<void> {
    if (this.state.teams.length > 0) return;
    let names: string[];
    try {
      names = await this.client.teams();
    } catch {
      // Not knowing which teams exist is a worse sheet, not a broken app —
      // and an older daemon without the route is exactly this case.
      return;
    }
    this.update(setTeams(this.state, names));
    if (this.state.team === null && names.length === 1) {
      this.update(setTeam(this.state, names[0]!, [], []));
    }
  }

  /** Watch a different team, or none. */
  async watchTeam(team: string | null): Promise<void> {
    this.update(setTeam(this.state, team, [], []));
    if (team !== null) await this.refreshTeam();
  }

  async kill(agentId: string): Promise<void> {
    try {
      await this.client.kill(agentId);
    } catch (e) {
      this.notice(`could not stop: ${describe(e)}`);
      return;
    }
    if (agentId === this.state.currentAgentId) {
      this.update({ ...this.state, busy: false });
    }
    await this.refreshRoster();
  }

  // ─── view actions ────────────────────────────────────────────────────────

  setInput(text: string): void {
    this.update(setInput(this.state, text));
  }

  setFollowing(following: boolean): void {
    if (following === this.state.following) return;
    this.update(setFollowing(this.state, following));
  }

  toggleThinking(): void {
    this.update(toggleThinking(this.state));
  }

  toggleDetails(): void {
    this.update(toggleDetails(this.state));
  }

  /**
   * Open a sheet, or close it if it is already open — the TUI's `Ctrl-A` and
   * `Ctrl-G`. Opening one refreshes what it is about to show, because both are
   * views of state other processes are writing.
   */
  async togglePane(pane: Exclude<Pane, "chat">): Promise<void> {
    this.update(togglePane(this.state, pane));
    if (this.state.pane !== pane) return;
    if (pane === "agents") await this.refreshRoster();
    else await this.refreshTeam();
  }

  setPane(pane: Pane): void {
    this.update(setPane(this.state, pane));
  }

  clear(): void {
    this.update(clearTranscript(this.state));
  }

  /** Greeting, mirroring the TUI's opening notice. */
  greet(hint: string): void {
    this.notice(hint);
  }
}

/** Whatever was thrown, as something a person can read. */
export function describe(e: unknown): string {
  if (e instanceof Error) return e.message;
  if (typeof e === "string") return e;
  return String(e);
}
