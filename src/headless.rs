//! `synththing run-script`: run a visualizer script without a window, with
//! a song playing into it, and report what happened. The same engine and
//! script code the app uses, driven frame by frame: render `1/fps` seconds
//! of audio, hand it to the script with the current notes and playback
//! state, apply anything the script asked for (pause, seek, channel
//! toggles), repeat.
//!
//! Exit codes: 0 ran cleanly, 1 the script had errors, 2 something
//! couldn't be loaded.

use std::path::Path;

use crate::cli::RunScriptArgs;
use crate::config::{nice_name, Config};
use crate::engine::Engine;
use crate::loader::{self, SoundFontProbe};
use crate::lua_visualizer::{LuaVisualizer, PlaybackRequest};
use crate::visualizer::{DisplayMode, StereoFrame, Visualizer, VisualizerInput};

const SAMPLE_RATE: u32 = 44_100;

pub fn run(args: &RunScriptArgs, config: &Config) -> i32 {
    match run_inner(args, config) {
        Ok(0) => 0,
        Ok(_) => 1,
        Err(message) => {
            eprintln!("error: {message}");
            2
        }
    }
}

/// Returns how many distinct script errors happened.
fn run_inner(args: &RunScriptArgs, config: &Config) -> Result<usize, String> {
    if !(args.fps.is_finite() && args.fps > 0.0) {
        return Err("--fps must be a positive number".into());
    }
    if args.width == 0 || args.height == 0 {
        return Err("--width and --height must be at least 1".into());
    }
    let source = std::fs::read_to_string(&args.script)
        .map_err(|e| format!("couldn't read {}: {e}", args.script.display()))?;

    let mut engine = Engine::new(SAMPLE_RATE);
    let mut visualizer = LuaVisualizer::new(source, Some(args.script.clone()), SAMPLE_RATE);
    visualizer.set_fixed_timestep(Some(1.0 / args.fps));
    if !args.save_data {
        visualizer.detach_store();
    }
    if let Some(song) = &args.song {
        load_song(song, args, config, &mut engine, &mut visualizer)?;
    }
    if args.start > 0.0 {
        engine.seek(args.start);
    }

    let mut errors = 0usize;
    let mut last_error: Option<String> = None;
    if let Some(error) = visualizer.error() {
        println!("compile error: {error}");
        return Ok(1);
    }

    let samples_per_frame = (f64::from(SAMPLE_RATE) / args.fps).round().max(1.0) as usize;
    let mut audio = vec![0.0f32; samples_per_frame * 2];
    let mut pixels = vec![0u32; args.width * args.height];
    let input = VisualizerInput { mode: DisplayMode::Window, ..Default::default() };

    for frame in 1..=args.frames {
        engine.render_into(&mut audio);
        // chunks_exact(2) rather than as_chunks::<2>(), like the rest of the code.
        #[allow(clippy::chunks_exact_to_as_chunks)]
        let samples: Vec<StereoFrame> = audio.chunks_exact(2).map(|s| (s[0], s[1])).collect();
        let view = engine.view();
        visualizer.render(&mut pixels, args.width, args.height, &samples, &engine.notes_snapshot(), &view, &input);

        if let Some(error) = visualizer.error()
            && last_error.as_deref() != Some(error)
        {
            println!("frame {frame} ({:.2}s): error: {error}", view.position);
            errors += 1;
            last_error = Some(error.to_string());
        } else if visualizer.error().is_none() {
            last_error = None;
        }

        for (channel, enabled) in visualizer.take_channel_requests() {
            engine.set_channel_enabled(channel, enabled);
        }
        for request in visualizer.take_playback_requests() {
            match request {
                PlaybackRequest::Pause(pause) if pause != engine.view().paused => engine.toggle_pause(),
                PlaybackRequest::Pause(_) => {}
                PlaybackRequest::Seek(seconds) => engine.seek(seconds),
            }
        }
    }

    if !args.quiet {
        for entry in visualizer.log_entries() {
            if entry.count > 1 {
                println!("log: {} (x{})", entry.message, entry.count);
            } else {
                println!("log: {}", entry.message);
            }
        }
    }
    let view = engine.view();
    println!(
        "ran {} frames at {} fps, song at {:.2}s of {:.2}s, {} error{}",
        args.frames,
        args.fps,
        view.position,
        view.length,
        errors,
        if errors == 1 { "" } else { "s" }
    );
    Ok(errors)
}

fn load_song(
    song: &Path,
    args: &RunScriptArgs,
    config: &Config,
    engine: &mut Engine,
    visualizer: &mut LuaVisualizer,
) -> Result<(), String> {
    let name = nice_name(song);
    let is_midi = song
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("mid") || e.eq_ignore_ascii_case("midi"));
    if !is_midi {
        let audio = loader::load_audio_file(song).map_err(|e| format!("couldn't read {}: {e}", song.display()))?;
        engine.load_audio_file(audio, name);
        return Ok(());
    }

    let soundfont = args
        .soundfont
        .clone()
        .or_else(|| config.soundfonts.iter().find(|p| p.exists()).cloned())
        .ok_or("a MIDI song needs a soundfont: pass --soundfont, or add one in the app")?;
    match loader::probe_soundfont(&soundfont) {
        Ok(SoundFontProbe::Playable(sf)) => engine
            .set_soundfont(sf, nice_name(&soundfont))
            .map_err(|e| format!("couldn't use {}: {e}", soundfont.display()))?,
        Ok(SoundFontProbe::Unplayable(reason)) => return Err(format!("can't use {}: {reason}", soundfont.display())),
        Ok(SoundFontProbe::NotSoundFont) => return Err(format!("{} isn't a soundfont", soundfont.display())),
        Err(e) => return Err(format!("couldn't read {}: {e}", soundfont.display())),
    }
    let midi = loader::load_midi(song).map_err(|e| format!("couldn't read {}: {e}", song.display()))?;
    engine.load_midi(midi.file, name);
    visualizer.set_note_list(midi.notes);
    Ok(())
}
