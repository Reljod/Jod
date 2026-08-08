//! Microphone capture.
//!
//! Records at whatever the input device natively offers, then downmixes to mono
//! and resamples to 16 kHz — the rate every ASR model here expects. Doing the
//! conversion locally keeps the upload small, which is most of the wire latency
//! on a short utterance.

use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SampleFormat, Stream};

use crate::guard;

/// Target rate for every model on the OpenRouter transcription endpoint.
pub const TARGET_RATE: u32 = 16_000;

#[derive(Default)]
struct Capture {
    /// Interleaved f32 samples exactly as the device delivered them.
    samples: Vec<f32>,
    channels: u16,
    rate: u32,
}

pub struct Recorder {
    capture: Arc<Mutex<Capture>>,
    /// Held only while recording. `cpal::Stream` is not `Send`, so the whole
    /// recorder lives behind the app's single-threaded state guard.
    stream: Option<Stream>,
}

// SAFETY: the stream is created, used and dropped under the same `Mutex` in
// `AppState`, so it is never touched from two threads at once. cpal marks
// `Stream` `!Send` only because some backends bind it to the creating thread;
// Tauri's command runtime keeps our access serialized.
unsafe impl Send for Recorder {}

impl Recorder {
    pub fn new() -> Self {
        Self { capture: Arc::new(Mutex::new(Capture::default())), stream: None }
    }

    pub fn is_recording(&self) -> bool {
        self.stream.is_some()
    }

    pub fn start(&mut self) -> Result<(), String> {
        if self.stream.is_some() {
            return Ok(());
        }

        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| "no microphone found — check System Settings › Sound › Input".to_string())?;
        let config = device
            .default_input_config()
            .map_err(|e| format!("could not read microphone config: {e}"))?;

        {
            let mut c = self.capture.lock().unwrap();
            c.samples.clear();
            c.channels = config.channels();
            c.rate = config.sample_rate().0;
        }

        let capture = Arc::clone(&self.capture);
        let err_fn = |e| eprintln!("[jod-voice] audio stream error: {e}");

        // Every backend hands us a different sample type; normalize to f32 up front.
        let stream = match config.sample_format() {
            SampleFormat::F32 => device.build_input_stream(
                &config.into(),
                move |data: &[f32], _: &_| push(&capture, data.iter().copied()),
                err_fn,
                None,
            ),
            SampleFormat::I16 => device.build_input_stream(
                &config.into(),
                move |data: &[i16], _: &_| push(&capture, data.iter().map(|s| s.to_float_sample())),
                err_fn,
                None,
            ),
            SampleFormat::U16 => device.build_input_stream(
                &config.into(),
                move |data: &[u16], _: &_| push(&capture, data.iter().map(|s| s.to_float_sample())),
                err_fn,
                None,
            ),
            other => return Err(format!("unsupported sample format: {other:?}")),
        }
        .map_err(|e| format!("could not open microphone stream: {e}"))?;

        stream.play().map_err(|e| format!("could not start microphone: {e}"))?;
        self.stream = Some(stream);
        Ok(())
    }

    /// Stops capture and returns a 16 kHz mono WAV as raw bytes.
    ///
    /// Refuses buffers that hold no speech. This is the primary defence against
    /// hallucinated transcripts: fed silence or room tone, Whisper invents
    /// canned phrases and its language detector can wander into Korean. Not
    /// sending the request at all is both the correct answer and the cheap one.
    pub fn stop(&mut self) -> Result<Vec<u8>, String> {
        let stream = self.stream.take().ok_or_else(|| "not recording".to_string())?;
        drop(stream); // closing the stream flushes the last callback

        let (samples, channels, rate) = {
            let c = self.capture.lock().unwrap();
            (c.samples.clone(), c.channels.max(1), c.rate.max(1))
        };
        if samples.is_empty() {
            return Err("no audio captured — is microphone permission granted?".into());
        }

        let mono = downmix(&samples, channels);
        let resampled = resample(&mono, rate, TARGET_RATE);

        match guard::assess(&resampled, TARGET_RATE) {
            guard::Speech::Present => encode_wav(&resampled),
            verdict => Err(verdict.message().to_string()),
        }
    }

    /// Peak amplitude of the most recent window, for the level meter.
    pub fn level(&self) -> f32 {
        let c = self.capture.lock().unwrap();
        let window = 2048.min(c.samples.len());
        c.samples[c.samples.len() - window..]
            .iter()
            .fold(0.0_f32, |peak, s| peak.max(s.abs()))
    }

    pub fn duration_secs(&self) -> f32 {
        let c = self.capture.lock().unwrap();
        if c.rate == 0 || c.channels == 0 {
            return 0.0;
        }
        c.samples.len() as f32 / (c.rate as f32 * c.channels as f32)
    }
}

fn push(capture: &Arc<Mutex<Capture>>, data: impl Iterator<Item = f32>) {
    if let Ok(mut c) = capture.lock() {
        c.samples.extend(data);
    }
}

fn downmix(samples: &[f32], channels: u16) -> Vec<f32> {
    if channels <= 1 {
        return samples.to_vec();
    }
    let n = channels as usize;
    samples.chunks(n).map(|f| f.iter().sum::<f32>() / n as f32).collect()
}

/// Linear-interpolation resampler. Speech at 16 kHz is band-limited enough that
/// the aliasing a proper anti-alias filter would remove is inaudible to the
/// models — and this keeps the hot path allocation-light.
fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let out_len = ((input.len() as f64) / ratio).floor() as usize;
    (0..out_len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let idx = pos.floor() as usize;
            let frac = (pos - idx as f64) as f32;
            let a = input[idx];
            let b = *input.get(idx + 1).unwrap_or(&a);
            a + (b - a) * frac
        })
        .collect()
}

fn encode_wav(samples: &[f32]) -> Result<Vec<u8>, String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut w = hound::WavWriter::new(&mut buf, spec)
            .map_err(|e| format!("could not encode WAV: {e}"))?;
        for &s in samples {
            let clamped = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            w.write_sample(clamped).map_err(|e| format!("could not write WAV: {e}"))?;
        }
        w.finalize().map_err(|e| format!("could not finalize WAV: {e}"))?;
    }
    Ok(buf.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_averages_stereo_pairs() {
        assert_eq!(downmix(&[1.0, 0.0, 0.5, 0.5], 2), vec![0.5, 0.5]);
    }

    #[test]
    fn downmix_passes_mono_through() {
        assert_eq!(downmix(&[0.1, 0.2], 1), vec![0.1, 0.2]);
    }

    #[test]
    fn resample_downsamples_to_expected_length() {
        let input: Vec<f32> = (0..48_000).map(|i| (i as f32 / 100.0).sin()).collect();
        assert_eq!(resample(&input, 48_000, 16_000).len(), 16_000);
    }

    #[test]
    fn resample_is_identity_at_same_rate() {
        let input = vec![0.1, -0.2, 0.3];
        assert_eq!(resample(&input, 16_000, 16_000), input);
    }

    #[test]
    fn wav_has_riff_header_and_expected_payload() {
        let wav = encode_wav(&[0.0, 0.5, -0.5]).unwrap();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        // 44-byte canonical header + 3 samples × 2 bytes
        assert_eq!(wav.len(), 44 + 6);
    }

    #[test]
    fn wav_clamps_out_of_range_samples() {
        let wav = encode_wav(&[9.0, -9.0]).unwrap();
        let hi = i16::from_le_bytes([wav[44], wav[45]]);
        let lo = i16::from_le_bytes([wav[46], wav[47]]);
        assert_eq!(hi, i16::MAX);
        assert_eq!(lo, -i16::MAX);
    }
}
