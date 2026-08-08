import { useState } from "react";

import type {
  HarnessInfo,
  HarnessKind,
  PermissionPolicy,
  SpawnArgs,
} from "../types";

const PERMISSIONS: { value: PermissionPolicy; label: string; hint: string }[] = [
  { value: "ask", label: "Ask", hint: "Tool calls needing approval are refused" },
  { value: "accept_edits", label: "Accept edits", hint: "File edits go through" },
  { value: "bypass", label: "Bypass", hint: "Auto-approve everything" },
];

interface Props {
  harnesses: HarnessInfo[];
  defaultWorkdir: string;
  disabled: boolean;
  onSpawn: (args: SpawnArgs) => Promise<void>;
}

export function SpawnForm({ harnesses, defaultWorkdir, disabled, onSpawn }: Props) {
  const available = harnesses.filter((h) => h.available);
  const [name, setName] = useState("");
  const [harness, setHarness] = useState<HarnessKind>("claude_code");
  const [prompt, setPrompt] = useState("");
  const [cwd, setCwd] = useState(defaultWorkdir);
  const [model, setModel] = useState("");
  const [permission, setPermission] = useState<PermissionPolicy>("ask");
  const [busy, setBusy] = useState(false);

  const canSubmit =
    !disabled && !busy && prompt.trim().length > 0 && available.length > 0;

  async function submit(event: React.FormEvent) {
    event.preventDefault();
    if (!canSubmit) return;
    setBusy(true);
    try {
      await onSpawn({
        name: name.trim() || "agent",
        harness,
        prompt,
        cwd: cwd.trim(),
        model: model.trim(),
        permission,
      });
      // Keep the harness/cwd/model settings — only the task itself is one-shot.
      setPrompt("");
      setName("");
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className="spawn" onSubmit={submit}>
      <h2>Delegate a task</h2>

      <label>
        <span>Agent name</span>
        <input
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="scout"
        />
      </label>

      <label>
        <span>Harness</span>
        <select
          value={harness}
          onChange={(e) => setHarness(e.target.value as HarnessKind)}
        >
          {harnesses.map((h) => (
            <option key={h.id} value={h.id} disabled={!h.available}>
              {h.label}
              {h.available ? "" : " — not installed"}
            </option>
          ))}
        </select>
      </label>

      <label>
        <span>Task</span>
        <textarea
          value={prompt}
          onChange={(e) => setPrompt(e.target.value)}
          rows={5}
          placeholder="Summarise what this repository does, in three sentences."
        />
      </label>

      <label>
        <span>Working directory</span>
        <input value={cwd} onChange={(e) => setCwd(e.target.value)} />
      </label>

      <div className="row">
        <label>
          <span>Model</span>
          <input
            value={model}
            onChange={(e) => setModel(e.target.value)}
            placeholder="default"
          />
        </label>

        <label>
          <span>Permissions</span>
          <select
            value={permission}
            onChange={(e) => setPermission(e.target.value as PermissionPolicy)}
          >
            {PERMISSIONS.map((p) => (
              <option key={p.value} value={p.value} title={p.hint}>
                {p.label}
              </option>
            ))}
          </select>
        </label>
      </div>

      <button type="submit" disabled={!canSubmit}>
        {busy ? "Starting…" : "Delegate"}
      </button>
    </form>
  );
}
