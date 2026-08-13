//! Transcribing on this machine, with whisper.cpp.
//!
//! The dictation path that does not leave the laptop: no key, no network, no
//! per-utterance cost, and nothing said at a desk arriving at somebody's API.
//!
//! ## Why a subprocess again
//!
//! Same reasoning as [`crate::record`], and one more. `whisper-rs` would link
//! whisper.cpp into `jod` itself, which means every build of the console — on
//! every worktree, for every agent in a parallel fleet — compiles a C++ speech
//! engine it will almost never call. `Cargo.toml` at the workspace root already
//! measures how much that kind of cost multiplies here.
//!
//! Running `whisper-cli` costs one process spawn against a transcription that
//! takes hundreds of milliseconds, and it lets a model be swapped without
//! rebuilding anything.
//!
//! ## The two flags this file exists to get right
//!
//! Both were read off `whisper-cli -h` and confirmed against real speech, not
//! taken from documentation:
//!
//! * **`-l` defaults to `en`.** Not `auto` — `en`. Left alone, whisper.cpp
//!   decodes Taglish as English and the Tagalog half comes out mangled or
//!   silently dropped. This is the same failure the cloud research documented
//!   for model choice, arriving through a different door, and it is why
//!   [`Whisper::argv`] always passes a language explicitly.
//! * **`-tr` translates.** It is never passed, and there is a test asserting
//!   so. Translation is the one outcome worse than a bad transcript, because
//!   it reads as success.
//!
//! ## English-only models are not offered
//!
//! Every `.en` model — `tiny.en`, `base.en`, `small.en` — is excluded from
//! [`CATALOG`] on purpose. They are smaller and faster and they cannot
//! represent Tagalog at all, so offering one would be offering the bug.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Where the GGML weights live.
const HOST: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

/// The binary names whisper.cpp has shipped under.
///
/// `main` is the pre-2024 name and is still what a long-installed build is
/// called; `whisper-cli` is current. Both are checked because the failure of
/// not checking is "install whisper.cpp" advice given to somebody who already
/// has it.
const PROGRAMS: [&str; 3] = ["whisper-cli", "whisper-cpp", "main"];

/// One model that can be downloaded and used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Model {
    /// What you type: `small`, `large-v3-turbo-q5_0`.
    pub name: &'static str,
    /// The file on the model host, which is also its name on disk.
    pub file: &'static str,
    pub mb: u32,
    pub note: &'static str,
}

/// The models worth offering, smallest first.
///
/// **Multilingual only.** See the module note: the `.en` builds are excluded
/// because Taglish is the input this exists for.
///
/// Quantized variants are preferred at every size — a `q5_1` is roughly 40% of
/// the weight for a difference that does not show up in dictation, and the
/// difference that *does* show up is the one between a model that fits in
/// memory and one that swaps.
pub const CATALOG: [Model; 6] = [
    Model {
        name: "tiny",
        file: "ggml-tiny-q5_1.bin",
        mb: 32,
        note: "fastest, roughest. Fine for a quick note, weak on technical words",
    },
    Model {
        name: "base",
        file: "ggml-base-q5_1.bin",
        mb: 60,
        note: "still small, noticeably steadier than tiny",
    },
    Model {
        name: "small",
        file: "ggml-small-q5_1.bin",
        mb: 190,
        note: "the light option — good Taglish, runs on anything",
    },
    Model {
        name: "medium",
        file: "ggml-medium-q5_0.bin",
        mb: 539,
        note: "better again, and slower than large-v3-turbo for no gain",
    },
    Model {
        name: "large-v3-turbo",
        file: "ggml-large-v3-turbo-q5_0.bin",
        mb: 574,
        note: "recommended — the family measured at 0% WER on the Taglish fixtures",
    },
    Model {
        name: "large-v3-turbo-full",
        file: "ggml-large-v3-turbo.bin",
        mb: 1620,
        note: "unquantized. Only if the quantized one is demonstrably wrong for you",
    },
];

/// What a fresh install should download.
///
/// The turbo family rather than the biggest thing that fits: it is the one the
/// cloud research already measured getting Taglish right, so choosing it here
/// keeps the local and remote paths honest about being the same model.
/// → `apps/jod-voice/docs/RESEARCH.md`
pub const RECOMMENDED: &str = "large-v3-turbo";

/// Look a model up by the name you would type.
pub fn model(name: &str) -> Option<Model> {
    let wanted = name.trim().to_lowercase();
    CATALOG
        .iter()
        .copied()
        .find(|m| m.name == wanted || m.file == wanted)
}

/// Where downloaded weights are kept.
///
/// Under Jod's own home rather than beside the checkout: a 574 MB binary is
/// not source, and a worktree per agent would otherwise mean a copy per agent.
pub fn models_dir(jod_home: &Path) -> PathBuf {
    jod_home.join("models")
}

impl Model {
    pub fn path(&self, jod_home: &Path) -> PathBuf {
        models_dir(jod_home).join(self.file)
    }

    pub fn is_installed(&self, jod_home: &Path) -> bool {
        self.path(jod_home).is_file()
    }

    pub fn url(&self) -> String {
        format!("{HOST}/{}", self.file)
    }
}

/// The models actually present on this machine.
pub fn installed(jod_home: &Path) -> Vec<Model> {
    CATALOG
        .iter()
        .copied()
        .filter(|m| m.is_installed(jod_home))
        .collect()
}

/// Download a model, reporting progress.
///
/// Written to a `.part` file and renamed only on success, because the failure
/// this prevents is the expensive one: a half-downloaded 574 MB file that is
/// the right *name* is indistinguishable from a working model until
/// whisper.cpp refuses to load it, by which point the download is long
/// forgotten.
pub async fn download(
    m: Model,
    jod_home: &Path,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<PathBuf, String> {
    use tokio::io::AsyncWriteExt;

    let dir = models_dir(jod_home);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("could not make {}: {e}", dir.display()))?;

    let final_path = m.path(jod_home);
    let part = final_path.with_extension("part");

    let res = reqwest::Client::new()
        .get(m.url())
        .send()
        .await
        .map_err(|e| format!("could not reach the model host: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("the model host answered {}", res.status()));
    }
    let total = res.content_length();

    let mut file = tokio::fs::File::create(&part)
        .await
        .map_err(|e| format!("could not write {}: {e}", part.display()))?;
    let mut got = 0u64;
    let mut stream = res;
    loop {
        let chunk = stream
            .chunk()
            .await
            .map_err(|e| format!("the download broke off: {e}"))?;
        let Some(chunk) = chunk else { break };
        got += chunk.len() as u64;
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("could not write {}: {e}", part.display()))?;
        progress(got, total);
    }
    file.flush()
        .await
        .map_err(|e| format!("could not finish {}: {e}", part.display()))?;
    drop(file);

    // A server that closed early leaves a plausible-looking file, so the size
    // is checked before the rename makes it look complete.
    if let Some(total) = total {
        if got != total {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(format!(
                "the download stopped at {got} of {total} bytes — nothing was installed"
            ));
        }
    }
    tokio::fs::rename(&part, &final_path)
        .await
        .map_err(|e| format!("could not install the model: {e}"))?;
    Ok(final_path)
}

/// A whisper.cpp binary on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Whisper {
    pub program: PathBuf,
}

impl Whisper {
    /// Find whisper.cpp, on `PATH` or at `WHISPER_CLI`.
    ///
    /// The environment variable exists because building whisper.cpp from
    /// source is the common way to get it and that leaves the binary in
    /// `build/bin`, not on `PATH`.
    pub fn detect() -> Option<Whisper> {
        if let Some(explicit) = std::env::var_os("WHISPER_CLI") {
            let p = PathBuf::from(explicit);
            if p.is_file() {
                return Some(Whisper { program: p });
            }
        }
        let paths = std::env::var_os("PATH")?;
        for dir in std::env::split_paths(&paths) {
            for name in PROGRAMS {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(Whisper {
                        program: candidate,
                    });
                }
            }
        }
        None
    }

    /// How to invoke whisper.cpp for one utterance.
    ///
    /// Read off `whisper-cli -h` and confirmed against real speech. Every flag
    /// here is load-bearing:
    ///
    /// * `-l` — **never omitted.** Its default is `en`, which decodes Taglish
    ///   as English. See the module note.
    /// * `-nt` — no timestamps, so stdout is the sentence and nothing else.
    /// * `-np` — no prints, so progress and system info stay off stdout.
    /// * `-t` — threads. Left to whisper's own default when `threads` is None.
    ///
    /// `-tr` is conspicuously absent and must stay that way.
    pub fn argv(&self, model: &Path, wav: &Path, language: &str, threads: Option<usize>) -> Vec<String> {
        let mut args = vec![
            "-m".into(),
            model.to_string_lossy().to_string(),
            "-f".into(),
            wav.to_string_lossy().to_string(),
            "-l".into(),
            language.to_string(),
            "-nt".into(),
            "-np".into(),
        ];
        if let Some(t) = threads {
            args.push("-t".into());
            args.push(t.to_string());
        }
        args
    }

    /// Transcribe WAV bytes.
    ///
    /// whisper.cpp reads a file, and [`crate::record::Recorder`] deletes its
    /// own the moment the recording is handed over — that deletion is the
    /// guarantee that abandoned audio of somebody does not accumulate in
    /// `/tmp`, and it is worth more than the write this costs. A ten-second
    /// utterance is around 320 KB.
    ///
    /// The file written here is removed on every path out, including the
    /// error one.
    pub fn transcribe_wav(
        &self,
        model: &Path,
        wav: &[u8],
        language: &str,
        threads: Option<usize>,
    ) -> Result<String, String> {
        let path = std::env::temp_dir().join(format!(
            "jod-transcribe-{}-{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&path, wav)
            .map_err(|e| format!("could not stage the recording for whisper.cpp: {e}"))?;
        let out = self.transcribe(model, &path, language, threads);
        let _ = std::fs::remove_file(&path);
        out
    }

    /// Transcribe a WAV that is already on disk.
    pub fn transcribe(
        &self,
        model: &Path,
        wav: &Path,
        language: &str,
        threads: Option<usize>,
    ) -> Result<String, String> {
        let out = Command::new(&self.program)
            .args(self.argv(model, wav, language, threads))
            .output()
            .map_err(|e| format!("could not run {}: {e}", self.program.display()))?;

        if !out.status.success() {
            // whisper.cpp puts its diagnostics on stderr, and they are specific
            // — a missing model, an unreadable WAV. Guessing from an exit code
            // would waste the debugging cycle those messages exist to save.
            let said = String::from_utf8_lossy(&out.stderr);
            let line = said
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("no output");
            return Err(format!("whisper.cpp failed: {line}"));
        }
        Ok(clean(&String::from_utf8_lossy(&out.stdout)))
    }
}

/// Tidy whisper.cpp's stdout into the sentence that was said.
///
/// Whisper marks non-speech with bracketed pseudo-captions — `[BLANK_AUDIO]`,
/// `(wind blowing)`, `[Music]` — which are descriptions of the recording, not
/// words spoken into it. They come from the subtitle corpora it was trained on
/// and would otherwise be typed into the composer as if dictated.
///
/// Only whole-line and whole-token brackets are removed. A bracket inside a
/// sentence is far more likely to be something said — an array index, a
/// citation — than a caption.
pub fn clean(raw: &str) -> String {
    let mut out = String::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // A line that is *entirely* a caption is dropped whole.
        let bare = line.trim_start_matches(['[', '(']).trim_end_matches([']', ')']);
        let is_caption = (line.starts_with('[') && line.ends_with(']'))
            || (line.starts_with('(') && line.ends_with(')'));
        if is_caption && !bare.contains(' ') || is_caption && looks_like_caption(bare) {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(line);
    }
    out.trim().to_string()
}

/// Whether bracketed text reads as a subtitle caption rather than speech.
fn looks_like_caption(inner: &str) -> bool {
    let lower = inner.to_lowercase();
    const MARKERS: [&str; 10] = [
        "blank_audio",
        "blank audio",
        "silence",
        "music",
        "applause",
        "laughter",
        "inaudible",
        "no speech",
        "speaking in",
        "foreign language",
    ];
    MARKERS.iter().any(|m| lower.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- the flags that decide whether Taglish survives ----

    /// The single most important line in this file. `-l` defaults to `en`, so
    /// omitting it decodes Tagalog as English.
    #[test]
    fn a_language_is_always_passed_because_the_default_is_english() {
        let w = Whisper {
            program: "whisper-cli".into(),
        };
        let args = w.argv(Path::new("m.bin"), Path::new("a.wav"), "auto", None);
        let at = args.iter().position(|a| a == "-l").expect("no -l flag");
        assert_eq!(args[at + 1], "auto");
    }

    /// Translation reads as success and is the one outcome worse than a bad
    /// transcript.
    #[test]
    fn translation_is_never_requested() {
        let w = Whisper {
            program: "whisper-cli".into(),
        };
        let args = w.argv(Path::new("m.bin"), Path::new("a.wav"), "auto", Some(4));
        assert!(!args.iter().any(|a| a == "-tr" || a == "--translate"));
    }

    /// stdout has to be the sentence and nothing else, or the composer fills
    /// with timestamps and system info.
    #[test]
    fn stdout_is_kept_clean_of_everything_but_the_words() {
        let w = Whisper {
            program: "whisper-cli".into(),
        };
        let args = w.argv(Path::new("m.bin"), Path::new("a.wav"), "auto", None);
        assert!(args.iter().any(|a| a == "-nt"), "timestamps not suppressed");
        assert!(args.iter().any(|a| a == "-np"), "logging not suppressed");
    }

    #[test]
    fn the_model_and_the_audio_both_reach_the_command() {
        let w = Whisper {
            program: "whisper-cli".into(),
        };
        let args = w.argv(Path::new("/m/ggml.bin"), Path::new("/tmp/a.wav"), "auto", None);
        assert!(args.iter().any(|a| a == "/m/ggml.bin"));
        assert!(args.iter().any(|a| a == "/tmp/a.wav"));
    }

    #[test]
    fn threads_are_left_to_whisper_when_unset() {
        let w = Whisper {
            program: "whisper-cli".into(),
        };
        let args = w.argv(Path::new("m.bin"), Path::new("a.wav"), "auto", None);
        assert!(!args.iter().any(|a| a == "-t"));
    }

    // ---- the catalog ----

    /// The whole reason this catalog is hand-written rather than mirroring the
    /// host: an English-only model cannot represent Tagalog, so offering one
    /// would be offering the bug.
    #[test]
    fn no_english_only_model_is_offered() {
        for m in CATALOG {
            assert!(
                !m.file.contains(".en"),
                "{} is English-only and would delete the Tagalog",
                m.name
            );
        }
    }

    #[test]
    fn the_recommended_model_is_in_the_catalog() {
        assert!(model(RECOMMENDED).is_some());
    }

    /// The recommendation has to stay tied to the family the research measured.
    #[test]
    fn the_recommendation_is_the_family_measured_on_taglish() {
        assert!(RECOMMENDED.contains("large-v3-turbo"));
    }

    #[test]
    fn a_model_is_found_by_the_name_you_would_type() {
        assert_eq!(model("small").map(|m| m.file), Some("ggml-small-q5_1.bin"));
        assert_eq!(model("  SMALL ").map(|m| m.file), Some("ggml-small-q5_1.bin"));
    }

    #[test]
    fn an_unknown_model_is_not_invented() {
        assert!(model("enormous").is_none());
    }

    #[test]
    fn every_model_downloads_from_the_weights_host() {
        for m in CATALOG {
            assert!(m.url().starts_with("https://"), "{} is not fetched over TLS", m.name);
            assert!(m.url().ends_with(m.file));
        }
    }

    #[test]
    fn models_live_under_jods_home_not_the_checkout() {
        let m = model("small").unwrap();
        let p = m.path(Path::new("/home/reljod/.jod"));
        assert!(p.starts_with("/home/reljod/.jod"));
        assert!(p.ends_with("ggml-small-q5_1.bin"));
    }

    // ---- cleaning what whisper says ----

    #[test]
    fn a_plain_transcript_is_left_alone() {
        assert_eq!(
            clean(" And so my fellow Americans, ask not.\n"),
            "And so my fellow Americans, ask not."
        );
    }

    /// The one that would otherwise be typed into the composer verbatim.
    #[test]
    fn a_blank_audio_caption_is_not_treated_as_speech() {
        assert_eq!(clean("[BLANK_AUDIO]"), "");
    }

    #[test]
    fn subtitle_captions_are_dropped() {
        assert_eq!(clean("[Music]\n(applause)\n[ Silence ]"), "");
    }

    /// Whisper emits one line per segment; dictation wants one sentence.
    #[test]
    fn segments_are_joined_into_one_line() {
        assert_eq!(clean("fix the parser\nand run the tests"), "fix the parser and run the tests");
    }

    #[test]
    fn a_caption_between_two_real_lines_does_not_eat_them() {
        assert_eq!(
            clean("i-refactor natin\n[BLANK_AUDIO]\nyung parser"),
            "i-refactor natin yung parser"
        );
    }

    /// Brackets inside a sentence are far likelier to be something said — an
    /// array index — than a caption.
    #[test]
    fn brackets_inside_a_sentence_are_kept() {
        assert_eq!(clean("set items[0] to null"), "set items[0] to null");
    }

    #[test]
    fn taglish_survives_cleaning_untouched() {
        let said = "pwede ba nating i-refactor yung parser ngayon?";
        assert_eq!(clean(said), said);
    }
}
