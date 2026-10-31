//! Starting playback and the app, the same on every platform: the engine on
//! its render thread, the audio output it feeds, and the [`App`] wired to
//! both.

use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};

use crate::app::App;
use crate::audio::{AudioEngine, AudioRing, Output, PlaybackShared, Volume};
use crate::audio_stream::Listeners;
use crate::config::Config;
use crate::engine::Engine;
use crate::visualizer::SampleTap;

/// Start playback and make the app, ready to hand to eframe. The audio
/// output has to stay alive for as long as the app runs: drop it after.
pub fn start() -> Result<(App, rodio::MixerDeviceSink)> {
    let sample_rate = crate::audio::SAMPLE_RATE;

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

    let volume = Volume::default();
    // Whoever listens through the local API (`GET /v1/audio/stream`).
    let listeners = Listeners::new();

    // Audio output, a pure consumer of the rings.
    let stream = rodio::DeviceSinkBuilder::open_default_sink()
        .map_err(|e| anyhow!("could not open an audio output device: {e}"))?;
    stream.mixer().add(Output::new(
        ring.clone(),
        Arc::clone(&flush),
        live_ring.clone(),
        volume.clone(),
        Arc::clone(&listeners),
        sample_rate,
    ));

    // Render thread, owns the engine, produces into the ring.
    let audio = AudioEngine::new(
        engine,
        tap.clone(),
        ring,
        live_ring,
        Arc::clone(&flush),
        command_rx,
        Arc::clone(&shared),
        volume,
        sample_rate,
    );
    std::thread::Builder::new()
        .name("synth-render".to_string())
        .spawn(move || audio.run())
        .map_err(|e| anyhow!("could not start the render thread: {e}"))?;

    let ran_before = Config::exists();
    let mut config = Config::load_for_app()?;
    config.soundfonts.sort();

    crate::usage::start(config.usage_data == Some(true));
    let mut app = App::new(command_tx, shared, config, tap, listeners, stream.mixer().clone(), sample_rate);
    app.autoload_soundfont();
    app.check_for_updates_on_launch();
    app.whats_new_on_launch(ran_before);
    if !ran_before {
        app.open_welcome();
    }
    Ok((app, stream))
}
