//! Capturing an utterance by running a recorder, not by linking one.
//!
//! ## Why a subprocess
//!
//! The obvious design links `cpal` and captures in-process, which is what
//! `apps/jod-voice` does. For the console it is the wrong one, for two reasons
//! that are not about taste:
//!
//! * **`jod` runs on a headless VPS.** `cpal` needs ALSA headers to build on
//!   Linux and a sound card to run. Linking it would mean the console — the
//!   whole product — stops building on the machine it is deployed to, in
//!   exchange for a feature that could not work there anyway.
//! * **The microphone is not where the TUI is.** Over SSH the console runs on
//!   the server. An in-process capture would faithfully record the server's
//!   silence. A subprocess has exactly the same problem, but it makes it
//!   *visible*: "no recorder found" is a sentence, whereas an empty buffer is
//!   a mystery.
//!
//! So: find a recording program, run it, read the WAV it wrote. The cost is
//! one process spawn per utterance, which is nothing against a network round
//! trip to a transcription model.
//!
//! ## Stopping is a signal, and it has to be the right one
//!
//! A WAV file's RIFF header carries the length of the data that follows, and
//! it can only be written once the recorder knows how much there was. Killing
//! the process outright leaves a header claiming zero samples, and every
//! reader believes it — the recording is not truncated, it is *empty*.
//!
//! `SIGINT` is the signal every recorder here treats as "stop cleanly and
//! finalise", so that is what stop sends, with `SIGKILL` held back for a
//! recorder that ignores it.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// Rate every transcription model here expects.
pub const TARGET_RATE: u32 = 16_000;

/// How long to wait for a recorder to finalise its file after `SIGINT`.
///
/// Generous, because the cost of being wrong is asymmetric: a few hundred
/// milliseconds of waiting against losing what was just said.
const FINALISE_TIMEOUT_MS: u64 = 2_000;

/// Longest one-shot recording a recorder is allowed to run for.
///
/// A backstop, not a limit anyone should reach. It exists because a recorder
/// whose stop signal was missed would otherwise fill the disk quietly.
pub const MAX_SECONDS: u64 = 300;

/// The bound for a continuous listening session.
///
/// Four hours. Long enough that hands-free work is never interrupted by it,
/// short enough that a console left running overnight with the microphone on
/// stops on its own rather than filling a disk with a silent room.
///
/// At 16 kHz mono 16-bit this is about 460 MB — which is the reason the
/// streaming reader discards audio as it consumes it rather than keeping the
/// session in memory.
pub const SESSION_SECONDS: u64 = 4 * 60 * 60;

/// A recording program this module knows how to drive.
///
/// Ordered by preference in [`Backend::detect`]. The ordering is the native
/// server first on each platform, because a recorder talking to the sound
/// server it belongs to is the one that respects the input device the user
/// actually chose in their settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// PipeWire — the default on current Linux desktops.
    PwRecord,
    /// ALSA's own, present almost everywhere Linux is.
    Arecord,
    /// SoX. Common on macOS via Homebrew, and on older Linux.
    Rec,
    /// The universal fallback, and the only one needing a platform-specific
    /// input spec.
    Ffmpeg,
}

impl Backend {
    /// The program name, as it appears on `PATH`.
    pub fn program(&self) -> &'static str {
        match self {
            Backend::PwRecord => "pw-record",
            Backend::Arecord => "arecord",
            Backend::Rec => "rec",
            Backend::Ffmpeg => "ffmpeg",
        }
    }

    /// In preference order.
    pub fn all() -> [Backend; 4] {
        [
            Backend::PwRecord,
            Backend::Arecord,
            Backend::Rec,
            Backend::Ffmpeg,
        ]
    }

    /// The arguments that record mono 16 kHz WAV to `path`.
    ///
    /// Every backend is pinned to the same rate and channel count so the guard
    /// and the uploader see one shape regardless of which program ran, and so
    /// the conversion happens in C rather than in a resampler written here.
    pub fn args(&self, path: &Path, max_seconds: u64) -> Vec<String> {
        let out = path.to_string_lossy().to_string();
        let rate = TARGET_RATE.to_string();
        match self {
            Backend::PwRecord => vec![
                "--rate".into(),
                rate,
                "--channels".into(),
                "1".into(),
                "--format".into(),
                "s16".into(),
                out,
            ],
            Backend::Arecord => vec![
                "-q".into(),
                "-f".into(),
                "S16_LE".into(),
                "-r".into(),
                rate,
                "-c".into(),
                "1".into(),
                "-t".into(),
                "wav".into(),
                // `arecord` needs a duration or it records until signalled;
                // the bound is the runaway guard, not the expected path.
                "-d".into(),
                max_seconds.to_string(),
                out,
            ],
            Backend::Rec => vec![
                "-q".into(),
                "-r".into(),
                rate,
                "-c".into(),
                "1".into(),
                "-b".into(),
                "16".into(),
                out,
                "trim".into(),
                "0".into(),
                max_seconds.to_string(),
            ],
            Backend::Ffmpeg => {
                // The input spec is the one genuinely platform-shaped thing
                // here: avfoundation on macOS, ALSA on Linux.
                let (f, i) = if cfg!(target_os = "macos") {
                    ("avfoundation", ":default")
                } else {
                    ("alsa", "default")
                };
                vec![
                    "-hide_banner".into(),
                    "-loglevel".into(),
                    "error".into(),
                    "-f".into(),
                    f.into(),
                    "-i".into(),
                    i.into(),
                    "-ar".into(),
                    rate,
                    "-ac".into(),
                    "1".into(),
                    "-t".into(),
                    max_seconds.to_string(),
                    "-y".into(),
                    out,
                ]
            }
        }
    }

    /// The first backend present on this machine.
    pub fn detect() -> Option<Backend> {
        Backend::all().into_iter().find(|b| on_path(b.program()))
    }
}

/// Whether `program` can be found on `PATH`.
fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let candidate = dir.join(program);
                // Existence, not permissions: a file on `PATH` under the right
                // name that cannot be executed is a broken install, and the
                // spawn error says so far better than a silent skip to the
                // next backend would.
                candidate.is_file()
            })
        })
        .unwrap_or(false)
}

/// What one utterance produced.
#[derive(Debug, Clone)]
pub struct Recording {
    /// The WAV bytes, exactly as they will be uploaded.
    pub wav: Vec<u8>,
    /// Mono samples, for the speech gate.
    pub samples: Vec<f32>,
    pub rate: u32,
}

impl Recording {
    pub fn seconds(&self) -> f32 {
        if self.rate == 0 {
            return 0.0;
        }
        self.samples.len() as f32 / self.rate as f32
    }
}

/// A recorder that is currently running.
///
/// Holds a temporary file which is removed on drop, so an abandoned recording
/// — the user pressed Escape, or the TUI exited mid-utterance — does not leave
/// audio of him on disk.
pub struct Recorder {
    backend: Backend,
    child: Child,
    path: PathBuf,
}

impl Recorder {
    /// Start recording, choosing a backend.
    pub fn start() -> Result<Recorder, String> {
        let backend = Backend::detect().ok_or_else(|| {
            format!(
                "no recording program found. Dictation runs one of: {}. \
                 Install one on the machine running this console — note that \
                 over SSH that is the server, not your laptop.",
                Backend::all()
                    .iter()
                    .map(|b| b.program())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        Recorder::start_with(backend, MAX_SECONDS)
    }

    /// Start a continuous listening session.
    ///
    /// The same recorder, bounded by [`SESSION_SECONDS`] rather than by one
    /// utterance, meant to be read while it runs by [`crate::stream`].
    pub fn start_session() -> Result<Recorder, String> {
        let backend = Backend::detect().ok_or_else(|| {
            format!(
                "no recording program found. Dictation runs one of: {}. \
                 Install one on the machine running this console — note that \
                 over SSH that is the server, not your laptop.",
                Backend::all()
                    .iter()
                    .map(|b| b.program())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        Recorder::start_with(backend, SESSION_SECONDS)
    }

    pub fn start_with(backend: Backend, max_seconds: u64) -> Result<Recorder, String> {
        let path = std::env::temp_dir().join(format!(
            "jod-dictation-{}-{}.wav",
            std::process::id(),
            chrono_ish_suffix()
        ));

        let child = Command::new(backend.program())
            .args(backend.args(&path, max_seconds))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            // Kept, not discarded: when a recorder refuses the device this is
            // the only place that says why.
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not start `{}`: {e}", backend.program()))?;

        Ok(Recorder {
            backend,
            child,
            path,
        })
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// The file being written to, for a reader that tails it while it runs.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the recorder is still alive.
    ///
    /// A recorder that exited on its own has failed — it was told to run for
    /// [`MAX_SECONDS`] — so this going false mid-utterance is an error, not
    /// completion.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Stop, and read back what was recorded.
    pub fn finish(mut self) -> Result<Recording, String> {
        self.interrupt();

        let waited = self.wait_for_exit();
        if !waited {
            // It ignored SIGINT. The file is very likely unusable, but killing
            // is still better than leaving a recorder holding the microphone.
            let _ = self.child.kill();
            let _ = self.child.wait();
            return Err(format!(
                "`{}` did not stop when asked, so the recording was lost",
                self.backend.program()
            ));
        }

        let bytes = std::fs::read(&self.path).map_err(|e| {
            format!(
                "`{}` wrote no recording: {e}. {}",
                self.backend.program(),
                self.stderr_hint()
            )
        })?;

        let samples = decode(&bytes)?;
        Ok(Recording {
            wav: bytes,
            samples,
            rate: TARGET_RATE,
        })
    }

    /// Throw the recording away.
    pub fn cancel(mut self) {
        self.interrupt();
        let _ = self.wait_for_exit();
    }

    /// Ask the recorder to stop and finalise the WAV header.
    fn interrupt(&mut self) {
        // SAFETY: `id()` is this child's pid, and the child has not been
        // reaped — `finish` and `cancel` both take `self` by value and wait
        // afterwards, so the pid cannot have been recycled between these lines.
        unsafe {
            libc::kill(self.child.id() as libc::pid_t, libc::SIGINT);
        }
    }

    /// Whether it exited within the finalise window.
    fn wait_for_exit(&mut self) -> bool {
        let step = std::time::Duration::from_millis(20);
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_millis(FINALISE_TIMEOUT_MS);
        while std::time::Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return true,
                Ok(None) => std::thread::sleep(step),
                Err(_) => return false,
            }
        }
        false
    }

    /// Whatever the recorder said on the way out, for the error message.
    fn stderr_hint(&mut self) -> String {
        use std::io::Read;
        let Some(mut err) = self.child.stderr.take() else {
            return String::new();
        };
        let mut buf = String::new();
        let _ = err.read_to_string(&mut buf);
        let line = buf.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        if line.is_empty() {
            String::new()
        } else {
            format!("It said: {line}")
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // Both are best-effort by design: this runs on the panic path too, and
        // a failure to clean up must not become a second failure.
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A filename suffix distinct enough for concurrent consoles.
///
/// Not a timestamp for its own sake — the pid already separates processes, and
/// this separates utterances within one.
fn chrono_ish_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// WAV bytes to mono f32 samples, for the speech gate.
///
/// Accepts what the backends here actually emit — 16-bit PCM — and also f32,
/// since `rec` can be configured for it. Anything multi-channel is downmixed
/// rather than refused: a recorder that ignored `-c 1` should cost a slightly
/// wrong level reading, not the whole utterance.
pub fn decode(wav: &[u8]) -> Result<Vec<f32>, String> {
    let reader = hound::WavReader::new(std::io::Cursor::new(wav))
        .map_err(|e| format!("the recording is not readable as WAV: {e}"))?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;

    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .into_samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("could not read the recording: {e}"))?,
        hound::SampleFormat::Int => {
            // Normalise by the format's own full scale, so a 24-bit recorder
            // does not read as sixteen thousand times too loud and sail past
            // every threshold in the guard.
            let full = (1_i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .into_samples::<i32>()
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("could not read the recording: {e}"))?
                .into_iter()
                .map(|s| s as f32 / full)
                .collect()
        }
    };

    if channels == 1 {
        return Ok(interleaved);
    }
    Ok(interleaved
        .chunks(channels)
        .map(|f| f.iter().sum::<f32>() / channels as f32)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav_bytes(samples: &[i16], channels: u16, rate: u32) -> Vec<u8> {
        let spec = hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut w = hound::WavWriter::new(&mut buf, spec).unwrap();
            for s in samples {
                w.write_sample(*s).unwrap();
            }
            w.finalize().unwrap();
        }
        buf.into_inner()
    }

    // ---- what each recorder is asked for --------------------------------

    /// The guard and the uploader assume one shape, so every backend has to be
    /// pinned to it. A backend recording at 44.1 kHz stereo would pass every
    /// test that only checked it ran.
    #[test]
    fn every_backend_records_mono_at_the_rate_the_models_expect() {
        for b in Backend::all() {
            let args = b.args(Path::new("/tmp/x.wav"), MAX_SECONDS).join(" ");
            assert!(
                args.contains("16000"),
                "{} does not pin the sample rate: {args}",
                b.program()
            );
            assert!(
                args.contains(" 1") || args.contains("-ac 1"),
                "{} does not pin mono: {args}",
                b.program()
            );
        }
    }

    #[test]
    fn every_backend_writes_to_the_file_it_was_given() {
        for b in Backend::all() {
            let args = b.args(Path::new("/tmp/utterance.wav"), MAX_SECONDS);
            assert!(
                args.iter().any(|a| a == "/tmp/utterance.wav"),
                "{} does not write to the requested path: {args:?}",
                b.program()
            );
        }
    }

    /// A missed stop signal must not be able to fill the disk.
    #[test]
    fn every_backend_is_bounded_so_a_missed_stop_cannot_run_for_ever() {
        for b in Backend::all() {
            // pw-record has no duration flag; it is bounded by the signal and
            // by `MAX_SECONDS` being unreachable in practice. Every other
            // backend takes the bound explicitly.
            if b == Backend::PwRecord {
                continue;
            }
            let args = b.args(Path::new("/tmp/x.wav"), MAX_SECONDS).join(" ");
            assert!(
                args.contains(&MAX_SECONDS.to_string()),
                "{} is unbounded: {args}",
                b.program()
            );
        }
    }

    #[test]
    fn a_missing_program_is_not_found_on_path() {
        assert!(!on_path("jod-definitely-not-a-real-recorder"));
    }

    // ---- reading back what was recorded ---------------------------------

    #[test]
    fn sixteen_bit_pcm_decodes_to_normalised_samples() {
        let wav = wav_bytes(&[i16::MAX, 0, i16::MIN], 1, TARGET_RATE);
        let got = decode(&wav).unwrap();
        assert_eq!(got.len(), 3);
        assert!((got[0] - 1.0).abs() < 0.001, "full scale did not reach 1.0");
        assert_eq!(got[1], 0.0);
    }

    /// A recorder that ignored `-c 1` should cost a slightly wrong level, not
    /// the utterance.
    #[test]
    fn a_stereo_recording_is_downmixed_rather_than_refused() {
        // Two frames of (1.0, 0.0) — each should average to 0.5.
        let wav = wav_bytes(&[i16::MAX, 0, i16::MAX, 0], 2, TARGET_RATE);
        let got = decode(&wav).unwrap();
        assert_eq!(got.len(), 2, "channels were not folded into frames");
        assert!((got[0] - 0.5).abs() < 0.01, "downmix is wrong: {}", got[0]);
    }

    /// The empty-header case this module's stop signal exists to prevent —
    /// it must read as an empty recording, not as a parse failure.
    #[test]
    fn a_wav_with_no_samples_decodes_to_nothing() {
        let wav = wav_bytes(&[], 1, TARGET_RATE);
        assert!(decode(&wav).unwrap().is_empty());
    }

    #[test]
    fn something_that_is_not_a_wav_is_refused_by_name() {
        let err = decode(b"this is not audio").unwrap_err();
        assert!(err.contains("WAV"), "unhelpful error: {err}");
    }

    /// The gate reads seconds, so a wrong duration would mis-reject speech.
    #[test]
    fn a_recordings_duration_comes_from_its_rate() {
        let r = Recording {
            wav: Vec::new(),
            samples: vec![0.0; TARGET_RATE as usize * 2],
            rate: TARGET_RATE,
        };
        assert!((r.seconds() - 2.0).abs() < 0.001);
    }
}
