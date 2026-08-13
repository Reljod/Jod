//! Listening continuously, and cutting what was said into sentences.
//!
//! The hands-free mode. The microphone is switched on once and stays on; this
//! module decides where one utterance ends and the next begins, so the console
//! can transcribe a sentence while you are already speaking the next one.
//!
//! ## Why the WAV is tailed rather than piped
//!
//! The recorder is a subprocess writing a file ([`crate::record`] explains
//! why). Reading its stdout instead would mean every backend agreeing on a raw
//! stream format and on flushing, which they do not. Tailing the file works
//! identically for all four.
//!
//! The header is read once for the format and then ignored. A growing WAV's
//! RIFF length field is *wrong* by construction — the writer only fixes it on
//! clean exit — so anything trusting it sees an empty file. What is true is
//! that samples are appended after the `data` chunk header, and that is what
//! this reads.
//!
//! ## Where a sentence ends
//!
//! Energy-based endpointing over 20 ms frames: speech starts after a few loud
//! frames, and ends after [`SILENCE_MS`] of quiet ones. That is the same shape
//! as the pause you naturally leave between sentences, which is why it feels
//! like it is following you rather than cutting you off.
//!
//! It is deliberately *not* whisper's own VAD. That would mean a model pass to
//! find out whether there was anything to transcribe, which is the cost this
//! avoids — an idle microphone in a quiet room should cost nothing at all.

use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

use crate::record::{Recorder, TARGET_RATE};

/// Frame length for endpointing. 20 ms is the usual granularity for this and
/// is short enough that a cut lands on the pause rather than inside a word.
const FRAME_MS: usize = 20;

/// Quiet needed to end an utterance.
///
/// 700 ms: longer than the gap inside a sentence — including the pause before
/// a word somebody is searching for — and shorter than the beat left after
/// finishing a thought. Too short chops sentences in half; too long makes the
/// console feel like it is lagging behind.
pub const SILENCE_MS: usize = 700;

/// Speech needed before an utterance is considered started.
///
/// Three frames. Stops a cough, a key press or a chair creak from opening an
/// utterance that then has to be thrown away after a transcription.
const ONSET_FRAMES: usize = 3;

/// Frame loudness that counts as speech.
///
/// Deliberately above [`crate::guard`]'s absolute floor: that one asks "was
/// this buffer speech at all", which is a different and more forgiving
/// question than "is he talking right now".
const SPEECH_RMS: f32 = 0.012;

/// Longest a single utterance may run before it is cut anyway.
///
/// Somebody dictating a long instruction without pausing should still see it
/// arriving, rather than nothing until they stop.
pub const MAX_UTTERANCE_SECS: usize = 25;

/// The shortest utterance worth sending to a model.
///
/// Below this it is almost always a door, a cough, or the tail of a word
/// already transcribed — and each one costs a model pass.
pub const MIN_UTTERANCE_SECS: f32 = 0.35;

/// Where one utterance ends and the next begins.
///
/// Split out from [`Session`] so it can be driven frame by frame in a test
/// without a microphone. That is not only convenience: this is the logic that
/// decides whether a sentence gets chopped in half, and it was worth a test
/// that feeds it real frame shapes rather than one that re-implemented it
/// alongside and agreed with itself.
#[derive(Default)]
pub struct Endpointer {
    pending: Vec<f32>,
    quiet_frames: usize,
    loud_frames: usize,
    speaking: bool,
}

impl Endpointer {
    pub fn is_speaking(&self) -> bool {
        self.speaking
    }

    /// Feed one whole frame. Returns an utterance when one has just ended.
    pub fn push(&mut self, frame: &[f32]) -> Option<Vec<f32>> {
        let (_, rms) = crate::guard::levels(frame);

        // Kept once speaking, so the pauses *inside* a sentence stay in the
        // audio handed to the model — whisper uses them for punctuation.
        if self.speaking || rms >= SPEECH_RMS {
            self.pending.extend_from_slice(frame);
        }

        if rms >= SPEECH_RMS {
            self.quiet_frames = 0;
            if !self.speaking {
                self.loud_frames += 1;
                if self.loud_frames >= ONSET_FRAMES {
                    self.speaking = true;
                }
            }
        } else {
            self.loud_frames = 0;
            if self.speaking {
                self.quiet_frames += 1;
            }
        }

        let ended = self.speaking && self.quiet_frames * FRAME_MS >= SILENCE_MS;
        let overran = self.pending.len() >= MAX_UTTERANCE_SECS * TARGET_RATE as usize;
        if !ended && !overran {
            return None;
        }

        // The silence that *ended* the utterance is not part of what was said.
        // Trimming it before the length check is what stops a cough followed
        // by 700 ms of quiet from measuring long enough to buy a model pass —
        // and it saves the model transcribing silence.
        let mut said = std::mem::take(&mut self.pending);
        let trailing = self.quiet_frames * frame_samples();
        said.truncate(said.len().saturating_sub(trailing));

        self.speaking = false;
        self.quiet_frames = 0;
        self.loud_frames = 0;

        long_enough(&said).then_some(said)
    }

    /// Whatever is part-spoken, for a session being switched off mid-sentence.
    pub fn take_pending(&mut self) -> Option<Vec<f32>> {
        let said = std::mem::take(&mut self.pending);
        long_enough(&said).then_some(said)
    }
}

/// A continuous listening session.
///
/// Owns the recorder and the read position in its file. Poll it; it hands back
/// finished utterances.
pub struct Session {
    recorder: Recorder,
    path: PathBuf,
    /// Byte offset of the next unread sample.
    at: u64,
    /// Set once the `data` chunk has been located.
    data_start: Option<u64>,
    bits: u16,
    channels: u16,
    /// Where sentences are cut.
    ends: Endpointer,
    /// Samples left over when a read did not land on a whole frame.
    spare: Vec<f32>,
    /// Bytes left over when a read did not land on a whole sample.
    remainder: Vec<u8>,
}

/// What a poll produced.
#[derive(Debug, Clone, PartialEq)]
pub enum Heard {
    /// Nothing has finished. `level` drives the meter, so the screen can show
    /// the microphone is live rather than merely claiming it.
    Nothing { level: f32, speaking: bool },
    /// A finished utterance, ready to transcribe.
    Utterance { samples: Vec<f32> },
}

impl Session {
    /// Switch the microphone on.
    pub fn start() -> Result<Session, String> {
        let recorder = Recorder::start_session()?;
        Ok(Session {
            path: recorder.path().to_path_buf(),
            recorder,
            at: 0,
            data_start: None,
            bits: 16,
            channels: 1,
            ends: Endpointer::default(),
            spare: Vec::new(),
            remainder: Vec::new(),
        })
    }

    pub fn backend(&self) -> &'static str {
        self.recorder.backend().program()
    }

    /// Whether the recorder is still alive.
    ///
    /// A session whose recorder died is deaf while still looking live, which is
    /// the worst possible state for something you are talking to with your
    /// hands full — so the caller checks this and says so.
    pub fn is_running(&mut self) -> bool {
        self.recorder.is_running()
    }

    /// Switch the microphone off, returning anything still part-spoken.
    ///
    /// Switching off mid-sentence should not lose the sentence — that is the
    /// difference between a toggle you can trust and one you have to time.
    pub fn finish(mut self) -> Option<Vec<f32>> {
        // `self` drops after this, which stops the recorder and removes the
        // file.
        self.ends.take_pending()
    }

    /// Read whatever has been recorded since the last call.
    ///
    /// Cheap when nothing has been said: a file length check and a short read.
    pub fn poll(&mut self) -> Result<Heard, String> {
        let fresh = self.read_new()?;
        if fresh.is_empty() && self.spare.is_empty() {
            return Ok(Heard::Nothing {
                level: 0.0,
                speaking: self.ends.is_speaking(),
            });
        }

        // A read almost never lands on a frame boundary. The tail is carried
        // to the next poll rather than measured short — a half frame reads as
        // quieter than it was, and enough of them in a row would end a
        // sentence somebody was still speaking.
        let mut buf = std::mem::take(&mut self.spare);
        buf.extend_from_slice(&fresh);

        let frame = frame_samples();
        let mut level = 0.0f32;
        let mut finished: Option<Vec<f32>> = None;

        let whole = buf.len() - (buf.len() % frame);
        for chunk in buf[..whole].chunks(frame) {
            let (_, rms) = crate::guard::levels(chunk);
            level = level.max(rms);
            if let Some(said) = self.ends.push(chunk) {
                // One utterance per poll is enough: a sentence plus its
                // trailing silence cannot both arrive inside one tick, and
                // holding the later one costs nothing but a tick.
                finished = Some(said);
            }
        }
        self.spare = buf[whole..].to_vec();

        match finished {
            Some(samples) => Ok(Heard::Utterance { samples }),
            None => Ok(Heard::Nothing {
                level,
                speaking: self.ends.is_speaking(),
            }),
        }
    }

    /// Samples appended to the WAV since the last read.
    fn read_new(&mut self) -> Result<Vec<f32>, String> {
        let mut file = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            // The recorder may not have created it yet on the first tick.
            Err(_) => return Ok(Vec::new()),
        };

        if self.data_start.is_none() {
            match read_header(&mut file)? {
                Some(header) => {
                    self.bits = header.bits;
                    self.channels = header.channels.max(1);
                    self.data_start = Some(header.data_start);
                    self.at = header.data_start;
                }
                // Header not fully written yet.
                None => return Ok(Vec::new()),
            }
        }

        let len = file
            .metadata()
            .map_err(|e| format!("could not stat the recording: {e}"))?
            .len();
        if len <= self.at {
            return Ok(Vec::new());
        }

        file.seek(SeekFrom::Start(self.at))
            .map_err(|e| format!("could not seek the recording: {e}"))?;
        let mut buf = Vec::with_capacity((len - self.at) as usize);
        let read = file
            .take(len - self.at)
            .read_to_end(&mut buf)
            .map_err(|e| format!("could not read the recording: {e}"))?;
        self.at += read as u64;

        // A read can land mid-sample; carry the tail to the next one rather
        // than emitting a sample built from half of two.
        if !self.remainder.is_empty() {
            let mut joined = std::mem::take(&mut self.remainder);
            joined.extend_from_slice(&buf);
            buf = joined;
        }
        let width = (self.bits as usize / 8).max(1) * self.channels as usize;
        let usable = buf.len() - (buf.len() % width.max(1));
        self.remainder = buf[usable..].to_vec();

        Ok(decode_pcm(&buf[..usable], self.bits, self.channels))
    }
}

fn frame_samples() -> usize {
    TARGET_RATE as usize * FRAME_MS / 1000
}

fn long_enough(samples: &[f32]) -> bool {
    samples.len() as f32 / TARGET_RATE as f32 >= MIN_UTTERANCE_SECS
}

struct Header {
    bits: u16,
    channels: u16,
    data_start: u64,
}

/// Walk the RIFF chunks for the format and the start of the samples.
///
/// Written by hand rather than with a WAV library for the reason in the module
/// note: every library trusts the length field, and on a file still being
/// written that field says zero.
fn read_header(file: &mut std::fs::File) -> Result<Option<Header>, String> {
    let mut head = [0u8; 12];
    file.seek(SeekFrom::Start(0))
        .map_err(|e| format!("could not read the recording: {e}"))?;
    if file.read_exact(&mut head).is_err() {
        return Ok(None);
    }
    if &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" {
        return Err("the recorder did not write a WAV file".into());
    }

    let mut at = 12u64;
    let mut bits = 16u16;
    let mut channels = 1u16;
    loop {
        let mut chunk = [0u8; 8];
        file.seek(SeekFrom::Start(at))
            .map_err(|e| format!("could not read the recording: {e}"))?;
        if file.read_exact(&mut chunk).is_err() {
            return Ok(None);
        }
        let id = &chunk[0..4];
        let size = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as u64;

        if id == b"fmt " {
            let mut fmt = [0u8; 16];
            if file.read_exact(&mut fmt).is_err() {
                return Ok(None);
            }
            channels = u16::from_le_bytes([fmt[2], fmt[3]]);
            bits = u16::from_le_bytes([fmt[14], fmt[15]]);
        } else if id == b"data" {
            return Ok(Some(Header {
                bits,
                channels,
                data_start: at + 8,
            }));
        }
        // `data` on a growing file carries a placeholder size, so its own
        // length must never be used to skip past it — which is why the branch
        // above returns rather than advancing.
        at += 8 + size;
    }
}

/// Raw PCM to mono f32.
fn decode_pcm(bytes: &[u8], bits: u16, channels: u16) -> Vec<f32> {
    let channels = channels.max(1) as usize;
    let mono: Vec<f32> = match bits {
        16 => bytes
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / i16::MAX as f32)
            .collect(),
        32 => bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect(),
        8 => bytes
            .iter()
            .map(|b| (*b as f32 - 128.0) / 128.0)
            .collect(),
        _ => Vec::new(),
    };
    if channels == 1 {
        return mono;
    }
    mono.chunks(channels)
        .map(|f| f.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Wrap raw samples back into a WAV, for the transcriber.
///
/// The engines take a file or a byte buffer that looks like one, and an
/// utterance cut out of the middle of a session has no header of its own.
pub fn to_wav(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    let data_len = (samples.len() * 2) as u32;
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&TARGET_RATE.to_le_bytes());
    out.extend_from_slice(&(TARGET_RATE * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speech(frames: usize) -> Vec<f32> {
        // Loud and impulsive, like the guard's own fixtures.
        let n = frames * frame_samples();
        (0..n)
            .map(|i| if i % 8 == 0 { 0.6 } else { 0.05 })
            .collect()
    }

    fn quiet(frames: usize) -> Vec<f32> {
        vec![0.0; frames * frame_samples()]
    }

    /// Drives the real [`Endpointer`] — the same code `Session::poll` runs.
    ///
    /// An earlier version of this test re-implemented the endpointing beside
    /// it, which meant the test agreed with a copy of the logic rather than
    /// with the logic. It missed a bug where trailing silence counted toward
    /// an utterance's length.
    struct Fake {
        ends: Endpointer,
        cut: Vec<Vec<f32>>,
    }

    impl Fake {
        fn new() -> Fake {
            Fake {
                ends: Endpointer::default(),
                cut: Vec::new(),
            }
        }

        fn feed(&mut self, samples: &[f32]) {
            for chunk in samples.chunks(frame_samples()) {
                if chunk.len() != frame_samples() {
                    continue;
                }
                if let Some(said) = self.ends.push(chunk) {
                    self.cut.push(said);
                }
            }
        }
    }

    // ---- where a sentence ends ----

    /// The core of hands-free: stop talking, and what you said is cut loose
    /// without touching anything.
    #[test]
    fn a_pause_ends_an_utterance() {
        let mut f = Fake::new();
        f.feed(&speech(40)); // 800 ms of talking
        assert!(f.cut.is_empty(), "cut while still speaking");
        f.feed(&quiet(40)); // 800 ms of pause
        assert_eq!(f.cut.len(), 1, "the pause did not end the utterance");
    }

    /// The pause *inside* a sentence — hunting for a word — must not cut it.
    #[test]
    fn a_short_pause_mid_sentence_does_not_cut_it() {
        let mut f = Fake::new();
        f.feed(&speech(30));
        f.feed(&quiet(20)); // 400 ms, under the threshold
        f.feed(&speech(30));
        assert!(f.cut.is_empty(), "a mid-sentence pause cut the sentence");
    }

    /// Two sentences with a real gap are two utterances, so the second can be
    /// transcribed while the third is being spoken.
    #[test]
    fn two_sentences_become_two_utterances() {
        let mut f = Fake::new();
        f.feed(&speech(30));
        f.feed(&quiet(40));
        f.feed(&speech(30));
        f.feed(&quiet(40));
        assert_eq!(f.cut.len(), 2);
    }

    /// A quiet room must never produce anything to transcribe — an idle
    /// microphone should cost nothing at all.
    #[test]
    fn silence_alone_never_produces_an_utterance() {
        let mut f = Fake::new();
        f.feed(&quiet(500)); // ten seconds of nothing
        assert!(f.cut.is_empty());
    }

    /// A cough or a key press should not open an utterance.
    #[test]
    fn a_single_loud_frame_does_not_open_an_utterance() {
        let mut f = Fake::new();
        f.feed(&speech(1));
        f.feed(&quiet(60));
        assert!(f.cut.is_empty(), "a transient was treated as speech");
    }

    /// Somebody dictating a long instruction without pausing still has to see
    /// it arriving.
    #[test]
    fn an_unbroken_monologue_is_cut_anyway() {
        let mut f = Fake::new();
        let frames = (MAX_UTTERANCE_SECS + 2) * 1000 / FRAME_MS;
        f.feed(&speech(frames));
        assert!(!f.cut.is_empty(), "a long monologue was never cut");
        assert!(
            f.cut[0].len() <= (MAX_UTTERANCE_SECS + 1) * TARGET_RATE as usize,
            "the forced cut did not bound the utterance"
        );
    }

    /// Each utterance costs a model pass, so a scrap of noise must not buy one.
    #[test]
    fn a_scrap_too_short_to_be_a_word_is_dropped() {
        let mut f = Fake::new();
        f.feed(&speech(5)); // 100 ms, under MIN_UTTERANCE_SECS
        f.feed(&quiet(40));
        assert!(f.cut.is_empty());
    }

    // ---- reading a WAV that is still being written ----

    #[test]
    fn sixteen_bit_pcm_decodes_to_normalised_mono() {
        let bytes: Vec<u8> = [i16::MAX, 0, i16::MIN]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let got = decode_pcm(&bytes, 16, 1);
        assert_eq!(got.len(), 3);
        assert!((got[0] - 1.0).abs() < 0.001);
    }

    #[test]
    fn stereo_frames_are_folded_to_mono() {
        let bytes: Vec<u8> = [i16::MAX, 0, i16::MAX, 0]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let got = decode_pcm(&bytes, 16, 2);
        assert_eq!(got.len(), 2);
        assert!((got[0] - 0.5).abs() < 0.01);
    }

    /// The header this module writes has to be readable by the header parser
    /// it also owns — and by whisper.cpp, which only takes 16-bit WAV.
    #[test]
    fn a_written_wav_round_trips_through_the_header_reader() {
        let samples = vec![0.5f32; 1600];
        let wav = to_wav(&samples);

        let dir = std::env::temp_dir().join(format!("jod-stream-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.wav");
        std::fs::write(&path, &wav).unwrap();

        let mut file = std::fs::File::open(&path).unwrap();
        let header = read_header(&mut file).unwrap().expect("header not found");
        assert_eq!(header.bits, 16);
        assert_eq!(header.channels, 1);
        assert_eq!(header.data_start, 44);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_written_wav_declares_the_rate_the_models_expect() {
        let wav = to_wav(&[0.0; 10]);
        let rate = u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]);
        assert_eq!(rate, TARGET_RATE);
    }

    /// A truncated file is the ordinary state of a recording that just
    /// started, not an error.
    #[test]
    fn a_header_that_has_not_been_written_yet_is_not_an_error() {
        let dir = std::env::temp_dir().join(format!("jod-stream-part-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("part.wav");
        std::fs::write(&path, b"RIFF").unwrap();
        let mut file = std::fs::File::open(&path).unwrap();
        assert!(read_header(&mut file).unwrap().is_none());
        let _ = std::fs::remove_file(&path);
    }

    /// The bug this module's hand-written parser exists to avoid: a growing
    /// WAV declares `data` size 0, and anything trusting it reads nothing.
    #[test]
    fn a_data_chunk_claiming_zero_length_still_yields_its_start() {
        let mut wav = to_wav(&vec![0.25f32; 800]);
        // Stamp the placeholder a live recorder writes.
        let n = wav.len();
        wav[40..44].copy_from_slice(&0u32.to_le_bytes());
        wav[4..8].copy_from_slice(&0u32.to_le_bytes());

        let dir = std::env::temp_dir().join(format!("jod-stream-zero-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("growing.wav");
        std::fs::write(&path, &wav).unwrap();

        let mut file = std::fs::File::open(&path).unwrap();
        let header = read_header(&mut file).unwrap().expect("header not found");
        assert_eq!(header.data_start, 44);
        assert!(n > 44, "the fixture had no samples to find");
        let _ = std::fs::remove_file(&path);
    }
}
