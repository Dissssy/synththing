//! A minimal example [`Visualizer`](crate::visualizer::Visualizer): a
//! scrolling oscilloscope-style waveform. This is just a starting point to
//! gut and rebuild — it exists so `--visualizer` draws *something*, and to
//! show the trait in use.

use std::collections::VecDeque;

use crate::visualizer::{StereoFrame, Visualizer};

const BACKGROUND: u32 = 0x0010_1018;
const LINE: u32 = 0x0033_ccff;

#[derive(Default)]
pub struct Waveform {
    /// Mono samples (L/R averaged), most recent at the back. Trimmed to the
    /// window width each frame, since one column plots one sample.
    history: VecDeque<f32>,
}

impl Visualizer for Waveform {
    fn render(&mut self, buffer: &mut [u32], width: usize, height: usize, samples: &[StereoFrame]) {
        for &(left, right) in samples {
            self.history.push_back((left + right) * 0.5);
        }
        while self.history.len() > width {
            self.history.pop_front();
        }

        buffer.fill(BACKGROUND);
        if height < 2 {
            return;
        }

        let mid = (height / 2) as isize;
        let amplitude = height as f32 * 0.45;

        let mut prev_y = mid;
        for (x, &sample) in self.history.iter().enumerate() {
            let y = mid - (sample.clamp(-1.0, 1.0) * amplitude) as isize;
            let y = y.clamp(0, height as isize - 1);
            draw_vline(buffer, width, height, x, prev_y, y);
            prev_y = y;
        }
    }
}

/// Fill in a vertical run between two y positions at column `x`, so the trace
/// stays a continuous line even when consecutive samples jump a lot.
fn draw_vline(buffer: &mut [u32], width: usize, height: usize, x: usize, y0: isize, y1: isize) {
    if x >= width {
        return;
    }
    let (lo, hi) = if y0 <= y1 { (y0, y1) } else { (y1, y0) };
    for y in lo..=hi {
        if (0..height as isize).contains(&y) {
            buffer[y as usize * width + x] = LINE;
        }
    }
}
