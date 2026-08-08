# Fast Taglish voice-to-text: what actually works

Research behind `jod-voice`. Everything here was measured on 2026-08-08 against
the live OpenRouter API, not taken from vendor claims. Reproduce with
`pnpm bench`.

---

## 1. The problem is code-switching, not Tagalog

Tagalog alone is a solved-ish problem — it is in Whisper's 99 languages and most
commercial ASR supports it. What breaks models is **Taglish**: switching between
Tagalog and English mid-sentence, and attaching Filipino verb affixes to English
stems (`i-refactor`, `na-deploy`, `mag-commit`).

Research on code-switched ASR identifies three failure modes, and all three
showed up in our measurements:

| Failure mode | What it looks like | Seen in |
| --- | --- | --- |
| **Language omission** | Outputs one language, silently drops the other | Deepgram Nova-3 |
| **Translation, not transcription** | Renders speech into a single language | Chat-model prompting without explicit prohibition |
| **Phonetic collapse** | Tagalog rendered as mangled pseudo-English | Parakeet, MAI Transcribe |

Language omission is the dangerous one, because output looks fluent and clean.
Nova-3 turned

> "Pwede mo bang i-refactor yung authentication module? Yung login flow kasi may
> bug pa rin sa session handling."

into

> "Refactor authentication module log in flow session handling unit tests edge cases."

Every Tagalog function word is gone. Nothing about that output signals it is
wrong — a WER-blind eye would call it a clean transcript.

**Consequence:** a Taglish transcriber cannot be chosen on English benchmarks,
and cannot be trusted on latency alone. It has to be measured on mixed speech.

---

## 2. Measured results

Three fixtures (coding-context Taglish, casual Taglish, pure Tagalog), each
transcribed by all 14 STT models on OpenRouter. WER is mean word error rate
against known ground truth; latency is the median of 7 interleaved trials on an
11.2s utterance.

| Model | WER | p50 latency | Verdict |
| --- | --- | --- | --- |
| **openai/whisper-large-v3-turbo** | **0.0%** | **0.76s** | **Default** |
| openai/gpt-4o-mini-transcribe | 0.0% | 1.00s | Excellent, slightly slower |
| openai/gpt-transcribe | 0.0% | 1.37s | Excellent, slower |
| openai/whisper-large-v3 | 0.0% | 1.42s | Same accuracy, ~2× latency of turbo |
| openai/whisper-1 | 0.0% | ~1.40s | Fine, no reason to prefer it |
| mistralai/voxtral-mini-transcribe | 3.4% | 1.31s | Cheapest usable: $0.003/min |
| google/chirp-3 | 4.8% | 5.50s | Accurate, far too slow |
| openai/gpt-4o-transcribe | 5.2% | 0.90s | Spells glottal stops (`'yung`) |
| qwen/qwen3-asr-flash | 12.6% | 0.89s | Tagalog outside its 11 languages |
| fish-audio/transcribe-1 | 22.8% | 0.87s | Drops affix hyphens, merges words |
| x-ai/grok-stt-1.0 | 26.8% | 1.28s | Inconsistent |
| microsoft/mai-transcribe-1.5 | 38.1% | 3.65s | Phonetic mangling |
| nvidia/parakeet-tdt-0.6b-v3 | 72.6% | 1.20s | **No Tagalog support** |
| deepgram/nova-3 | 90.3% | 1.77s | **Drops Tagalog entirely** |

### The trap worth naming

**Parakeet TDT 0.6B v3 is the model most people would reach for.** It is the
speed/cost darling of 2026 self-hosted ASR, it is marketed as multilingual, and
it is the cheapest transcription model on OpenRouter at $0.0015/min. Its
"multilingual" claim covers **25 European Union languages**. Tagalog is not one
of them. Measured WER: 72.6%.

Deepgram Nova-3 advertises Tagalog support on its own product page. Measured
WER: 90.3%, by way of deleting the Tagalog.

---

## 3. Why the winner wins

`whisper-large-v3-turbo` is the fastest model that transcribes Tagalog
correctly. Two reasons it holds up:

- **Whisper's training data includes Tagalog**, and its decoder was trained on
  multilingual audio without a forced single-language output, so mid-sentence
  switching does not fight the model.
- **Turbo cuts decoder layers, not the encoder.** The acoustic understanding
  that matters for Tagalog phonology is intact; what got cheaper is text
  generation. That is why it matches large-v3's accuracy at ~half the latency.

At 0.76s p50 for an 11.2s utterance the round trip is ~0.07× realtime — the
wait after you release the key is dominated by network, not inference.

---

## 4. Architecture: why push-to-talk beats streaming here

Streaming ASR wins when the user needs to see words appear as they speak
(captions, live meetings). Dictating an instruction to a coding agent is a
different shape:

- **The utterance is bounded.** You press, say one instruction, release.
- **Whole-sentence context improves accuracy**, and it matters most for
  code-switching — the model resolves `i-refactor` correctly partly from what
  follows it. Streaming commits to words before that context arrives.
- **Latency is already invisible.** Sub-second after release, on an utterance
  that took you 5–10 seconds to say.
- Streaming costs more and the transcription endpoint does not support it.

So: buffer locally, send once on release. If you later want live partials,
Silero VAD (<2MB, <1ms per 30ms frame) plus 400ms overlapping windows is the
standard route — but it buys nothing for this use case.

### The `prompt` field is a dead end

The single most important API detail: **OpenRouter's `/audio/transcriptions`
endpoint accepts a `prompt` field and ignores it on every provider.** The
documentation says so explicitly.

This matters because prompt-biasing is the normal way to teach Whisper your
jargon — feed it library names and identifiers and it stops mis-hearing them.
That lever does not exist here. Model choice is the only accuracy control on the
fast path.

The workaround, which `jod-voice` implements as an optional toggle, is a second
pass over the **text** through `/chat/completions`, which does accept a system
prompt. It costs ~400ms and a fraction of a cent, and it is the only place a
"never translate, never drop a language" instruction can actually land. It is
off by default because the top models do not need it; turn it on when dictating
dense technical identifiers.

---

## 5. Local models: why not

For Apple Silicon, the local options are real but do not fit this job:

- **Parakeet.cpp** is extremely fast (~27ms encoder for 10s audio on Metal) —
  and has no Tagalog.
- **whisper.cpp with Metal** runs large-v3 at roughly 2–10× realtime depending
  on chip, so a 10s utterance costs ~1–5s locally versus 0.76s over the network,
  plus ~1.5GB of model weights and a cold-start penalty.

Local wins on privacy and offline use. If either becomes a requirement,
whisper.cpp with `large-v3-turbo` quantized is the port — same model family, so
Taglish accuracy should carry over. Until then the API is faster.

---

## 6. Method and honest caveats

- Fixtures are **TTS-generated** (Gemini 3.1 Flash TTS) from known ground-truth
  Taglish sentences. This gives exact reference text, which real recordings
  cannot without hand-transcription.
- **TTS speech is cleaner than real speech.** No Filipino regional accent, no
  background noise, no disfluency, no overlapping speech. Real-world WER will be
  higher across the board. Published figures for Taglish on commercial systems
  land around 75–85% accuracy on casual mixed speech.
- What the fixtures *do* measure reliably is **relative ranking and failure
  mode**. A model that deletes Tagalog on clean synthetic audio will not recover
  it on noisy real audio.
- The app therefore ships a **Compare panel**: record your own voice once, run it
  through four models simultaneously, and read the transcripts side by side.
  That is the only test that reflects how *you* speak.

Re-run any time with:

```sh
pnpm bench          # all 14 models
pnpm bench:top      # just the usable tier
```

Drop your own `name.wav` + `name.txt` pair into `fixtures/` and the benchmark
picks it up automatically.

---

## Sources

- [OpenRouter Speech-to-Text documentation](https://openrouter.ai/docs/guides/overview/multimodal/stt)
- [OpenRouter audio APIs announcement](https://openrouter.ai/blog/announcements/announcing-audio-apis/)
- [OpenRouter speech-to-text model collection](https://openrouter.ai/collections/speech-to-text-models)
- [nvidia/parakeet-tdt-0.6b-v3 model card](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3) — language list
- [CS-FLEURS: massively multilingual code-switched speech dataset](https://arxiv.org/pdf/2509.14161)
- [Are you speaking my languages? Spoken language adherence in multimodal LLMs](https://arxiv.org/pdf/2606.17281) — code-switching failure modes
- [Silero VAD guide](https://aiadoptionagency.com/silero-vad-voice-activity-detection/)
- [Parakeet.cpp vs Whisper self-hosted comparison](https://modelslab.com/blog/audio-generation/parakeet-cpp-vs-whisper-self-hosted-asr-comparison-2026)
- [whisper.cpp Metal on Apple Silicon](https://fazm.ai/blog/whisper-cpp-metal-apple-silicon)
- [Tagalog/Filipino transcription state of the art, 2026](https://convertaudiototext.com/blog/tagalog-filipino-transcription)
