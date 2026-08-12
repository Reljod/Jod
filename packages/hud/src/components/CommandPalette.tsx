import { useEffect, useMemo, useRef, useState } from "react";
import type { AgentNode, World } from "../state/world";
import { HARNESS_KINDS, harnessCode } from "../types";
import type { HarnessInfo, HarnessKind, PermissionPolicy, Resume, SpawnRequest } from "../types";

interface Props {
  open: boolean;
  world: World;
  harnesses: HarnessInfo[];
  canWrite: boolean;
  /** Pre-filled when continuing an existing thread. */
  seed: { resume: Resume; cwd: string; harness: HarnessKind; name: string } | null;
  onClose(): void;
  onSpawn(req: SpawnRequest): void;
  onSelect(id: string): void;
}

/**
 * ⌘K console: jump to an agent, or delegate a new one.
 *
 * `resume` is a first-class control rather than an afterthought — it is the
 * difference between firing one-shot tasks at a harness and holding an ongoing
 * conversation with it, and retrofitting threading later is painful.
 */
export function CommandPalette({
  open,
  world,
  harnesses,
  canWrite,
  seed,
  onClose,
  onSpawn,
  onSelect,
}: Props) {
  const [query, setQuery] = useState("");
  const [mode, setMode] = useState<"jump" | "spawn">("jump");
  const [name, setName] = useState("");
  const [prompt, setPrompt] = useState("");
  const [cwd, setCwd] = useState("");
  const [harness, setHarness] = useState<HarnessKind>("claude_code");
  const [permission, setPermission] = useState<PermissionPolicy>("ask");
  const [resume, setResume] = useState<Resume>("fresh");
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!open) return;
    setQuery("");
    inputRef.current?.focus();
  }, [open]);

  // Continuing a thread arrives pre-filled from the dossier.
  useEffect(() => {
    if (!seed) return;
    setMode("spawn");
    setResume(seed.resume);
    setCwd(seed.cwd);
    setHarness(seed.harness);
    setName(`${seed.name} (cont.)`);
  }, [seed]);

  const matches = useMemo(() => {
    const q = query.trim().toLowerCase();
    const list: AgentNode[] = [];
    for (const id of world.order) {
      const n = world.agents.get(id);
      if (!n) continue;
      if (
        !q ||
        n.summary.name.toLowerCase().includes(q) ||
        n.summary.cwd.toLowerCase().includes(q) ||
        n.summary.harness.includes(q)
      ) {
        list.push(n);
      }
    }
    return list.slice(0, 8);
  }, [query, world, world.revision]);

  if (!open) return null;

  const available = new Set(harnesses.filter((h) => h.available).map((h) => h.id));
  const canSubmit = canWrite && name.trim() && prompt.trim() && cwd.trim();

  const submit = () => {
    if (!canSubmit) return;
    const req: SpawnRequest = {
      name: name.trim(),
      harness,
      prompt: prompt.trim(),
      cwd: cwd.trim(),
      permission,
    };
    if (resume !== "fresh") req.resume = resume;
    onSpawn(req);
    setName("");
    setPrompt("");
    onClose();
  };

  return (
    <div className="palette-backdrop" onClick={onClose}>
      <div className="palette" onClick={(e) => e.stopPropagation()}>
        <div className="palette-tabs">
          <button className={mode === "jump" ? "on" : ""} onClick={() => setMode("jump")}>
            JUMP
          </button>
          <button className={mode === "spawn" ? "on" : ""} onClick={() => setMode("spawn")}>
            DELEGATE
          </button>
          <span className="spacer" />
          <button className="close" onClick={onClose}>ESC</button>
        </div>

        {mode === "jump" ? (
          <>
            <input
              ref={inputRef}
              className="palette-input"
              placeholder="Find an agent by name, path or harness…"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && matches[0]) {
                  onSelect(matches[0].summary.id);
                  onClose();
                }
              }}
            />
            <div className="palette-results">
              {matches.length === 0 && <p className="empty">No match.</p>}
              {matches.map((n) => (
                <button
                  key={n.summary.id}
                  onClick={() => {
                    onSelect(n.summary.id);
                    onClose();
                  }}
                >
                  <i className={`hx hx-${n.summary.harness}`}>
                    {harnessCode(n.summary.harness)}
                  </i>
                  <span className="pn">{n.summary.name}</span>
                  <span className={`ps st-${n.summary.status}`}>{n.summary.status}</span>
                  <span className="pc">{n.summary.cwd}</span>
                </button>
              ))}
            </div>
          </>
        ) : (
          <div className="palette-form">
            {!canWrite && (
              <p className="readonly">
                This session holds a read-only token — delegation is disabled.
              </p>
            )}
            <label>
              NAME
              <input value={name} onChange={(e) => setName(e.target.value)} placeholder="pr-digest" />
            </label>
            <label>
              WORKING DIRECTORY
              <input
                value={cwd}
                onChange={(e) => setCwd(e.target.value)}
                placeholder="/home/reljod/repo/Jod"
              />
            </label>
            <label className="wide">
              TASK
              <textarea
                value={prompt}
                onChange={(e) => setPrompt(e.target.value)}
                rows={4}
                placeholder="What should this agent do?"
              />
            </label>

            <div className="row">
              <label>
                HARNESS
                <select
                  value={harness}
                  onChange={(e) => setHarness(e.target.value as HarnessKind)}
                >
                  {HARNESS_KINDS.map((k) => (
                    <option key={k} value={k} disabled={harnesses.length > 0 && !available.has(k)}>
                      {harnessCode(k)}
                      {harnesses.length > 0 && !available.has(k) ? " (absent)" : ""}
                    </option>
                  ))}
                </select>
              </label>
              <label>
                PERMISSION
                <select
                  value={permission}
                  onChange={(e) => setPermission(e.target.value as PermissionPolicy)}
                >
                  <option value="ask">ASK</option>
                  <option value="accept_edits">ACCEPT EDITS</option>
                  <option value="bypass">BYPASS</option>
                </select>
              </label>
              <label>
                THREAD
                <select
                  value={typeof resume === "string" ? resume : "session"}
                  onChange={(e) => {
                    const v = e.target.value;
                    setResume(
                      v === "session" && typeof resume !== "string" ? resume : (v as Resume),
                    );
                  }}
                >
                  <option value="fresh">FRESH</option>
                  <option value="last">CONTINUE LAST</option>
                  {typeof resume !== "string" && (
                    <option value="session">SESSION {resume.session.slice(0, 8)}</option>
                  )}
                </select>
              </label>
            </div>

            {permission === "bypass" && (
              <p className="caution">
                BYPASS auto-approves every tool call. Only sane inside a throwaway worktree.
              </p>
            )}

            <button className="go" disabled={!canSubmit} onClick={submit}>
              DELEGATE
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
