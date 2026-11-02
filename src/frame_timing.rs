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
//!
//! Every frame also goes into the last minute's stats (`summary`), which
//! feedback can send along.
//!
//! While the profiler overlay is open (`puffin::are_scopes_on`), each
//! section is a puffin scope too, and `begin` marks puffin's frames.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::{Mutex, Once};
use std::time::{Duration, Instant};

use eframe::egui;

/// A frame at 60 fps.
const FRAME_BUDGET: Duration = Duration::from_micros(16_667);
/// One section taking this long is logged even in a frame within budget.
const SECTION_BUDGET: Duration = Duration::from_millis(8);
/// Sections shorter than this are left out of the line, as noise.
const SHOWN: Duration = Duration::from_micros(500);
/// At most one line this often; the frames skipped in between are counted
/// on the next one.
const LINE_GAP: Duration = Duration::from_millis(250);
/// How far back the stats go.
const STATS_WINDOW: Duration = Duration::from_secs(60);
/// The stats are kept a second at a time, the oldest second going as a
/// new one starts.
const STATS_BUCKET: Duration = Duration::from_secs(1);
/// How late past its due time a frame has to come to be logged as stalled.
const STALL: Duration = Duration::from_secs(1);

/// What the window was last doing (`set_window`), for the stalled-frame
/// and UI-stuck lines: a frame long in coming while the window isn't on
/// screen isn't a hang, just the system not asking for one.
static WINDOW: Mutex<Window> = Mutex::new(Window::Focused);

/// What the window's doing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Window {
    Focused,
    /// Shown, but another window has the keyboard.
    Unfocused,
    Minimized,
    /// Hidden in the tray.
    Hidden,
    /// No window at all: an Android app in the background.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    Background,
}

impl Window {
    fn name(self) -> &'static str {
        match self {
            Window::Focused => "focused",
            Window::Unfocused => "unfocused",
            Window::Minimized => "minimized",
            Window::Hidden => "hidden",
            Window::Background => "background",
        }
    }

    /// On screen, so the system draws the frames the app asks for.
    fn shown(self) -> bool {
        matches!(self, Window::Focused | Window::Unfocused)
    }
}

/// Say what the window's doing now (each frame's logic, and Android's
/// background ticks).
pub fn set_window(window: Window) {
    *WINDOW.lock().unwrap() = window;
}

fn window() -> Window {
    *WINDOW.lock().unwrap()
}

/// How often the watchdog looks at the UI thread.
const WATCH_EVERY: Duration = Duration::from_millis(500);

/// What the UI thread is doing, for the watchdog: a frame that never ends
/// (a hang) is never reported by `begin`, so another thread has to notice.
static WATCH: Mutex<Option<Watch>> = Mutex::new(None);
static WATCHDOG: Once = Once::new();

struct Watch {
    /// Since when it's been at `stage`.
    since: Instant,
    stage: Stage,
    /// The section it last went into, in the app's code.
    section: Option<&'static str>,
    /// How soon the next frame is due, if the app asked for one: without
    /// one, a long wait is just the app idle.
    due: Option<Duration>,
    /// How long it had been stuck at the last line about it.
    logged: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Stage {
    /// Between `begin` and `end`.
    App,
    /// After `end`: egui's painting, the GPU, the window system, or idle.
    Outside,
}

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
    /// When the last frame drawn ended, and how soon the next frame was
    /// due then (`expect_next_within`), if the app asked for one.
    last_end: Option<Instant>,
    due: Option<Duration>,
    last_line: Option<Instant>,
    skipped: u32,
    /// The stats, a second (`STATS_BUCKET`) per entry, by when it began.
    stats: VecDeque<(Instant, Stats)>,
    /// Each section's puffin scope, once it's been profiled on this thread.
    puffin_ids: Vec<(&'static str, puffin::ScopeId)>,
}

/// How a run of frames went.
#[derive(Default)]
struct Stats {
    frames: u32,
    /// Frames over `FRAME_BUDGET`.
    slow: u32,
    total: Duration,
    worst: Duration,
    /// Each section's time over these frames, and its worst in one.
    sections: Vec<(&'static str, Duration, Duration)>,
}

impl Stats {
    fn add_frame(&mut self, total: Duration, sections: &[(&'static str, Duration)]) {
        self.frames += 1;
        self.slow += u32::from(total >= FRAME_BUDGET);
        self.total += total;
        self.worst = self.worst.max(total);
        for &(name, time) in sections {
            self.add_section(name, time, time);
        }
    }

    fn add_section(&mut self, name: &'static str, time: Duration, worst: Duration) {
        match self.sections.iter_mut().find(|(n, ..)| *n == name) {
            Some((_, sum, most)) => {
                *sum += time;
                *most = (*most).max(worst);
            }
            None => self.sections.push((name, time, worst)),
        }
    }

    fn add(&mut self, other: &Stats) {
        self.frames += other.frames;
        self.slow += other.slow;
        self.total += other.total;
        self.worst = self.worst.max(other.worst);
        for &(name, time, worst) in &other.sections {
            self.add_section(name, time, worst);
        }
    }
}

struct Finished {
    /// From `begin` to `end`: our own code, egui's layout included.
    app: Duration,
    sections: Vec<(&'static str, Duration)>,
}

/// Starts a frame. A frame that never got to `end` (nothing was drawn) is
/// dropped unreported. One that comes well after it was due (the app
/// asked for it, `expect_next_within`) is logged as stalled: that time
/// went outside the app's code, to egui's painting, the GPU or the window
/// system, which the slow-frame line doesn't see. `size` is the window's
/// size, for the line.
pub fn begin(size: Option<egui::Vec2>) {
    puffin::GlobalProfiler::lock().new_frame();
    FRAME.with_borrow_mut(|f| {
        let now = Instant::now();
        if let (Some(end), Some(due)) = (f.last_end.take(), f.due.take()) {
            let gap = now - end;
            // (What the window was doing in the gap: the frame's logic
            // hasn't said what it's doing now yet. Off screen, the gap is
            // the system not drawing, not a stall.)
            let window = window();
            if gap > due + STALL && window.shown() {
                let size = size.map_or("unknown".to_string(), |w| format!("{:.0}x{:.0} pt", w.x, w.y));
                let window = window.name();
                log::warn!(
                    window;
                    "stalled frame: {} ms after the last one, due within {} ms; window {size}, {window}",
                    ms(gap),
                    ms(due)
                );
            }
        }
        f.previous = f.ended.take();
        f.start = Some(now);
        f.sections.clear();
    });
    WATCHDOG.call_once(|| {
        let spawned = std::thread::Builder::new().name("ui watchdog".into()).spawn(watchdog);
        if let Err(e) = spawned {
            log::warn!("couldn't start the UI watchdog: {e}");
        }
    });
    // (The frame that was due is due still: a hang in it counts, till
    // `logic_done` says whether the next is.)
    let mut watch = WATCH.lock().unwrap();
    let due = watch.as_ref().and_then(|w| w.due);
    *watch = Some(Watch { since: Instant::now(), stage: Stage::App, section: None, due, logged: Duration::ZERO });
}

/// The app's logic for the frame is done, and has said (or not, by
/// `expect_next_within`) when it wants the next one: the watchdog goes by
/// that from now on. (Called whether or not the frame is drawn, so a
/// hidden window's last tick, wanting nothing, turns the watchdog off.)
pub fn logic_done() {
    let due = FRAME.with_borrow(|f| f.due);
    if let Some(watch) = WATCH.lock().unwrap().as_mut() {
        watch.due = due;
    }
}

/// The app wants the next frame within `time` (it asked egui for a
/// repaint by then), so a frame much later than that is a stall.
pub fn expect_next_within(time: Duration) {
    FRAME.with_borrow_mut(|f| f.due = Some(f.due.map_or(time, |due| due.min(time))));
}

/// Closes the frame `begin` started.
pub fn end() {
    if let Some(watch) = WATCH.lock().unwrap().as_mut() {
        (watch.since, watch.stage, watch.logged) = (Instant::now(), Stage::Outside, Duration::ZERO);
    }
    FRAME.with_borrow_mut(|f| {
        f.last_end = Some(Instant::now());
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
    let (puffin_id, in_frame) = FRAME.with_borrow_mut(|f| {
        f.open.push(Duration::ZERO);
        (puffin::are_scopes_on().then(|| f.puffin_id(name)), f.start.is_some())
    });
    if in_frame && let Some(watch) = WATCH.lock().unwrap().as_mut() {
        watch.section = Some(name);
    }
    Scope {
        name,
        start: Instant::now(),
        _puffin: puffin_id.map(|id| puffin::ProfilerScope::new(id, "")),
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
    /// Ends the puffin scope after this one (fields drop after `drop`).
    _puffin: Option<puffin::ProfilerScope>,
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
        let timed: Duration = frame.sections.iter().map(|(_, time)| *time).sum();
        frame
            .sections
            .push(("other app code", frame.app.saturating_sub(timed)));
        if cpu_usage.is_some() {
            frame.sections.push(("egui painting", total - frame.app));
        }
        let now = Instant::now();
        f.record(now, total, &frame.sections);
        crate::usage::frame(total, &frame.sections);

        if total < FRAME_BUDGET && !slow_section {
            return;
        }
        if f.last_line.is_some_and(|last| now - last < LINE_GAP) {
            f.skipped += 1;
            return;
        }
        f.last_line = Some(now);

        frame.sections.sort_by_key(|(_, time)| std::cmp::Reverse(*time));
        let parts: Vec<String> = frame
            .sections
            .iter()
            .filter(|(_, time)| *time >= SHOWN)
            .map(|(name, time)| format!("{name} {}", ms(*time)))
            .collect();
        // (Sent with usage data: where the time went, by the section's name
        // in the code.)
        let slowest = frame.sections.first().map_or("none", |(name, _)| *name);
        let skipped = std::mem::take(&mut f.skipped);
        let skipped = if skipped > 0 {
            format!(" (+{skipped} slow frames not logged)")
        } else {
            String::new()
        };
        log::warn!(
            slowest;
            "slow frame: {} ms (budget {} ms): {}{skipped}",
            ms(total),
            ms(FRAME_BUDGET),
            parts.join(", ")
        );
    });
}

impl Frames {
    /// The section's puffin scope, registered the first time it's needed.
    /// (puffin's macros take one name per call site; ours come from the
    /// caller.)
    fn puffin_id(&mut self, name: &'static str) -> puffin::ScopeId {
        if let Some((_, id)) = self.puffin_ids.iter().find(|(n, _)| *n == name) {
            return *id;
        }
        let id = puffin::ThreadProfiler::call(|tp| tp.register_named_scope(name, "", "", 0));
        self.puffin_ids.push((name, id));
        id
    }

    fn record(&mut self, now: Instant, total: Duration, sections: &[(&'static str, Duration)]) {
        while self.stats.front().is_some_and(|(start, _)| now - *start > STATS_WINDOW) {
            self.stats.pop_front();
        }
        if !self.stats.back().is_some_and(|(start, _)| now - *start < STATS_BUCKET) {
            self.stats.push_back((now, Stats::default()));
        }
        if let Some((_, stats)) = self.stats.back_mut() {
            stats.add_frame(total, sections);
        }
    }
}

/// The last minute's frames, for a person to read: how many were drawn and
/// how many ran over budget, then the whole frame's and each section's
/// average and worst time, slowest first. None without any frames.
pub fn summary() -> Option<String> {
    FRAME.with_borrow(|f| {
        let now = Instant::now();
        let mut all = Stats::default();
        for (_, stats) in f.stats.iter().filter(|(start, _)| now - *start <= STATS_WINDOW) {
            all.add(stats);
        }
        if all.frames == 0 {
            return None;
        }
        let average = |sum: Duration| sum / all.frames;
        all.sections.sort_by_key(|(_, sum, _)| std::cmp::Reverse(*sum));
        let mut text = format!(
            "{} frames drawn in the last {} s, {} over {} ms\nwhole frame: average {} ms, worst {} ms\n",
            all.frames,
            STATS_WINDOW.as_secs(),
            all.slow,
            ms(FRAME_BUDGET),
            ms(average(all.total)),
            ms(all.worst)
        );
        for (name, sum, worst) in all.sections.iter().filter(|(.., worst)| *worst >= SHOWN) {
            text += &format!("{name}: average {} ms, worst {} ms\n", ms(average(*sum)), ms(*worst));
        }
        Some(text)
    })
}

/// Every `WATCH_EVERY`, logs a UI thread stuck (`Watch::stuck`): where it
/// is, in the app's code (and the section it last went into) or outside
/// it, and what the window was doing.
fn watchdog() {
    loop {
        std::thread::sleep(WATCH_EVERY);
        let window = window();
        let Some(Stuck { stuck, due, stage, section }) = WATCH.lock().unwrap().as_mut().and_then(|w| w.stuck(window, Instant::now())) else {
            continue;
        };
        let (stuck, due, window) = (ms(stuck), ms(due), window.name());
        let section = section.unwrap_or("none");
        match stage {
            Stage::App => log::warn!(
                section, window;
                "UI stuck: no frame for {stuck} ms, due within {due} ms; in the app's code (last in {section}), window {window}"
            ),
            Stage::Outside => log::warn!(
                window;
                "UI stuck: no frame for {stuck} ms, due within {due} ms; outside the app's code \
                 (egui painting, the GPU or the window system), window {window}"
            ),
        }
    }
}

/// The UI thread, stuck: for how long, and where (`Watch`).
#[derive(Debug, PartialEq)]
struct Stuck {
    stuck: Duration,
    due: Duration,
    stage: Stage,
    section: Option<&'static str>,
}

impl Watch {
    /// The UI thread stuck, well past when its next frame was due: again
    /// each time the stuck time doubles. Outside the app's code with the
    /// window off screen, it's only waiting for the system to draw again.
    fn stuck(&mut self, window: Window, now: Instant) -> Option<Stuck> {
        let due = self.due?;
        if self.stage == Stage::Outside && !window.shown() {
            return None;
        }
        let stuck = now - self.since;
        if stuck < due + STALL || stuck < self.logged * 2 {
            return None;
        }
        self.logged = stuck;
        Some(Stuck { stuck, due, stage: self.stage, section: self.section })
    }
}

fn ms(time: Duration) -> String {
    format!("{:.1}", time.as_secs_f64() * 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_covers_each_section() {
        assert!(summary().is_none());
        for _ in 0..3 {
            begin(None);
            time("songs", || std::thread::sleep(Duration::from_millis(2)));
            end();
            // (The next frame reports this one.)
            begin(None);
            report(Some(0.020));
            end();
        }
        let text = summary().unwrap();
        assert!(text.starts_with("3 frames drawn in the last 60 s, 3 over 16.7 ms\n"), "{text}");
        assert!(text.contains("whole frame: average 20.0 ms, worst 20.0 ms"), "{text}");
        // (Sleeps run long on a busy machine: the sections' times vary.)
        assert!(text.contains("\nsongs: average "), "{text}");
        assert!(text.contains("\negui painting: average "), "{text}");
    }

    #[test]
    fn the_watchdog_tells_a_hang_from_idle() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let mut watch = Watch { since: start, stage: Stage::Outside, section: None, due: None, logged: Duration::ZERO };
        assert_eq!(watch.stuck(Window::Focused, at(10_000)), None, "nothing due: idle");
        watch.due = Some(Duration::from_millis(200));
        assert_eq!(watch.stuck(Window::Focused, at(1_000)), None, "not late enough yet");
        let stuck = Stuck { stuck: Duration::from_millis(1_500), due: Duration::from_millis(200), stage: Stage::Outside, section: None };
        assert_eq!(watch.stuck(Window::Focused, at(1_500)), Some(stuck));
        assert_eq!(watch.stuck(Window::Focused, at(2_500)), None, "said once till it doubles");
        assert!(watch.stuck(Window::Focused, at(3_000)).is_some(), "and again once it has");
        (watch.stage, watch.section, watch.logged) = (Stage::App, Some("Songs"), Duration::ZERO);
        let stuck = watch.stuck(Window::Focused, at(3_000)).unwrap();
        assert_eq!((stuck.stage, stuck.section), (Stage::App, Some("Songs")));
    }

    #[test]
    fn the_watchdog_lets_an_off_screen_window_wait() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let due = Some(Duration::from_millis(200));
        let mut watch = Watch { since: start, stage: Stage::Outside, section: None, due, logged: Duration::ZERO };
        for window in [Window::Background, Window::Minimized, Window::Hidden] {
            assert_eq!(watch.stuck(window, at(200_000)), None, "{window:?}: no frames drawn, no hang");
        }
        watch.stage = Stage::App;
        assert!(watch.stuck(Window::Background, at(200_000)).is_some(), "stuck in the app's code is a hang anywhere");
    }
}
