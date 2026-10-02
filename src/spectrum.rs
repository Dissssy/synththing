//! Windowed-FFT spectrum analysis. Used to back the `fft_left`/`fft_right`
//! functions exposed to visualizer scripts (see `lua_visualizer.rs`), the
//! accelerated part of the "global Rust accel functions" a script can call
//! instead of doing heavy per-sample math in interpreted Lua.

use std::collections::VecDeque;
use std::f32::consts::PI;
use std::sync::Arc;

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

/// Samples fed into each FFT. A power of two; at 44.1kHz that's ~23ms of
/// audio and ~43Hz of frequency resolution per bin.
pub const FFT_SIZE: usize = 1024;

/// A rolling window over one audio channel plus a planned FFT to analyze it.
/// Each channel of a stereo signal wants its own instance.
pub struct SpectrumAnalyzer {
    ring: VecDeque<f32>,
    fft: Arc<dyn Fft<f32>>,
}

impl SpectrumAnalyzer {
    pub fn new() -> Self {
        Self {
            ring: VecDeque::with_capacity(FFT_SIZE),
            fft: FftPlanner::new().plan_fft_forward(FFT_SIZE),
        }
    }

    /// Push newly-arrived samples into the rolling window and return the
    /// current windowed magnitude spectrum (length `FFT_SIZE / 2`, covering
    /// DC up to Nyquist), normalized so a full-scale sine tone reads about
    /// `1.0`. Empty until a full window of samples has arrived.
    pub fn analyze(&mut self, samples: &[f32]) -> Vec<f32> {
        for &sample in samples {
            self.ring.push_back(sample);
            if self.ring.len() > FFT_SIZE {
                self.ring.pop_front();
            }
        }
        if self.ring.len() < FFT_SIZE {
            return Vec::new();
        }

        // Hann window to reduce spectral leakage from the edges of the buffer.
        let mut buffer: Vec<Complex32> = self
            .ring
            .iter()
            .enumerate()
            .map(|(i, &sample)| {
                let w = 0.5 - 0.5 * (2.0 * PI * i as f32 / (FFT_SIZE - 1) as f32).cos();
                Complex32::new(sample * w, 0.0)
            })
            .collect();
        self.fft.process(&mut buffer);

        // The input is real-valued, so only the first half of the spectrum
        // (up to Nyquist) carries independent information.
        buffer[..FFT_SIZE / 2]
            .iter()
            .map(|c| c.norm() * 2.0 / FFT_SIZE as f32)
            .collect()
    }
}

impl Default for SpectrumAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

/// How many recent frames the onset threshold averages over (~0.5 s at
/// 60 fps).
const ONSET_HISTORY: usize = 30;
/// After an onset, ignore new ones for this long, so one hit isn't
/// reported over several frames.
const ONSET_REFRACTORY_SECS: f32 = 0.08;

/// Rough note-onset detection for scripts (`onset()`): spectral flux, how
/// much the spectrum gained since the previous frame, against an adaptive
/// threshold from the last half second. Catches drum hits and note attacks
/// well enough for visuals; not a beat tracker.
pub struct OnsetDetector {
    analyzer: SpectrumAnalyzer,
    previous: Vec<f32>,
    history: VecDeque<f32>,
    samples_since_onset: usize,
    refractory_samples: usize,
}

impl OnsetDetector {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            analyzer: SpectrumAnalyzer::new(),
            previous: Vec::new(),
            history: VecDeque::with_capacity(ONSET_HISTORY),
            samples_since_onset: usize::MAX / 2,
            refractory_samples: (sample_rate as f32 * ONSET_REFRACTORY_SECS) as usize,
        }
    }

    /// Feed this frame's samples (mono); returns whether an onset happened
    /// and the frame's flux (its strength: bigger is a sharper change). No
    /// samples (paused) is never an onset.
    pub fn update(&mut self, mono: &[f32]) -> (bool, f32) {
        if mono.is_empty() {
            return (false, 0.0);
        }
        self.samples_since_onset = self.samples_since_onset.saturating_add(mono.len());
        let spectrum = self.analyzer.analyze(mono);
        if spectrum.is_empty() {
            return (false, 0.0);
        }
        let flux: f32 = if self.previous.len() == spectrum.len() {
            spectrum.iter().zip(&self.previous).map(|(now, before)| (now - before).max(0.0)).sum()
        } else {
            0.0
        };
        self.previous = spectrum;

        let mean = if self.history.is_empty() {
            0.0
        } else {
            self.history.iter().sum::<f32>() / self.history.len() as f32
        };
        let onset = self.history.len() >= 4
            && flux > mean * 1.5 + 0.02
            && self.samples_since_onset >= self.refractory_samples;
        if onset {
            self.samples_since_onset = 0;
        }
        if self.history.len() == ONSET_HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(flux);
        (onset, flux)
    }
}

/// Root-mean-square level of `samples`: 0 for silence, about 0.707 for a
/// full-scale sine, 1.0 for full-scale square.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rms_of_known_signals() {
        assert_eq!(rms(&[]), 0.0);
        assert!((rms(&[1.0, -1.0, 1.0, -1.0]) - 1.0).abs() < 1e-6);
        let sine: Vec<f32> = (0..44_100).map(|i| (i as f32 * 0.05).sin()).collect();
        assert!((rms(&sine) - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.01);
    }

    #[test]
    fn onsets_on_sudden_sound_not_steady_sound() {
        let mut detector = OnsetDetector::new(44_100);
        let frame = 735;
        let silence = vec![0.0f32; frame];
        let tone = |t0: usize| -> Vec<f32> { (t0..t0 + frame).map(|i| (i as f32 * 0.3).sin() * 0.8).collect() };
        let mut onsets = Vec::new();
        for n in 0..60 {
            // Quiet for a while, then a tone starts at frame 30 and holds.
            let samples = if n < 30 { silence.clone() } else { tone(n * frame) };
            onsets.push(detector.update(&samples).0);
        }
        let hits: Vec<usize> = onsets.iter().enumerate().filter(|&(_, &o)| o).map(|(i, _)| i).collect();
        assert!(!hits.is_empty() && hits[0] >= 30 && hits[0] <= 32, "{hits:?}");
        // A steady tone afterwards isn't a stream of onsets.
        assert!(hits.iter().filter(|&&i| i > 36).count() == 0, "{hits:?}");
        assert!(!detector.update(&[]).0);
    }
}
