#!/usr/bin/env node
/**
 * Reproduces the model selection in docs/RESEARCH.md.
 *
 * Generates Taglish fixtures via OpenRouter TTS (once, cached on disk), then
 * transcribes each with every candidate model and reports word error rate and
 * latency. Run it whenever you want to re-check the default model, or point it
 * at your own recordings by dropping WAV + matching TXT into fixtures/.
 *
 *   pnpm bench            # all models
 *   pnpm bench --top      # only the models actually worth using
 *
 * Requires OPENROUTER_API_KEY, which `pnpm bench` supplies via Doppler.
 */
import { mkdirSync, existsSync, readFileSync, writeFileSync, readdirSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const FIXTURES = join(HERE, "..", "fixtures");
const KEY = process.env.OPENROUTER_API_KEY;
const TOP_ONLY = process.argv.includes("--top");

if (!KEY) {
  console.error(
    "OPENROUTER_API_KEY is not set.\n" +
      "Run: doppler run --project jod-apps --config dev_personal -- node scripts/bench.mjs",
  );
  process.exit(1);
}

/** Ground-truth Taglish, weighted toward how you actually talk to a coding agent. */
const CASES = [
  {
    name: "taglish_code",
    voice: "Kore",
    text:
      "Pwede mo bang i-refactor yung authentication module? Yung login flow kasi may bug pa rin " +
      "sa session handling. Tapos i-add mo na rin yung unit tests para sa mga edge cases.",
  },
  {
    name: "taglish_casual",
    voice: "Puck",
    text:
      "Grabe ang traffic kanina sa EDSA, muntik na akong ma-late sa meeting. Buti na lang " +
      "nag-work from home yung team namin ngayon.",
  },
  {
    name: "tagalog_pure",
    voice: "Kore",
    text:
      "Maganda ang panahon ngayong umaga kaya naisipan kong maglakad papunta sa palengke " +
      "upang bumili ng gulay at isda.",
  },
];

const ALL_MODELS = [
  "openai/whisper-large-v3-turbo",
  "openai/gpt-4o-mini-transcribe",
  "openai/gpt-transcribe",
  "mistralai/voxtral-mini-transcribe",
  "openai/whisper-large-v3",
  "openai/gpt-4o-transcribe",
  "openai/whisper-1",
  "google/chirp-3",
  "qwen/qwen3-asr-flash-2026-02-10",
  "fish-audio/transcribe-1",
  "x-ai/grok-stt-1.0",
  "microsoft/mai-transcribe-1.5",
  "nvidia/parakeet-tdt-0.6b-v3",
  "deepgram/nova-3",
];
const TOP_MODELS = ALL_MODELS.slice(0, 5);
const MODELS = TOP_ONLY ? TOP_MODELS : ALL_MODELS;

// --- fixtures --------------------------------------------------------------

/** Gemini TTS returns headerless PCM, so we wrap it ourselves. 24 kHz mono s16. */
function wrapPcmAsWav(pcm, rate = 24000) {
  const header = Buffer.alloc(44);
  header.write("RIFF", 0);
  header.writeUInt32LE(36 + pcm.length, 4);
  header.write("WAVE", 8);
  header.write("fmt ", 12);
  header.writeUInt32LE(16, 16);
  header.writeUInt16LE(1, 20); // PCM
  header.writeUInt16LE(1, 22); // mono
  header.writeUInt32LE(rate, 24);
  header.writeUInt32LE(rate * 2, 28); // byte rate
  header.writeUInt16LE(2, 32); // block align
  header.writeUInt16LE(16, 34); // bits
  header.write("data", 36);
  header.writeUInt32LE(pcm.length, 40);
  return Buffer.concat([header, pcm]);
}

async function ensureFixtures() {
  mkdirSync(FIXTURES, { recursive: true });
  for (const c of CASES) {
    const wav = join(FIXTURES, `${c.name}.wav`);
    if (existsSync(wav)) continue;
    process.stdout.write(`generating fixture ${c.name}… `);
    const res = await fetch("https://openrouter.ai/api/v1/audio/speech", {
      method: "POST",
      headers: { Authorization: `Bearer ${KEY}`, "Content-Type": "application/json" },
      body: JSON.stringify({
        model: "google/gemini-3.1-flash-tts-preview",
        input: c.text,
        voice: c.voice,
        response_format: "pcm",
      }),
    });
    if (!res.ok) {
      console.log(`FAILED: ${res.status} ${await res.text()}`);
      continue;
    }
    const pcm = Buffer.from(await res.arrayBuffer());
    writeFileSync(wav, wrapPcmAsWav(pcm));
    writeFileSync(join(FIXTURES, `${c.name}.txt`), c.text);
    console.log(`ok (${(pcm.length / 2 / 24000).toFixed(1)}s)`);
  }
}

// --- scoring ---------------------------------------------------------------

const normalize = (s) =>
  s
    .toLowerCase()
    .replace(/[^\p{L}\p{N}\s]/gu, "")
    .split(/\s+/)
    .filter(Boolean);

/**
 * Tagalog function words with no English homograph. Used to catch a model that
 * translates instead of transcribing — measured behaviour when `language=en` is
 * pinned, and the reason the app never sends that hint.
 */
const TAGALOG_MARKERS = new Set([
  "ang", "ng", "mga", "yung", "iyong", "kasi", "tapos", "pwede", "puwede", "hindi", "naman",
  "ako", "akong", "ikaw", "siya", "niya", "nila", "natin", "namin", "ninyo", "kong", "mong",
  "yun", "iyon", "ito", "dito", "diyan", "ganito", "talaga", "grabe", "muntik", "buti",
  "kaya", "bang",
]);

const tagalogMarkers = (s) => normalize(s).filter((w) => TAGALOG_MARKERS.has(w)).length;

/** Levenshtein over words, divided by reference length. */
function wer(ref, hyp) {
  const r = normalize(ref);
  const h = normalize(hyp);
  const d = Array.from({ length: r.length + 1 }, (_, i) =>
    Array.from({ length: h.length + 1 }, (_, j) => (i === 0 ? j : j === 0 ? i : 0)),
  );
  for (let i = 1; i <= r.length; i++) {
    for (let j = 1; j <= h.length; j++) {
      d[i][j] = Math.min(
        d[i - 1][j] + 1,
        d[i][j - 1] + 1,
        d[i - 1][j - 1] + (r[i - 1] === h[j - 1] ? 0 : 1),
      );
    }
  }
  return d[r.length][h.length] / Math.max(1, r.length);
}

async function transcribe(model, wavPath) {
  const data = readFileSync(wavPath).toString("base64");
  const t0 = performance.now();
  try {
    const res = await fetch("https://openrouter.ai/api/v1/audio/transcriptions", {
      method: "POST",
      headers: { Authorization: `Bearer ${KEY}`, "Content-Type": "application/json" },
      body: JSON.stringify({ model, input_audio: { data, format: "wav" } }),
    });
    const ms = Math.round(performance.now() - t0);
    const body = await res.json();
    if (!res.ok || typeof body.text !== "string") {
      return { error: body?.error?.message ?? `HTTP ${res.status}`, ms };
    }
    return { text: body.text.trim(), ms, cost: body.usage?.cost ?? 0 };
  } catch (e) {
    return { error: String(e), ms: Math.round(performance.now() - t0) };
  }
}

// --- run -------------------------------------------------------------------

await ensureFixtures();

const cases = readdirSync(FIXTURES)
  .filter((f) => f.endsWith(".wav"))
  .map((f) => {
    const name = f.replace(/\.wav$/, "");
    const txt = join(FIXTURES, `${name}.txt`);
    return existsSync(txt)
      ? { name, wav: join(FIXTURES, f), ref: readFileSync(txt, "utf8").trim() }
      : null;
  })
  .filter(Boolean);

if (!cases.length) {
  console.error("No fixtures with matching .txt ground truth found.");
  process.exit(1);
}

console.log(`\n${cases.length} fixtures × ${MODELS.length} models\n`);

const scores = new Map();
for (const model of MODELS) {
  const results = await Promise.all(cases.map((c) => transcribe(model, c.wav)));
  const wers = [];
  const lats = [];
  let cost = 0;
  let translated = 0;
  results.forEach((r, i) => {
    if (r.error) return;
    wers.push(wer(cases[i].ref, r.text));
    lats.push(r.ms);
    cost += r.cost ?? 0;
    // Reference had Tagalog, output has none: the model translated it.
    if (tagalogMarkers(cases[i].ref) >= 2 && tagalogMarkers(r.text) === 0) translated++;
  });
  const ok = wers.length === cases.length;
  scores.set(model, {
    wer: ok ? wers.reduce((a, b) => a + b, 0) / wers.length : null,
    lat: lats.length ? Math.round(lats.reduce((a, b) => a + b, 0) / lats.length) : null,
    cost,
    translated,
    sample: results[0]?.text ?? results[0]?.error ?? "",
  });
}

const rows = [...scores.entries()].sort(
  (a, b) => (a[1].wer ?? 9) - (b[1].wer ?? 9) || (a[1].lat ?? 9e9) - (b[1].lat ?? 9e9),
);

console.log("MODEL".padEnd(42) + "WER".padStart(8) + "AVG ms".padStart(9) + "  VERDICT");
console.log("-".repeat(78));
for (const [model, s] of rows) {
  const w = s.wer === null ? "  ERR" : `${(s.wer * 100).toFixed(1)}%`;
  let verdict =
    s.wer === null ? "failed" : s.wer < 0.06 ? "usable" : s.wer < 0.2 ? "marginal" : "unusable";
  if (s.translated) verdict = `TRANSLATED (${s.translated}/${cases.length}) — do not use`;
  console.log(model.padEnd(42) + w.padStart(8) + String(s.lat ?? "-").padStart(9) + "  " + verdict);
}

const best = rows.find(([, s]) => s.wer !== null && s.wer < 0.06 && !s.translated);
if (best) {
  console.log(`\nRecommended default: ${best[0]}`);
  console.log(`Sample: ${scores.get(best[0]).sample}`);
}

// Fail loudly if no model clears the usable bar — that is a real regression.
if (!best) {
  console.error("\nNo model reached usable accuracy. Investigate before shipping.");
  process.exit(1);
}
