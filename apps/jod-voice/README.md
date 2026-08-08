# jod-voice

Push-to-talk dictation that understands Taglish, built for talking to a terminal
or a coding agent instead of typing at it.

Hold a key, speak Tagalog-English however it comes out, release. The transcript
lands in under a second — copy it, or send it straight into a running Jod
session with its context intact.

> Model choice here is measured, not assumed. The default is the fastest model
> that transcribes Taglish correctly; several popular fast models silently
> delete Tagalog. See [`docs/RESEARCH.md`](docs/RESEARCH.md).

---

## Running it

```sh
pnpm install
pnpm app          # launches with OPENROUTER_API_KEY injected from Doppler
```

`pnpm app` shells through `scripts/with-key.sh`, which resolves the key from the
`jod-apps` Doppler project. The key is never written to this repo.

macOS will ask for **microphone** permission on first record, and for
**accessibility/input monitoring** the first time the global hotkey fires.

### If Doppler is not available

Export the key yourself and run Tauri directly:

```sh
export OPENROUTER_API_KEY=...
pnpm tauri dev
```

---

## Using it

| Action | How |
| --- | --- |
| Dictate | Hold **⌥Space**, or hold the on-screen button |
| Cancel an utterance | **Esc** while recording |
| Keep dictating | Each utterance appends, so you can build a long instruction |
| Send to a Claude session | Pick a session, **Send to session** |
| Check a model on your own voice | Record once, then **Compare models** |

The session dropdown lists live `claude --bg` sessions via the `orchestrate`
skill's `orc` CLI. Sending continues that session rather than starting a new
one, so your dictated instruction arrives mid-conversation.

---

## Configuration

**Model** — defaults to `openai/whisper-large-v3-turbo` (0% WER on the Taglish
fixtures, 0.76s p50). The dropdown shows measured WER and latency for each
option; entries marked ⚠ fail on Tagalog and are listed only so you can see the
failure yourself.

**Language hint** — leave on auto. Your words come back exactly as spoken; the
app never asks the model to render them in a different language.

Pinning English is deliberately not offered. Measured: `language=en` makes
Whisper **translate** Taglish into English rather than transcribe it — "Pwede mo
bang i-refactor yung authentication module?" comes back as "Can you refactor
your authentication module?", with the response still claiming
`task: transcribe`. For dictation that is the worst kind of bug, because the
output looks perfect and says something you did not say.

What the app does do about stray detections, without touching your words:

1. **Refuses to send non-speech at all** — duration, peak, and a gain-invariant
   peak-to-RMS check that separates speech from fans and hiss. This is what
   fixed the Korean transcripts; they came from near-empty recordings.
2. **Rejects any non-Latin script** in the result, since Tagalog and English
   cannot produce one. No retry, no rewriting — it just refuses.
3. **Discards the repair pass** if your Tagalog went in and did not come out.

The detected language is shown next to the latency, so a bad detection is
visible rather than silent.

**Taglish repair pass** — off by default. Sends the *text* (not the audio)
through a cheap LLM with an explicit "never translate, never drop a language"
instruction, fixing technical identifiers and punctuation. Costs ~400ms. Worth
enabling when dictating dense library and API names.

Override the Jod checkout used for session lookup with `JOD_REPO=/path/to/Jod`.

---

## Benchmarking

```sh
pnpm bench          # all 14 OpenRouter STT models
pnpm bench:top      # only the usable tier
```

Generates Taglish fixtures on first run (cached in `fixtures/`, gitignored),
then reports WER and latency per model. It exits non-zero if no model reaches
usable accuracy, so it works as a regression check.

To benchmark on your own speech, drop a matching `name.wav` and `name.txt`
(ground truth) into `fixtures/`.

---

## Layout

```
src/                     frontend — push-to-talk UI, model roster, compare panel
  models.ts              measured WER/latency per model
src-tauri/src/
  audio.rs               cpal capture → mono → 16kHz → WAV
  guard.rs               speech gate + script and translation guards
  openrouter.rs          transcription + optional Taglish repair pass
  sessions.rs            `orc` bridge to running Claude sessions
scripts/
  with-key.sh            Doppler key injection
  bench.mjs              reproducible model benchmark
docs/RESEARCH.md         why these models, why this architecture
```

---

## Tests

```sh
cd src-tauri && cargo test    # 32 tests: audio, speech gate, translation guard, sessions
pnpm build                    # strict TypeScript + production bundle
```

---

## Status

Prototype. Known gaps:

- **Not measured on real Filipino-accented speech.** Fixtures are TTS-generated,
  which is cleaner than real audio. Rankings should hold; absolute WER will be
  worse. Use the Compare panel on your own voice before trusting a model.
- **No auto-paste into the frontmost app.** Copy, or send to a Jod session. Real
  paste needs macOS accessibility permissions and a keystroke-synthesis path.
- **No local/offline model.** Requires network. See `docs/RESEARCH.md` §5 for
  the whisper.cpp port if offline becomes a requirement.
- **Hotkey is fixed** at ⌥Space; not yet configurable in the UI.
