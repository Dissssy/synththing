#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! synththing, a small GUI MIDI player built on rustysynth + rodio + egui.
//!
//! Play/pause, seek, and playback-speed control for a MIDI file, a Lua-scripted
//! visualizer, and a hot-swappable list of soundfonts retained between runs.
//! Synthesis runs on its own thread (`audio.rs`), rendering ahead into a ring
//! buffer so a dense passage can't stall the audio callback or the GUI.

mod app;
mod changelog;
mod cli;
mod headless;
mod applog;
mod audio;
mod code_edit;
mod config;
mod credits;
mod engine;
mod ffmpeg;
mod filebrowser;
mod gamepad;
mod layout;
mod loader;
mod lua_completion;
mod lua_docs;
mod lua_highlight;
mod live;
mod lua_analysis;
mod lua_visualizer;
mod midi_notes;
mod offline;
mod pixel_font;
mod playlist;
mod png;
mod recorder;
mod song_info;
mod snippets;
mod spectrum;
mod theme;
mod starter;
mod script_host;
mod sprite_code;
mod typing;
mod updater;
mod watch;
mod visualizer;

use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use eframe::egui;

use crate::app::App;
use crate::cli::{Cli, Command};
use clap::Parser;
use crate::audio::{AudioEngine, AudioRing, PlaybackShared, SynthSource};
use crate::config::Config;
use crate::engine::Engine;
use crate::visualizer::SampleTap;

fn main() -> Result<()> {
    // Arguments mean someone ran this from a terminal: attach to it first,
    // so --help, errors and run-script output show up there.
    if std::env::args_os().len() > 1 && cli::attach_console() {
        println!();
    }
    let cli = Cli::parse();

    if let Some(Command::RunScript(args)) = &cli.command {
        let config = Config::load().unwrap_or_default();
        std::process::exit(headless::run(args, &config));
    }

    applog::init(cli.console);
    log::info!("synththing v{} starting", env!("CARGO_PKG_VERSION"));

    let sample_rate = 44_100u32;

    let engine = Engine::new(sample_rate);

    // The visualizer tap: fed the pre-volume signal by the render thread,
    // drained by the GUI each frame. Capacity is in stereo frames (~1 second).
    let tap = SampleTap::new(sample_rate as usize);

    // Ring buffer between the render thread and the audio callback, sized for
    // two seconds so any buffer-slider setting fits with room to spare.
    let ring = AudioRing::new(sample_rate as usize * 2 * 2);
    // The same for notes scripts play (the live synth), mixed in by the
    // output; it only ever holds a few tens of milliseconds.
    let live_ring = AudioRing::new(sample_rate as usize * 2 / 2);
    // The visualizer gets audio as it's heard, not as it's rendered.
    tap.hold_back_unheard(ring.backlog(), live_ring.backlog());
    let flush = Arc::new(AtomicBool::new(false));
    let shared = Arc::new(Mutex::new(PlaybackShared::default()));
    let (command_tx, command_rx) = mpsc::channel();

    // Audio output, a pure consumer of the ring.
    let stream = rodio::DeviceSinkBuilder::open_default_sink()
        .map_err(|e| anyhow!("could not open an audio output device: {e}"))?;
    stream
        .mixer()
        .add(SynthSource::new(ring.clone(), Arc::clone(&flush), sample_rate));
    stream
        .mixer()
        .add(SynthSource::new(live_ring.clone(), Arc::new(AtomicBool::new(false)), sample_rate));

    // Render thread, owns the engine, produces into the ring.
    let audio = AudioEngine::new(
        engine,
        tap.clone(),
        ring,
        live_ring,
        Arc::clone(&flush),
        command_rx,
        Arc::clone(&shared),
        sample_rate,
    );
    std::thread::Builder::new()
        .name("synth-render".to_string())
        .spawn(move || audio.run())
        .map_err(|e| anyhow!("could not start the render thread: {e}"))?;

    let ran_before = Config::exists();
    let mut config = Config::load()?;
    config.soundfonts.sort();

    let mut app = App::new(command_tx, shared, config, tap, sample_rate);
    app.autoload_first_soundfont();
    app.check_for_updates_on_launch();
    app.whats_new_on_launch(ran_before);
    if !ran_before {
        app.open_welcome();
    }
    if cli.visualizer {
        app.set_visualizer_open(true);
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1100.0, 750.0]),
        ..Default::default()
    };
    eframe::run_native("synththing", options, Box::new(|_cc| Ok(Box::new(app))))
        .map_err(|e| anyhow!("failed to run the GUI: {e}"))?;

    // App drops here, closing the command channel; the render thread sees the
    // disconnect and exits. Keep the audio stream alive until then.
    drop(stream);
    Ok(())
}
