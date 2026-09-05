//! FFT-based stereo spectrum visualizer.
//!
//! Left and right channels are drawn as two overlapping sets of bars in
//! complementary colors — red for left, cyan for right — so a column lit by
//! both channels equally combines to white (`red + cyan = white`). Bars grow
//! from the bottom of the window; a column at full scale reaches the very
//! top row. Bar heights are smoothed across frames so they don't flicker.

use std::collections::VecDeque;
use std::f32::consts::PI;
use std::sync::Arc;

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

use crate::visualizer::{StereoFrame, Visualizer};

/// Samples per channel fed into each FFT. A power of two; at 44.1kHz that's
/// ~23ms of audio and ~43Hz of frequency resolution per bin.
const FFT_SIZE: usize = 1024;
/// Must match the engine's configured output sample rate (see `main.rs`).
const SAMPLE_RATE: f32 = 44_100.0;

/// Bars cover this frequency range, log-spaced across the window's width —
/// most musically interesting content lives here, and log spacing gives bass
/// as much horizontal room as treble, the way a real spectrum analyzer does.
const MIN_FREQUENCY_HZ: f32 = 30.0;
const MAX_FREQUENCY_HZ: f32 = 16_000.0;

/// A bin at or above this reads as 0dB and fills its bar completely; at or
/// below the other end it reads as silence. Roughly dBFS for a windowed FFT.
const MAX_DB: f32 = 0.0;
const MIN_DB: f32 = -60.0;

/// Per-frame exponential smoothing of bar height: higher = smoother but
/// laggier. This is deliberately mild ("minor smoothing"), not a slow decay.
const SMOOTHING: f32 = 0.55;

const BACKGROUND: u32 = 0x0010_1018;
const LEFT_COLOR: u32 = 0x00ff_0000; // red
const RIGHT_COLOR: u32 = 0x0000_ffff; // cyan
const BOTH_COLOR: u32 = 0x00ff_ffff; // red + cyan = white

pub struct FastFourierTransform {
    left_ring: VecDeque<f32>,
    right_ring: VecDeque<f32>,
    fft: Arc<dyn Fft<f32>>,
    /// Smoothed 0..1 bar heights from the previous frame; resized to the
    /// window's width on demand.
    left_bars: Vec<f32>,
    right_bars: Vec<f32>,
}

impl Default for FastFourierTransform {
    fn default() -> Self {
        Self {
            left_ring: VecDeque::with_capacity(FFT_SIZE),
            right_ring: VecDeque::with_capacity(FFT_SIZE),
            fft: FftPlanner::new().plan_fft_forward(FFT_SIZE),
            left_bars: Vec::new(),
            right_bars: Vec::new(),
        }
    }
}

impl Visualizer for FastFourierTransform {
    fn render(&mut self, buffer: &mut [u32], width: usize, height: usize, samples: &[StereoFrame]) {
        for &(left, right) in samples {
            push_sample(&mut self.left_ring, left);
            push_sample(&mut self.right_ring, right);
        }

        let left_spectrum = spectrum(&self.left_ring, &self.fft);
        let right_spectrum = spectrum(&self.right_ring, &self.fft);

        resize_bars(&mut self.left_bars, width);
        resize_bars(&mut self.right_bars, width);
        smooth_bars(&mut self.left_bars, &left_spectrum, width);
        smooth_bars(&mut self.right_bars, &right_spectrum, width);

        buffer.fill(BACKGROUND);
        for x in 0..width {
            draw_column(buffer, width, height, x, self.left_bars[x], self.right_bars[x]);
        }
    }
}

/// Push a new sample, dropping the oldest once the ring holds a full FFT window.
fn push_sample(ring: &mut VecDeque<f32>, sample: f32) {
    ring.push_back(sample);
    if ring.len() > FFT_SIZE {
        ring.pop_front();
    }
}

/// Windowed FFT magnitude spectrum of `ring`, normalized so a full-scale sine
/// tone reads about `1.0`. Empty until a full window of samples has arrived.
fn spectrum(ring: &VecDeque<f32>, fft: &Arc<dyn Fft<f32>>) -> Vec<f32> {
    if ring.len() < FFT_SIZE {
        return Vec::new();
    }

    // Hann window to reduce spectral leakage from the edges of the buffer.
    let mut buffer: Vec<Complex32> = ring
        .iter()
        .enumerate()
        .map(|(i, &sample)| {
            let w = 0.5 - 0.5 * (2.0 * PI * i as f32 / (FFT_SIZE - 1) as f32).cos();
            Complex32::new(sample * w, 0.0)
        })
        .collect();
    fft.process(&mut buffer);

    // The input is real-valued, so only the first half of the spectrum
    // (up to Nyquist) carries independent information.
    buffer[..FFT_SIZE / 2]
        .iter()
        .map(|c| c.norm() * 2.0 / FFT_SIZE as f32)
        .collect()
}

fn resize_bars(bars: &mut Vec<f32>, width: usize) {
    if bars.len() != width {
        *bars = vec![0.0; width];
    }
}

fn smooth_bars(bars: &mut [f32], spectrum: &[f32], width: usize) {
    for (x, bar) in bars.iter_mut().enumerate() {
        let target = bucket_unit_value(spectrum, x, width);
        *bar = *bar * SMOOTHING + target * (1.0 - SMOOTHING);
    }
}

/// Peak magnitude (as a 0..1 fraction of the `MIN_DB..MAX_DB` range) within
/// the log-spaced frequency bucket that column `x` of `width` covers.
fn bucket_unit_value(spectrum: &[f32], x: usize, width: usize) -> f32 {
    if spectrum.is_empty() || width == 0 {
        return 0.0;
    }

    let freq_at = |t: f32| MIN_FREQUENCY_HZ * (MAX_FREQUENCY_HZ / MIN_FREQUENCY_HZ).powf(t);
    let bin_for_freq = |freq: f32| ((freq / SAMPLE_RATE) * FFT_SIZE as f32).round() as usize;

    // Skip bin 0 (DC) and clamp into range so odd window sizes can't panic.
    let lo = bin_for_freq(freq_at(x as f32 / width as f32)).clamp(1, spectrum.len() - 1);
    let hi = bin_for_freq(freq_at((x + 1) as f32 / width as f32)).clamp(lo + 1, spectrum.len());

    let peak = spectrum[lo..hi].iter().copied().fold(0.0f32, f32::max);
    let db = 20.0 * peak.max(1e-6).log10();
    ((db - MIN_DB) / (MAX_DB - MIN_DB)).clamp(0.0, 1.0)
}

/// Draw one column's left/right bars bottom-up, blending to white where both
/// cover the same row.
fn draw_column(buffer: &mut [u32], width: usize, height: usize, x: usize, left: f32, right: f32) {
    if height == 0 {
        return;
    }
    let left_h = (left.clamp(0.0, 1.0) * (height - 1) as f32).round() as usize;
    let right_h = (right.clamp(0.0, 1.0) * (height - 1) as f32).round() as usize;

    for y in 0..height {
        let from_bottom = height - 1 - y;
        let color = match (from_bottom <= left_h, from_bottom <= right_h) {
            (true, true) => BOTH_COLOR,
            (true, false) => LEFT_COLOR,
            (false, true) => RIGHT_COLOR,
            (false, false) => continue,
        };
        buffer[y * width + x] = color;
    }
}
