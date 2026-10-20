//! The slow-frame log: each frame's work timed in named sections and, when
//! a frame runs past its budget, one line in the log saying where the time
//! went.
//!
//! `begin` starts a frame (first thing in `App::logic`), `scope` (or
//! `time`) times a section until it ends, and `end` closes the frame (the
//! end of `App::ui`). eframe only knows the whole frame's time (ours plus
//! egui's tessellation and painting) once that frame is over, so it's the
//! next frame's `report` that decides on and writes the line.
//!
//! Sections nest: each is counted by its own time, less the sections
//! inside it, so the sections and "other" add up to the frame.

use std::cell::RefCell;
use std::time::{Duration, Instant};

/// A frame at 60 fps.
const FRAME_BUDGET: Duration = Duration::from_micros(16_667);
/// One section taking this long is logged even in a frame within budget.
const SECTION_BUDGET: Duration = Duration::from_millis(8);
/// Sections shorter than this are left out of the line, as noise.
const SHOWN: Duration = Duration::from_micros(500);
/// At most one line this often; the frames skipped in between are counted
/// on the next one.
const LINE_GAP: Duration = Duration::from_millis(250);

thread_local! {
    static FRAME: RefCell<Frames> = RefCell::new(Frames::default());
}

#[derive(Default)]
struct Frames {
    /// When the frame underway began, if it did.
    start: Option<Instant>,
    /// Each section's own time so far this frame.
    sections: Vec<(&'static str, Duration)>,
    /// One entry per section open now: the time sections inside it took.
    open: Vec<Duration>,
    /// This frame, once `end` closed it.
    ended: Option<Finished>,
    /// The frame before this one, waiting on `report`.
    previous: Option<Finished>,
    last_line: Option<Instant>,
    skipped: u32,
}

struct Finished {
    /// From `begin` to `end`: our own code, egui's layout included.
    app: Duration,
    sections: Vec<(&'static str, Duration)>,
}

/// Starts a frame. A frame that never got to `end` (nothing was drawn) is
/// dropped unreported.
pub fn begin() {
    FRAME.with_borrow_mut(|f| {
        f.previous = f.ended.take();
        f.start = Some(Instant::now());
        f.sections.clear();
    });
}

/// Closes the frame `begin` started.
pub fn end() {
    FRAME.with_borrow_mut(|f| {
        if let Some(start) = f.start.take() {
            f.ended = Some(Finished {
                app: start.elapsed(),
                sections: std::mem::take(&mut f.sections),
            });
        }
    });
}

/// Times a section of the frame until the returned guard drops.
pub fn scope(name: &'static str) -> Scope {
    FRAME.with_borrow_mut(|f| f.open.push(Duration::ZERO));
    Scope {
        name,
        start: Instant::now(),
    }
}

/// Times `run` as a section of the frame.
pub fn time<T>(name: &'static str, run: impl FnOnce() -> T) -> T {
    let _scope = scope(name);
    run()
}

pub struct Scope {
    name: &'static str,
    start: Instant,
}

impl Drop for Scope {
    fn drop(&mut self) {
        let elapsed = self.start.elapsed();
        FRAME.with_borrow_mut(|f| {
            let inner = f.open.pop().unwrap_or_default();
            if let Some(parent) = f.open.last_mut() {
                *parent += elapsed;
            }
            let own = elapsed.saturating_sub(inner);
            match f.sections.iter_mut().find(|(name, _)| *name == self.name) {
                Some((_, time)) => *time += own,
                None => f.sections.push((self.name, own)),
            }
        });
    }
}

/// Logs the previous frame if it ran over budget. `cpu_usage` is eframe's
/// time for that whole frame (`IntegrationInfo::cpu_usage`), painting
/// included, waiting on vsync not.
pub fn report(cpu_usage: Option<f32>) {
    FRAME.with_borrow_mut(|f| {
        let Some(mut frame) = f.previous.take() else {
            return;
        };
        let total = cpu_usage
            .map_or(frame.app, Duration::from_secs_f32)
            .max(frame.app);
        let slow_section = frame
            .sections
            .iter()
            .any(|(_, time)| *time >= SECTION_BUDGET);
        if total < FRAME_BUDGET && !slow_section {
            return;
        }
        let now = Instant::now();
        if f.last_line.is_some_and(|last| now - last < LINE_GAP) {
            f.skipped += 1;
            return;
        }
        f.last_line = Some(now);

        let timed: Duration = frame.sections.iter().map(|(_, time)| *time).sum();
        frame
            .sections
            .push(("other app code", frame.app.saturating_sub(timed)));
        if cpu_usage.is_some() {
            frame.sections.push(("egui painting", total - frame.app));
        }
        frame.sections.sort_by_key(|(_, time)| std::cmp::Reverse(*time));
        let parts: Vec<String> = frame
            .sections
            .iter()
            .filter(|(_, time)| *time >= SHOWN)
            .map(|(name, time)| format!("{name} {}", ms(*time)))
            .collect();
        let skipped = std::mem::take(&mut f.skipped);
        let skipped = if skipped > 0 {
            format!(" (+{skipped} slow frames not logged)")
        } else {
            String::new()
        };
        log::warn!(
            "slow frame: {} ms (budget {} ms): {}{skipped}",
            ms(total),
            ms(FRAME_BUDGET),
            parts.join(", ")
        );
    });
}

fn ms(time: Duration) -> String {
    format!("{:.1}", time.as_secs_f64() * 1000.0)
}
