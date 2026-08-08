import { invoke } from "@tauri-apps/api/core";
import { register, unregister } from "@tauri-apps/plugin-global-shortcut";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { COMPARE_DEFAULT, MODELS } from "./models";

interface Transcript {
  text: string;
  latency_ms: number;
  cost_usd: number;
  model: string;
  raw_text: string | null;
  repair_ms: number | null;
}

interface ModelResult {
  model: string;
  text: string;
  latency_ms: number;
  cost_usd: number;
  error: string | null;
}

interface Session {
  id: string;
  name: string;
  state: string;
  cwd: string;
}

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

const ptt = $<HTMLButtonElement>("ptt");
const meterFill = $<HTMLDivElement>("meter-fill");
const statusEl = $<HTMLParagraphElement>("status");
const transcriptEl = $<HTMLTextAreaElement>("transcript");
const metricsEl = $<HTMLDivElement>("metrics");
const modelEl = $<HTMLSelectElement>("model");
const languageEl = $<HTMLSelectElement>("language");
const repairEl = $<HTMLInputElement>("repair");
const sessionEl = $<HTMLSelectElement>("session");
const sendBtn = $<HTMLButtonElement>("send");
const sendStatus = $<HTMLParagraphElement>("send-status");
const keyStatus = $<HTMLSpanElement>("key-status");
const compareBtn = $<HTMLButtonElement>("compare-btn");
const compareOut = $<HTMLDivElement>("compare-out");

/** The push-to-talk hotkey. Alt+Space stays clear of macOS Spotlight. */
const HOTKEY = "Alt+Space";

let recording = false;
let meterTimer: number | undefined;

function setStatus(msg: string, kind: "ok" | "err" | "busy" | "idle" = "idle") {
  statusEl.textContent = msg;
  statusEl.className = `status status-${kind}`;
}

// --- recording -------------------------------------------------------------

async function startRecording() {
  if (recording) return;
  try {
    await invoke("start_recording");
    recording = true;
    ptt.classList.add("live");
    setStatus("Listening… release to transcribe.", "busy");
    meterTimer = window.setInterval(pollMeter, 60);
  } catch (e) {
    setStatus(String(e), "err");
  }
}

async function pollMeter() {
  try {
    const s = await invoke<{ recording: boolean; duration_secs: number; level: number }>(
      "recording_state",
    );
    // Speech peaks well below 1.0; scale so normal talking fills most of the bar.
    meterFill.style.width = `${Math.min(100, s.level * 260)}%`;
  } catch {
    /* the meter is cosmetic — never let it break the recording loop */
  }
}

function stopMeter() {
  if (meterTimer) window.clearInterval(meterTimer);
  meterTimer = undefined;
  meterFill.style.width = "0%";
}

async function stopRecording() {
  if (!recording) return;
  recording = false;
  ptt.classList.remove("live");
  stopMeter();
  setStatus("Transcribing…", "busy");

  try {
    const t = await invoke<Transcript>("stop_and_transcribe", {
      model: modelEl.value,
      language: languageEl.value || null,
      repair: repairEl.checked,
      repairModel: null,
    });

    // Append rather than replace, so several utterances build one instruction.
    transcriptEl.value = transcriptEl.value ? `${transcriptEl.value.trim()} ${t.text}` : t.text;

    const bits = [
      `${t.latency_ms} ms`,
      t.repair_ms !== null ? `+${t.repair_ms} ms repair` : null,
      `$${t.cost_usd.toFixed(5)}`,
      t.model,
    ].filter(Boolean);
    metricsEl.textContent = bits.join("  ·  ");
    if (t.raw_text && t.raw_text !== t.text) {
      metricsEl.title = `Before repair: ${t.raw_text}`;
    }
    setStatus("Done.", "ok");
  } catch (e) {
    setStatus(String(e), "err");
  }
}

// Pointer events cover mouse and trackpad; the global hotkey is handled below.
ptt.addEventListener("pointerdown", startRecording);
ptt.addEventListener("pointerup", stopRecording);
ptt.addEventListener("pointerleave", () => {
  if (recording) stopRecording();
});

// Escape abandons the utterance instead of transcribing it.
window.addEventListener("keydown", async (e) => {
  if (e.key === "Escape" && recording) {
    recording = false;
    ptt.classList.remove("live");
    stopMeter();
    await invoke("cancel_recording");
    setStatus("Cancelled.", "idle");
  }
});

// --- global hotkey ---------------------------------------------------------

/**
 * Tauri's global-shortcut fires a single event per press with a state field, so
 * one registration covers both edges of push-to-talk.
 */
async function bindHotkey() {
  try {
    await unregister(HOTKEY).catch(() => {});
    await register(HOTKEY, (event) => {
      if (event.state === "Pressed") startRecording();
      else if (event.state === "Released") stopRecording();
    });
  } catch (e) {
    setStatus(`Hotkey ${HOTKEY} unavailable (${e}). The button still works.`, "err");
  }
}

// --- populate UI -----------------------------------------------------------

function fillModels() {
  for (const m of MODELS) {
    const o = document.createElement("option");
    o.value = m.id;
    const wer = m.wer !== undefined ? ` — WER ${(m.wer * 100).toFixed(1)}%` : "";
    const lat = m.p50 !== undefined ? `, ${m.p50}s` : "";
    o.textContent = `${m.unsuitable ? "⚠ " : ""}${m.label}${wer}${lat}`;
    o.title = m.note;
    modelEl.appendChild(o);
  }
  modelEl.value = MODELS[0].id;
}

async function loadSessions() {
  try {
    const list = await invoke<Session[]>("list_sessions", { jodRepo: null });
    sessionEl.innerHTML = '<option value="">— no Jod session —</option>';
    for (const s of list) {
      const o = document.createElement("option");
      o.value = s.id;
      o.textContent = `${s.name || s.id} (${s.state})`;
      sessionEl.appendChild(o);
    }
    sendStatus.textContent = list.length
      ? `${list.length} session${list.length === 1 ? "" : "s"} found.`
      : "No Jod sessions running.";
  } catch (e) {
    sendStatus.textContent = `Could not list sessions: ${e}`;
  }
}

sessionEl.addEventListener("change", () => {
  sendBtn.disabled = !sessionEl.value;
});

sendBtn.addEventListener("click", async () => {
  const text = transcriptEl.value.trim();
  if (!text || !sessionEl.value) return;
  sendBtn.disabled = true;
  sendStatus.textContent = "Sending…";
  try {
    await invoke("send_to_session", { id: sessionEl.value, message: text, jodRepo: null });
    sendStatus.textContent = `Sent to ${sessionEl.value}.`;
    transcriptEl.value = "";
  } catch (e) {
    sendStatus.textContent = String(e);
  } finally {
    sendBtn.disabled = false;
  }
});

$("copy").addEventListener("click", async () => {
  await writeText(transcriptEl.value);
  setStatus("Copied to clipboard.", "ok");
});

$("clear").addEventListener("click", () => {
  transcriptEl.value = "";
  metricsEl.textContent = "";
});

// --- compare ---------------------------------------------------------------

compareBtn.addEventListener("click", async () => {
  compareBtn.disabled = true;
  compareOut.innerHTML = '<p class="subtle">Running…</p>';
  try {
    const rows = await invoke<ModelResult[]>("compare_models", {
      models: COMPARE_DEFAULT,
      language: languageEl.value || null,
      useLast: true,
    });
    compareOut.innerHTML = "";
    for (const r of rows) {
      const div = document.createElement("div");
      div.className = "cmp";
      div.innerHTML = `<div class="cmp-head"><strong>${r.model}</strong>
        <span class="subtle">${r.error ? "error" : `${r.latency_ms} ms · $${r.cost_usd.toFixed(5)}`}</span></div>
        <div class="cmp-text">${escapeHtml(r.error ?? r.text)}</div>`;
      compareOut.appendChild(div);
    }
  } catch (e) {
    compareOut.innerHTML = `<p class="status-err">${escapeHtml(String(e))}</p>`;
  } finally {
    compareBtn.disabled = false;
  }
});

function escapeHtml(s: string) {
  const d = document.createElement("div");
  d.textContent = s;
  return d.innerHTML;
}

// --- boot ------------------------------------------------------------------

async function boot() {
  fillModels();
  await bindHotkey();
  const hasKey = await invoke<boolean>("key_status");
  keyStatus.textContent = hasKey ? "OpenRouter key loaded" : "no API key";
  keyStatus.className = `pill ${hasKey ? "pill-ok" : "pill-err"}`;
  if (!hasKey) {
    setStatus("Launch via `pnpm app` so Doppler injects OPENROUTER_API_KEY.", "err");
  }
  await loadSessions();
}

boot();
