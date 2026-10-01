//! Windowed-FFT spectrum analysis. Used to back the `fft_left`/`fft_right`
//! functions exposed to visualizer scripts (see `lua_visualizer.rs`) — the
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
