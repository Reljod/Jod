/**
 * Model roster, ordered by measured Taglish suitability.
 *
 * `wer` is the mean word error rate over the three Taglish fixtures; `p50` is
 * the median round-trip on an 11.2s utterance over 7 interleaved trials. Both
 * come from `pnpm bench` — see `docs/RESEARCH.md` for method and caveats.
 *
 * These fixtures are TTS-generated, which is cleaner than real speech. Treat the
 * ranking as a starting point and confirm on your own voice with the Compare
 * panel before trusting a model.
 */
export interface ModelInfo {
  id: string;
  label: string;
  /** Median round-trip on an 11.2s utterance, in seconds. */
  p50?: number;
  /** Mean word error rate across the Taglish fixtures; lower is better. */
  wer?: number;
  note: string;
  /** Fails on Tagalog badly enough that it is offered only for comparison. */
  unsuitable?: boolean;
}

export const MODELS: ModelInfo[] = [
  {
    id: "openai/whisper-large-v3-turbo",
    label: "Whisper large-v3 turbo",
    p50: 0.76,
    wer: 0.0,
    note: "Perfect on the fixtures and the fastest of the accurate tier. Default.",
  },
  {
    id: "openai/gpt-4o-mini-transcribe",
    label: "GPT-4o mini transcribe",
    p50: 1.0,
    wer: 0.0,
    note: "Equally accurate, ~30% slower. Token-priced rather than per-second.",
  },
  {
    id: "openai/gpt-transcribe",
    label: "GPT transcribe",
    p50: 1.37,
    wer: 0.0,
    note: "Equally accurate, slower. $0.0045/min.",
  },
  {
    id: "openai/whisper-large-v3",
    label: "Whisper large-v3",
    p50: 1.42,
    wer: 0.0,
    note: "Same accuracy as turbo at roughly twice the latency.",
  },
  {
    id: "openai/whisper-1",
    label: "Whisper 1",
    p50: 1.4,
    wer: 0.0,
    note: "The original endpoint. Accurate, unremarkable latency.",
  },
  {
    id: "mistralai/voxtral-mini-transcribe",
    label: "Voxtral mini transcribe",
    p50: 1.31,
    wer: 0.034,
    note: "Cheapest of the usable tier at $0.003/min.",
  },
  {
    id: "google/chirp-3",
    label: "Google Chirp 3",
    p50: 5.5,
    wer: 0.048,
    note: "Accurate but far too slow for push-to-talk.",
  },
  {
    id: "openai/gpt-4o-transcribe",
    label: "GPT-4o transcribe",
    p50: 0.9,
    wer: 0.052,
    note: "Fast, but adds glottal-stop spellings ('yung) that inflate edit distance.",
  },
  {
    id: "qwen/qwen3-asr-flash-2026-02-10",
    label: "Qwen3 ASR Flash",
    p50: 0.89,
    wer: 0.126,
    note: "Fast, but Tagalog is outside its 11 supported languages.",
  },
  {
    id: "fish-audio/transcribe-1",
    label: "Fish Audio Transcribe 1",
    wer: 0.228,
    note: "Drops affix hyphens and merges words.",
    unsuitable: true,
  },
  {
    id: "x-ai/grok-stt-1.0",
    label: "Grok STT 1.0",
    wer: 0.268,
    note: "Inconsistent on conversational Taglish.",
    unsuitable: true,
  },
  {
    id: "microsoft/mai-transcribe-1.5",
    label: "MAI Transcribe 1.5",
    wer: 0.381,
    note: "Mangles Tagalog phonetics badly.",
    unsuitable: true,
  },
  {
    id: "nvidia/parakeet-tdt-0.6b-v3",
    label: "Parakeet TDT 0.6B v3",
    wer: 0.726,
    note: "25 European languages only — no Tagalog. Included to show the failure.",
    unsuitable: true,
  },
  {
    id: "deepgram/nova-3",
    label: "Deepgram Nova-3",
    wer: 0.903,
    note: "Drops Tagalog words entirely, keeping only the English. Do not use.",
    unsuitable: true,
  },
];

/** Models worth putting head-to-head by default in the Compare panel. */
export const COMPARE_DEFAULT = [
  "openai/whisper-large-v3-turbo",
  "openai/gpt-4o-mini-transcribe",
  "mistralai/voxtral-mini-transcribe",
  "openai/gpt-4o-transcribe",
];
