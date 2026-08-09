import { useEffect, useMemo, useSyncExternalStore } from "react";

import { completions } from "./commands";
import type { Conversation } from "./conversation";
import { statusLine } from "./session";
import { AddressGate } from "./components/AddressGate";
import { AgentsSheet } from "./components/AgentsSheet";
import { AuthGate } from "./components/AuthGate";
import { Completions } from "./components/Completions";
import { Composer } from "./components/Composer";
import { StatusBar } from "./components/StatusBar";
import { TeamSheet } from "./components/TeamSheet";
import { Transcript } from "./components/Transcript";

/**
 * The whole screen, and almost none of the behaviour.
 *
 * Everything that can be got wrong lives in `Conversation`, which knows nothing
 * about React; this subscribes to it and draws the result. That split is why
 * the app's rules are tested headless in `test/` rather than through a
 * simulated browser.
 */
export function App({ conversation }: { conversation: Conversation }) {
  const view = useSyncExternalStore(conversation.subscribe, conversation.getSnapshot);
  const { session, link, canSend } = view;

  // Find out whether this device already holds a session, once.
  useEffect(() => {
    void conversation.probe();
    return () => conversation.stop();
  }, [conversation]);

  useKeyboardInset();
  useResumeOnForeground(conversation);

  // Recomputed only when the composer changes: this runs on every keystroke and
  // the result feeds a list that must not flicker.
  const suggestions = useMemo(() => completions(session.input), [session.input]);

  // Address before token: there is no point asking for a credential until we
  // know which daemon it is for.
  if (link.phase === "address") {
    return (
      <AddressGate
        reason={link.reason}
        onSubmit={(address) => conversation.setOrigin(address)}
      />
    );
  }

  if (link.phase === "auth") {
    return (
      <AuthGate
        reason={link.reason}
        onSubmit={(token) => conversation.connect(token)}
      />
    );
  }

  const note =
    link.phase === "offline"
      ? link.reason
      : link.phase === "probing"
        ? "connecting…"
        : link.scope === "write"
          ? null
          : "read-only";

  return (
    <div className="app">
      <div className="topbar">
        <span className="brand">JOD</span>
        <span className={`linkdot ${link.phase}`} aria-label={link.phase} />
        <span className="spacer" />

        {/* `Ctrl-T`, `/details`, `Ctrl-A` and `Ctrl-G`, as things a thumb can
            reach. Every one of them is also a slash command, so the terminal
            muscle memory works too. */}
        <button
          className={`iconbtn${session.showThinking ? " on" : ""}`}
          onClick={() => conversation.toggleThinking()}
          aria-pressed={session.showThinking}
        >
          THINK
        </button>
        <button
          className={`iconbtn${session.showDetails ? " on" : ""}`}
          onClick={() => conversation.toggleDetails()}
          aria-pressed={session.showDetails}
        >
          TOOLS
        </button>
        <button
          className={`iconbtn${session.pane === "agents" ? " on" : ""}`}
          onClick={() => void conversation.togglePane("agents")}
          aria-pressed={session.pane === "agents"}
        >
          AGENTS
        </button>
        <button
          className={`iconbtn${session.pane === "team" ? " on" : ""}`}
          onClick={() => void conversation.togglePane("team")}
          aria-pressed={session.pane === "team"}
        >
          TEAM
        </button>
      </div>

      <Transcript
        entries={session.transcript}
        following={session.following}
        onFollowingChange={(f) => conversation.setFollowing(f)}
      />

      <Completions
        items={suggestions}
        onAccept={(line) => conversation.setInput(line)}
      />

      <Composer
        value={session.input}
        // A slash line is an instruction to Jod, not to the agent, so it stays
        // sendable while a turn is in flight and while the session is
        // read-only — switching the model for the next turn is exactly what
        // you do while waiting for this one.
        disabled={!canSend && !isCommand(session.input)}
        busy={session.busy && !isCommand(session.input)}
        onChange={(v) => conversation.setInput(v)}
        onSend={() => void conversation.send()}
      />

      <StatusBar text={statusLine(session)} busy={session.busy} note={note} />

      {session.pane === "agents" ? (
        <AgentsSheet
          agents={session.agents}
          currentAgentId={session.currentAgentId}
          canWrite={canSend}
          onKill={(id) => void conversation.kill(id)}
          onResume={(id) => void conversation.applySlash({ kind: "resume", id })}
          onClose={() => conversation.setPane("chat")}
        />
      ) : null}

      {session.pane === "team" ? (
        <TeamSheet
          team={session.team}
          teams={session.teams}
          members={session.members}
          tasks={session.tasks}
          onWatch={(name) => void conversation.watchTeam(name)}
          onClose={() => conversation.setPane("chat")}
        />
      ) : null}
    </div>
  );
}

/**
 * Whether the composer holds a slash command rather than a prompt.
 *
 * Only used to decide whether SEND stays live; the authoritative parse happens
 * in `Conversation.send`, where a bare `/` is not a command either.
 */
function isCommand(input: string): boolean {
  const trimmed = input.trim();
  return trimmed.startsWith("/") && trimmed.length > 1;
}

/**
 * Publish the on-screen keyboard's height as `--keyboard`.
 *
 * iOS does not shrink the layout viewport when the keyboard opens — the page
 * keeps its full height and the keyboard is drawn on top, which would leave the
 * composer underneath it. `visualViewport` is the only thing that reports the
 * real visible area, so the difference between the two is the inset the shell
 * needs.
 */
function useKeyboardInset(): void {
  useEffect(() => {
    const vv = window.visualViewport;
    if (!vv) return;

    const apply = () => {
      const inset = Math.max(0, window.innerHeight - vv.height - vv.offsetTop);
      document.documentElement.style.setProperty("--keyboard", `${inset}px`);
    };

    apply();
    vv.addEventListener("resize", apply);
    vv.addEventListener("scroll", apply);
    return () => {
      vv.removeEventListener("resize", apply);
      vv.removeEventListener("scroll", apply);
      document.documentElement.style.removeProperty("--keyboard");
    };
  }, []);
}

/**
 * Catch up when the app comes back from the background.
 *
 * iOS suspends a backgrounded app and its open sockets go with it, so a run
 * that finished while the phone was in a pocket would otherwise never appear.
 * `visibilitychange` is the signal that survives being suspended, where a
 * timer does not.
 */
function useResumeOnForeground(conversation: Conversation): void {
  useEffect(() => {
    const onVisible = () => {
      if (document.visibilityState === "visible") {
        void conversation.resumeAfterBackground();
      }
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => document.removeEventListener("visibilitychange", onVisible);
  }, [conversation]);
}
