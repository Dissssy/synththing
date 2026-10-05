//! `synththing run-script`: run a visualizer script without a window, with
//! a song playing into it, and report what happened. The same engine and
//! script code the app uses, driven frame by frame: render `1/fps` seconds
//! of audio, hand it to the script with the current notes and playback
//! state, apply anything the script asked for (pause, seek, channel
//! toggles, notes to play), repeat. With `--video`, every frame and its
//! audio also go to a `Recorder`, the same one the app records with; a
//! script with `record_prepare` gets ready first (those frames aren't
//! recorded or counted), and can end the run with `recording_done()`.
//!
//! Exit codes: 0 ran cleanly, 1 the script had errors, 2 something
//! couldn't be loaded.

use std::path::Path;

use crate::cli::RunScriptArgs;
use crate::config::{nice_name, Config};
use crate::engine::Engine;
use crate::loader::{self, SoundFontProbe};
use crate::lua_visualizer::{LuaVisualizer, PlaybackRequest, RecordKind, RecordPhase, RecordingTake};
use crate::recorder::{Format, Quality, Recorder, Resolution};
use crate::visualizer::{DisplayMode, StereoFrame, Visualizer, VisualizerInput};

const SAMPLE_RATE: u32 = 44_100;

/// How long a script may prepare before recording starts anyway, in
/// seconds of frames.
const PREPARE_LIMIT_SECONDS: f64 = 120.0;

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

/// A prepared recording starts: the script is told, and frames count.
fn start_take(take: &mut Option<RecordingTake>, visualizer: &mut LuaVisualizer) {
    if let Some(t) = take {
        t.phase = RecordPhase::Recording;
        visualizer.set_recording(true, *take);
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
    // A video sets the size; the frame rate has to be a whole number.
    let (width, height) = match &args.video {
        Some(_) => Resolution::parse(&args.video_size)
            .ok_or_else(|| {
                format!(
                    "--video-size {} isn't one of 720p, 1080p, 1440p, 4k, vertical, 720p-vertical, square, \
                     or a WIDTHxHEIGHT from 64 to 4096",
                    args.video_size
                )
            })?
            .size(),
        None => (args.width, args.height),
    };
    let mut recorder = match &args.video {
        Some(path) => {
            if args.fps.fract() != 0.0 || args.fps > 240.0 {
                return Err("--fps must be a whole number (up to 240) for --video".into());
            }
            let ffmpeg = crate::ffmpeg::find()
                .ok_or("--video needs ffmpeg: put it on the PATH, or record once in the app to download it")?;
            let quality = Quality::parse(&args.video_quality).ok_or_else(|| {
                format!("--video-quality {} isn't one of standard, sharp, lossless", args.video_quality)
            })?;
            let format = Format { width, height, fps: args.fps as u32, quality };
            let recorder = Recorder::start(&ffmpeg, format, SAMPLE_RATE, path.clone()).map_err(|e| format!("{e:#}"))?;
            Some(recorder.wait_for_encoder())
        }
        None => None,
    };
    let source = std::fs::read_to_string(&args.script)
        .map_err(|e| format!("couldn't read {}: {e}", args.script.display()))?;

    let mut engine = Engine::new(SAMPLE_RATE);
    let mut visualizer = LuaVisualizer::new(source, Some(args.script.clone()), SAMPLE_RATE);
    visualizer.set_fixed_timestep(Some(1.0 / args.fps));
    // A video is a recording: of the song, or free without one.
    let options = visualizer.options();
    let mut take = args.video.is_some().then(|| RecordingTake {
        phase: if options.record_prepare { RecordPhase::Preparing } else { RecordPhase::Recording },
        kind: if args.song.is_some() { RecordKind::Song } else { RecordKind::Free },
        auto: args.auto && options.record_auto,
    });
    if args.auto && !options.record_auto {
        println!("note: --auto, but the script doesn't play itself (script_options record_auto)");
    }
    visualizer.set_recording(take.is_some_and(|t| t.phase == RecordPhase::Recording), take);
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
    let mut live = vec![0.0f32; samples_per_frame * 2];
    let mut pixels = vec![0u32; width * height];
    let input = VisualizerInput { mode: DisplayMode::Window, ..Default::default() };

    let prepare_limit = (PREPARE_LIMIT_SECONDS * args.fps) as u32;
    let mut prepared_frames = 0u32;
    let mut frame = 0u32;
    while frame < args.frames {
        let preparing = take.is_some_and(|t| t.phase == RecordPhase::Preparing);
        if preparing {
            prepared_frames += 1;
            if prepared_frames > prepare_limit {
                println!("the script didn't call recording_ready() in {PREPARE_LIMIT_SECONDS} s; recording anyway");
                start_take(&mut take, &mut visualizer);
            }
        } else {
            frame += 1;
        }
        let preparing = take.is_some_and(|t| t.phase == RecordPhase::Preparing);
        engine.render_into(&mut audio);
        // Notes the script plays, mixed in as the app's output does.
        if engine.live_busy() {
            engine.render_live(&mut live);
            for (sample, extra) in audio.iter_mut().zip(&live) {
                *sample += extra;
            }
        }
        // chunks_exact(2) rather than as_chunks::<2>(), like the rest of the code.
        #[allow(clippy::chunks_exact_to_as_chunks)]
        let samples: Vec<StereoFrame> = audio.chunks_exact(2).map(|s| (s[0], s[1])).collect();
        let view = engine.view();
        let mut notes = engine.notes_snapshot();
        notes.active.extend(engine.live_notes());
        visualizer.render(&mut pixels, width, height, &samples, &notes, &view, &input);

        if frame == args.frames
            && let Some(path) = &args.screenshot
        {
            std::fs::write(path, crate::png::encode_rgb(&pixels, width, height))
                .map_err(|e| format!("couldn't write {}: {e}", path.display()))?;
            println!("saved the last frame to {}", path.display());
        }
        if let Some(recording) = &mut recorder
            && !preparing
        {
            recording.feed(1.0 / args.fps, &samples, Some(&mut pixels)).map_err(|e| format!("{e:#}"))?;
        }

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
        for command in visualizer.take_live_commands() {
            engine.live_command(command);
        }
        let mut done = false;
        for request in visualizer.take_playback_requests() {
            match request {
                PlaybackRequest::RecordingReady if preparing => start_take(&mut take, &mut visualizer),
                PlaybackRequest::RecordingDone if options.record_prepare && take.is_some() && !preparing => done = true,
                PlaybackRequest::RecordingReady | PlaybackRequest::RecordingDone => {}
                PlaybackRequest::Pause(pause) if pause != engine.view().paused => engine.toggle_pause(),
                PlaybackRequest::Pause(_) => {}
                PlaybackRequest::Seek(seconds) => engine.seek(seconds),
                PlaybackRequest::Speed(speed) => engine.set_speed(speed),
                // No playlist here: nothing to move between.
                PlaybackRequest::PlayTrack(_)
                | PlaybackRequest::NextTrack
                | PlaybackRequest::PreviousTrack
                | PlaybackRequest::Loop(_)
                | PlaybackRequest::Shuffle(_) => {}
            }
        }
        if done {
            println!("frame {frame} ({:.2}s): the script ended the take (recording_done)", engine.view().position);
            break;
        }
    }

    if let Some(recording) = recorder {
        let output = recording.output().to_path_buf();
        let seconds = recording.seconds();
        recording.finish_now().map_err(|e| format!("{e:#}"))?;
        println!("saved a {seconds:.1} s video to {}", output.display());
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
    let perf = visualizer.perf_summary();
    if perf.frames > 0 {
        println!(
            "render() took {:.2} ms on average, {:.2} ms at worst (a 60 fps frame allows {:.1} ms){}",
            perf.overall_avg_ms,
            perf.overall_max_ms,
            1000.0 / 60.0,
            if perf.half_rate { "; the app would drop it to 30 fps" } else { "" }
        );
    }
    let view = engine.view();
    println!(
        "ran {} frames at {} fps, song at {:.2}s of {:.2}s, {} error{}",
        frame,
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
        visualizer.set_song(Some(song), Some(audio.song_id.clone()));
        engine.load_audio_file(audio, name, visualizer.options().start_paused);
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
    engine.load_midi(midi.file, name, visualizer.options().start_paused);
    visualizer.set_note_list(midi.notes);
    visualizer.set_song(Some(song), Some(midi.song_id));
    Ok(())
}
