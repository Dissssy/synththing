//! synththing — a small TUI MIDI player built on rustysynth + rodio.
//!
//! Play/pause, seek, and playback-speed control for a MIDI file, with a
//! hot-swappable list of soundfonts that is retained between runs.

mod app;
mod config;
mod engine;
mod filebrowser;
mod visualizer;
mod waveform;
mod fft;

use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};

use crate::app::App;
use crate::config::Config;
use crate::engine::{Engine, SynthSource};
use crate::visualizer::SampleTap;

fn main() -> Result<()> {
    let sample_rate = 44_100u32;

    let engine = Arc::new(Mutex::new(Engine::new(sample_rate)));

    // Always tapped (a Mutex push of a few KB every ~23ms is negligible) so
    // the in-TUI 'v' hotkey can open a visualizer window at any time, not
    // just when started with it. `--visualizer`/`--viz` opens it immediately.
    // Capacity is in stereo frames, so this caps the tap at one second.
    let tap = SampleTap::new(sample_rate as usize);

    // Open the default audio device and feed it our synth source. The source
    // never ends; it emits silence until a MIDI file and soundfont are chosen.
    let stream = rodio::DeviceSinkBuilder::open_default_sink()
        .map_err(|e| anyhow!("could not open an audio output device: {e}"))?;
    let source = SynthSource::new(Arc::clone(&engine), sample_rate).with_tap(tap.clone());
    stream.mixer().add(source);

    let mut config = Config::load()?;
    config.soundfonts.sort();
    let mut app = App::new(Arc::clone(&engine), config, tap);
    app.autoload_first_soundfont();
    if std::env::args().any(|a| a == "--visualizer" || a == "--viz") {
        app.set_visualizer_open(true);
    }

    // Make sure a panic mid-render still restores the terminal.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        previous_hook(info);
    }));

    let mut terminal = ratatui::init();
    let result = app.run(&mut terminal);
    ratatui::restore();

    // Keep the audio stream alive for the whole session.
    drop(stream);
    result
}
