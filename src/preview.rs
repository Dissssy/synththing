//! Script previews (docs/SERVER.md): a short looping animation of what a
//! script draws, and a still, for the Online Library and the script picker.
//!
//! The script plays a stretch from the middle of each starter song
//! (`starter::SONGS`) at `WIDTH` x `HEIGHT`, drawn by `offline::Stage` (the
//! synth runs, since scripts react to the sound, but nothing is heard or
//! encoded), as a recording in Auto, so a game that can play itself
//! (`record_auto`) shows itself being played rather than its menu. Each
//! kept frame is scored for how much there is to see (contrast, colours,
//! motion); the best-scoring `FRAMES` in a row become the animation, a
//! sprite sheet `COLUMNS` frames wide, and the best single frame the still.
//!
//! The first frame with anything on it (not all one colour) is written as
//! the still straight away, so a script that stalls later still has one:
//! `synththing preview` runs in a process of its own, stopped by whoever
//! started it after a time limit (the server, for uploads; the app, for the
//! script picker's previews). A script that never draws anything gets no
//! preview at all.
//!
//! `check` is the same run, cut short: a library server refuses an upload
//! that doesn't compile, or errors in its first seconds.
//!
//! A song's preview (`render_song`) is drawn by the bundled keyboard
//! visualizer (falling notes), the same for every song: `SONG_SECONDS` from
//! where the server found it busiest, every kept frame in order
//! (`SONG_FRAMES`), so the app's silent snippet of the same stretch plays
//! in step with it.

use std::io::Cursor;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::lua_visualizer::{PlaybackRequest, RecordKind, RecordPhase, RecordingTake, SettingValue};
use crate::offline::Stage;
use crate::starter::SONGS;

/// A frame's size.
pub const WIDTH: usize = 320;
pub const HEIGHT: usize = 180;
/// The animation: this many frames, shown at `FPS`.
pub const FRAMES: usize = 48;
pub const FPS: f64 = 12.0;
/// Frames per row in the sprite sheet.
pub const COLUMNS: usize = 8;
/// The still and the sheet, as written into the output folder.
pub const STILL: &str = "preview.png";
pub const SHEET: &str = "preview-sheet.png";

/// The script runs at this many frames a second (its own motion stays
/// smooth), and every `KEEP_EVERY`th frame is kept: `FPS`.
const RUN_FPS: f64 = 24.0;
const KEEP_EVERY: u32 = 2;
/// Seconds of each song kept, after `SETTLE` seconds of letting the
/// script get going.
const SECONDS_PER_SONG: f64 = 12.0;
const SETTLE: f64 = 1.0;
/// How long a script with `record_prepare` may get ready for each song.
const PREPARE_LIMIT: f64 = 30.0;
/// How much of the first song `check` runs (preparing included).
const CHECK_SECONDS: f64 = 4.0;
/// A song's preview: this long from its `preview_start`, all of it kept.
pub const SONG_SECONDS: f64 = crate::library::songs::PREVIEW_SECONDS;
pub const SONG_FRAMES: usize = (SONG_SECONDS * FPS) as usize;
/// What draws songs' previews (bundled).
pub const SONG_SCRIPT: &str = "keyboard.lua";
/// The start of the error for a script that doesn't run.
pub const SCRIPT_ERROR: &str = "the script has an error: ";
/// The most memory a script may use while its preview is made.
pub const MEMORY_LIMIT: usize = 512 * 1024 * 1024;

/// Run `synththing preview` (this program) on `script` in a process of its
/// own, writing into `out` (or only checking it runs, `--check`), stopped
/// after `limit`. Whatever it wrote by then stays (the still of its first
/// frame, at least, unless it couldn't run). A script's own error comes
/// back starting with `SCRIPT_ERROR`.
pub fn run_process(script: &Path, soundfont: &Path, out: &Path, limit: Duration, check: bool) -> Result<(), String> {
    spawn(script, soundfont, out, limit, if check { vec!["--check".into()] } else { Vec::new() })
}

/// As `run_process`, for a song's preview (`render_song`) from `start`.
#[cfg_attr(test, allow(dead_code))]
pub fn run_song_process(song: &Path, soundfont: &Path, out: &Path, limit: Duration, start: f64) -> Result<(), String> {
    spawn(song, soundfont, out, limit, vec!["--song-start".into(), start.to_string()])
}

fn spawn(script: &Path, soundfont: &Path, out: &Path, limit: Duration, extra: Vec<String>) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("couldn't find this program: {e}"))?;
    std::fs::create_dir_all(out).map_err(|e| format!("couldn't make {}: {e}", out.display()))?;
    let log = out.join("preview.log");
    let errors = std::fs::File::create(&log).map_err(|e| format!("couldn't write {}: {e}", log.display()))?;
    let mut command = Command::new(exe);
    command
        .arg("preview")
        .arg(script)
        .arg("--out")
        .arg(out)
        .arg("--soundfont")
        .arg(soundfont)
        .args(extra)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(errors);
    // (The same config folder, if it isn't the usual one.)
    if let Some(dir) = crate::config::config_dir_override() {
        command.env(crate::config::CONFIG_DIR_VAR, dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().map_err(|e| format!("couldn't start making it: {e}"))?;
    let started = Instant::now();
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break Ok(()),
            Ok(Some(status)) => {
                let said = std::fs::read_to_string(&log).unwrap_or_default();
                let said =
                    said.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().trim_start_matches("error: ");
                break Err(if said.is_empty() { format!("it stopped ({status})") } else { said.to_string() });
            }
            Ok(None) if started.elapsed() > limit => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!("stopped after {} s", limit.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => break Err(e.to_string()),
        }
    };
    let _ = std::fs::remove_file(&log);
    result
}

/// What was made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Made {
    /// The still and the animation.
    Animated,
    /// Only a still (the script didn't draw long enough to animate).
    Still,
    /// Nothing: it never drew anything (all one colour).
    Nothing,
}

/// Make `script`'s preview in `out` (`STILL`, and `SHEET` if there's an
/// animation), with `soundfont` for the songs. An error means the script
/// doesn't compile, or something couldn't be loaded or written; the still
/// may have been written by then.
pub fn render(script: &Path, soundfont: &Path, out: &Path) -> Result<Made, String> {
    let source =
        std::fs::read_to_string(script).map_err(|e| format!("couldn't read {}: {e}", script.display()))?;
    std::fs::create_dir_all(out).map_err(|e| format!("couldn't make {}: {e}", out.display()))?;
    for name in [STILL, SHEET] {
        let _ = std::fs::remove_file(out.join(name));
    }
    let songs = out.join(".songs");
    std::fs::create_dir_all(&songs).map_err(|e| format!("couldn't make {}: {e}", songs.display()))?;
    for (name, bytes) in SONGS {
        std::fs::write(songs.join(name), bytes).map_err(|e| format!("couldn't write a song: {e}"))?;
    }
    let result = render_songs(source, script, soundfont, Some(out), &songs);
    let _ = std::fs::remove_dir_all(&songs);
    result
}

/// Check that `script` runs: it compiles, and plays a few seconds of a
/// song without an error (`SCRIPT_ERROR` and the error, if not). `work` is
/// a folder for the song.
pub fn check(script: &Path, soundfont: &Path, work: &Path) -> Result<(), String> {
    let source =
        std::fs::read_to_string(script).map_err(|e| format!("couldn't read {}: {e}", script.display()))?;
    let songs = work.join(".songs");
    std::fs::create_dir_all(&songs).map_err(|e| format!("couldn't make {}: {e}", songs.display()))?;
    let (name, bytes) = SONGS[0];
    std::fs::write(songs.join(name), bytes).map_err(|e| format!("couldn't write a song: {e}"))?;
    let result = render_songs(source, script, soundfont, None, &songs);
    let _ = std::fs::remove_dir_all(&songs);
    result.map(|_| ())
}

/// Make `song`'s preview (a MIDI file) in `out`, from `start` seconds:
/// `SHEET` (`SONG_FRAMES` frames; if the song ends sooner, its last frame
/// fills the rest) and the middle frame as `STILL`.
pub fn render_song(song: &Path, soundfont: &Path, out: &Path, start: f64) -> Result<Made, String> {
    std::fs::create_dir_all(out).map_err(|e| format!("couldn't make {}: {e}", out.display()))?;
    for name in [STILL, SHEET] {
        let _ = std::fs::remove_file(out.join(name));
    }
    let source = crate::lua_visualizer::bundled_default(SONG_SCRIPT).ok_or("the keyboard visualizer isn't bundled")?;
    let mut stage = Stage::new(source.to_string(), None, WIDTH, HEIGHT, RUN_FPS);
    stage.visualizer.detach_store();
    stage.visualizer.set_setting("play_along", SettingValue::Bool(false));
    stage.load_song(song, Some(soundfont), &[], false)?;
    stage.engine.seek(start.max(0.0));
    let mut frames: Vec<Vec<u32>> = Vec::with_capacity(SONG_FRAMES);
    let mut count = 0u32;
    while frames.len() < SONG_FRAMES {
        let (_, view) = stage.frame();
        if let Some(error) = stage.visualizer.error() {
            return Err(format!("the keyboard visualizer failed: {error}"));
        }
        if count.is_multiple_of(KEEP_EVERY) {
            frames.push(stage.pixels.clone());
        }
        count += 1;
        if view.finished {
            break;
        }
    }
    let last = frames.last().cloned().ok_or("it drew nothing")?;
    frames.resize(SONG_FRAMES, last);
    write_png(&out.join(STILL), &frames[SONG_FRAMES / 2], WIDTH, HEIGHT)?;
    let cells: Vec<&Vec<u32>> = frames.iter().collect();
    write_png(&out.join(SHEET), &grid(&cells), WIDTH * COLUMNS, HEIGHT * SONG_FRAMES.div_ceil(COLUMNS))?;
    Ok(Made::Animated)
}

/// The preview's frames, written into `out`; or, without one, the check.
fn render_songs(source: String, script: &Path, soundfont: &Path, out: Option<&Path>, songs: &Path) -> Result<Made, String> {
    let check = out.is_none();
    let mut stage = Stage::new(source, Some(script.to_path_buf()), WIDTH, HEIGHT, RUN_FPS);
    stage.visualizer.detach_store();
    if let Some(error) = stage.visualizer.error() {
        return Err(format!("{SCRIPT_ERROR}{error}"));
    }
    let options = stage.visualizer.options();
    let prepared = options.record_prepare;
    let dt = 1.0 / RUN_FPS;
    let mut fallback = false;
    let mut best_still: Option<(f32, Vec<u32>)> = None;
    let mut best_run: Option<(f32, Vec<Vec<u32>>)> = None;

    let songs_to_play = if check { &SONGS[..1] } else { &SONGS[..] };
    let mut checked = 0.0;
    'songs: for (name, _) in songs_to_play {
        stage.load_song(&songs.join(name), Some(soundfont), &[], prepared)?;
        let mut take = RecordingTake {
            phase: if prepared { RecordPhase::Preparing } else { RecordPhase::Recording },
            kind: RecordKind::Song,
            auto: options.record_auto,
        };
        stage.visualizer.set_recording(!prepared, Some(take));
        // From the middle (a prepared script starts its song itself).
        if !prepared {
            let length = stage.engine.view().length;
            stage.engine.seek(((length - SECONDS_PER_SONG) / 2.0).max(0.0));
        }
        let (mut waited, mut ran, mut count) = (0.0, 0.0, 0u32);
        let mut frames: Vec<Vec<u32>> = Vec::new();
        let mut scores: Vec<f32> = Vec::new();
        loop {
            let recording = take.phase == RecordPhase::Recording;
            let (_, view) = stage.frame();
            if let Some(error) = stage.visualizer.error() {
                if check {
                    return Err(format!("{SCRIPT_ERROR}{error}"));
                }
                // Broken from here on (stopped by the watchdog, say): what
                // it drew so far is all there is.
                break 'songs;
            }
            checked += dt;
            if check {
                if checked >= CHECK_SECONDS {
                    return Ok(Made::Still);
                }
            } else if let Some(out) = out
                && !fallback
                && score(&stage.pixels, None) > 0.0
            {
                write_png(&out.join(STILL), &stage.pixels, WIDTH, HEIGHT)?;
                fallback = true;
            }
            let mut done = false;
            for request in stage.requests() {
                match request {
                    PlaybackRequest::RecordingReady if take.phase == RecordPhase::Preparing => {
                        take.phase = RecordPhase::Recording;
                        stage.visualizer.set_recording(true, Some(take));
                    }
                    PlaybackRequest::RecordingDone if recording => done = true,
                    other => {
                        stage.transport(other);
                    }
                }
            }
            if !recording {
                waited += dt;
                if waited > PREPARE_LIMIT {
                    take.phase = RecordPhase::Recording;
                    stage.visualizer.set_recording(true, Some(take));
                }
                continue;
            }
            ran += dt;
            count += 1;
            if ran > SETTLE && count.is_multiple_of(KEEP_EVERY) {
                let score = score(&stage.pixels, frames.last().map(Vec::as_slice));
                if best_still.as_ref().is_none_or(|(best, _)| score > *best) {
                    best_still = Some((score, stage.pixels.clone()));
                }
                frames.push(stage.pixels.clone());
                scores.push(score);
            }
            if done || view.finished || ran >= SETTLE + SECONDS_PER_SONG {
                break;
            }
        }
        if let Some((score, start)) = best_window(&scores, FRAMES)
            && best_run.as_ref().is_none_or(|(best, _)| score > *best)
        {
            let end = (start + FRAMES).min(frames.len());
            best_run = Some((score, frames.drain(start..end).collect()));
        }
    }

    let Some(out) = out else { return Ok(Made::Still) };
    // (Nothing to see in any of them: the first frame that had something
    // stays, if one did.)
    if let Some((score, still)) = &best_still
        && *score > 0.0
    {
        write_png(&out.join(STILL), still, WIDTH, HEIGHT)?;
    }
    match best_run {
        Some((score, frames)) if frames.len() > 1 && score > 0.0 => {
            let sheet = sheet(&frames);
            write_png(&out.join(SHEET), &sheet, WIDTH * COLUMNS, HEIGHT * FRAMES / COLUMNS)?;
            Ok(Made::Animated)
        }
        _ if out.join(STILL).exists() => Ok(Made::Still),
        _ => Ok(Made::Nothing),
    }
}

/// How much there is to see in a frame: its contrast, how many colours it
/// has, and how much it changed since `previous`. A blank frame (one
/// colour, and the same as before) scores 0.
fn score(pixels: &[u32], previous: Option<&[u32]>) -> f32 {
    let luma = |p: u32| f64::from(((p >> 16) & 0xff) * 77 + ((p >> 8) & 0xff) * 150 + (p & 0xff) * 29) / 256.0;
    let mut colours = [0u64; 64];
    let (mut sum, mut squares, mut moved, mut n) = (0.0, 0.0, 0.0, 0.0);
    for y in (0..HEIGHT).step_by(2) {
        for x in (0..WIDTH).step_by(2) {
            let i = y * WIDTH + x;
            let p = pixels[i];
            let l = luma(p);
            sum += l;
            squares += l * l;
            n += 1.0;
            // 4 bits a channel.
            let bin = (((p >> 20) & 0xf) << 8 | ((p >> 12) & 0xf) << 4 | ((p >> 4) & 0xf)) as usize;
            colours[bin / 64] |= 1 << (bin % 64);
            if let Some(previous) = previous {
                moved += (l - luma(previous[i])).abs();
            }
        }
    }
    let mean = sum / n;
    let spread = (squares / n - mean * mean).max(0.0).sqrt();
    if spread < 2.0 && moved / n < 1.0 {
        return 0.0;
    }
    let distinct: u32 = colours.iter().map(|c| c.count_ones()).sum();
    let contrast = (spread / 64.0).min(1.0);
    let variety = (f64::from(distinct).ln() / 256f64.ln()).clamp(0.0, 1.0);
    let motion = (moved / n / 24.0).min(1.0);
    (contrast + 0.5 * variety + motion) as f32
}

/// The `len` scores in a row with the highest total (fewer, if there
/// aren't that many): (the total, where they start).
fn best_window(scores: &[f32], len: usize) -> Option<(f32, usize)> {
    if scores.is_empty() {
        return None;
    }
    let len = len.min(scores.len());
    let mut total: f32 = scores[..len].iter().sum();
    let mut best = (total, 0);
    for start in 1..=scores.len() - len {
        total += scores[start + len - 1] - scores[start - 1];
        if total > best.0 {
            best = (total, start);
        }
    }
    Some(best)
}

/// `frames` as a sprite sheet: `FRAMES` of them, `COLUMNS` to a row. Too
/// few are played forward then back to fill it, so it still loops smoothly.
fn sheet(frames: &[Vec<u32>]) -> Vec<u32> {
    let order: Vec<usize> = if frames.len() >= FRAMES {
        (0..FRAMES).collect()
    } else {
        let there_and_back: Vec<usize> = (0..frames.len()).chain((1..frames.len() - 1).rev()).collect();
        there_and_back.iter().copied().cycle().take(FRAMES).collect()
    };
    let cells: Vec<&Vec<u32>> = order.iter().map(|&i| &frames[i]).collect();
    grid(&cells)
}

/// Frames as a sprite sheet, `COLUMNS` to a row.
fn grid(frames: &[&Vec<u32>]) -> Vec<u32> {
    let width = WIDTH * COLUMNS;
    let mut sheet = vec![0u32; width * HEIGHT * frames.len().div_ceil(COLUMNS)];
    for (cell, frame) in frames.iter().enumerate() {
        let (cx, cy) = ((cell % COLUMNS) * WIDTH, (cell / COLUMNS) * HEIGHT);
        for y in 0..HEIGHT {
            let row = (cy + y) * width + cx;
            sheet[row..row + WIDTH].copy_from_slice(&frame[y * WIDTH..(y + 1) * WIDTH]);
        }
    }
    sheet
}

/// `pixels` (`0x00RRGGBB`) as a compressed PNG.
pub fn encode_png(pixels: &[u32], width: usize, height: usize) -> Result<Vec<u8>, String> {
    let rgb: Vec<u8> = pixels.iter().flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8]).collect();
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::High);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(&rgb).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    Ok(bytes)
}

fn write_png(path: &Path, pixels: &[u32], width: usize, height: usize) -> Result<(), String> {
    let bytes = encode_png(pixels, width, height).map_err(|e| format!("couldn't make a PNG: {e}"))?;
    // Written whole, then moved into place: whoever's waiting never reads half of one.
    let partial = path.with_extension("part");
    std::fs::write(&partial, bytes)
        .and_then(|()| std::fs::rename(&partial, path))
        .map_err(|e| format!("couldn't write {}: {e}", path.display()))
}

/// A PNG (a preview, or anything else a server sends) as RGBA: width,
/// height, pixels. Images over 8192 pixels a side aren't taken.
pub fn decode_png(bytes: &[u8]) -> Result<(usize, usize, Vec<u8>), String> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let (width, height) = (reader.info().width as usize, reader.info().height as usize);
    if width == 0 || height == 0 || width > 8192 || height > 8192 {
        return Err(format!("a {width} x {height} image isn't a preview"));
    }
    let mut buffer = vec![0; reader.output_buffer_size().ok_or("the image is too big")?];
    let frame = reader.next_frame(&mut buffer).map_err(|e| e.to_string())?;
    let data = &buffer[..frame.buffer_size()];
    // chunks_exact rather than as_chunks, like the rest of the code.
    #[allow(clippy::chunks_exact_to_as_chunks)]
    let rgba = match frame.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => data.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => data.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => data.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return Err("an indexed image wasn't expanded".into()),
    };
    if rgba.len() != width * height * 4 {
        return Err("the image's data is the wrong size".into());
    }
    Ok((width, height, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn soundfont() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/starter/soundfonts/TimGM6mb.sf2")
    }

    #[test]
    fn makes_song_previews() {
        let out = std::env::temp_dir().join(format!("synththing-song-preview-{}", std::process::id()));
        let song = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/starter/songs/Ode to Joy.mid");
        assert_eq!(render_song(&song, &soundfont(), &out, 2.0).unwrap(), Made::Animated);
        let (w, h, sheet) = decode_png(&std::fs::read(out.join(SHEET)).unwrap()).unwrap();
        assert_eq!((w, h), (WIDTH * COLUMNS, HEIGHT * SONG_FRAMES / COLUMNS));
        // Notes fall: the first frame and a later one differ.
        let cell = |i: usize| {
            let (cx, cy) = ((i % COLUMNS) * WIDTH, (i / COLUMNS) * HEIGHT);
            (0..HEIGHT).flat_map(|y| sheet[((cy + y) * w + cx) * 4..((cy + y) * w + cx + WIDTH) * 4].to_vec()).collect::<Vec<u8>>()
        };
        assert_ne!(cell(0), cell(24));
        assert!(out.join(STILL).exists());
        let _ = std::fs::remove_dir_all(&out);
    }

    #[test]
    fn scores_and_windows() {
        let blank = vec![0u32; WIDTH * HEIGHT];
        assert_eq!(score(&blank, None), 0.0);
        let stripes: Vec<u32> = (0..WIDTH * HEIGHT).map(|i| if (i / 4) % 2 == 0 { 0xff8000 } else { 0x0020ff }).collect();
        let still = score(&stripes, Some(&stripes));
        assert!(still > 0.5, "{still}");
        // Motion counts.
        assert!(score(&stripes, Some(&blank)) > still);
        assert_eq!(best_window(&[0.0, 1.0, 5.0, 5.0, 0.0], 2), Some((10.0, 2)));
        assert_eq!(best_window(&[1.0, 2.0], 5), Some((3.0, 0)));
        assert_eq!(best_window(&[], 5), None);
    }

    #[test]
    fn png_round_trip() {
        let pixels: Vec<u32> = (0..12u32).map(|i| i * 0x10_2030).collect();
        let (w, h, rgba) = decode_png(&encode_png(&pixels, 4, 3).unwrap()).unwrap();
        assert_eq!((w, h), (4, 3));
        assert_eq!(&rgba[4..8], &[0x10, 0x20, 0x30, 255]);
    }

    /// A moving script gets an animation; one that never draws anything
    /// still gets its (blank) first frame; one that doesn't compile, nothing.
    #[test]
    fn makes_previews() {
        let dir = std::env::temp_dir().join(format!("synththing-preview-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("moving.lua");
        std::fs::write(
            &script,
            "function render(w, h) clear({ r = 0, g = 0, b = 0 }) local x = (FRAME * 7) % w \
             rect(x, 20, x + 40, 60, { r = 255, g = 120, b = 0 }) end",
        )
        .unwrap();
        let out = dir.join("moving");
        assert_eq!(render(&script, &soundfont(), &out), Ok(Made::Animated));
        let (w, h, _) = decode_png(&std::fs::read(out.join(SHEET)).unwrap()).unwrap();
        assert_eq!((w, h), (WIDTH * COLUMNS, HEIGHT * FRAMES / COLUMNS));
        assert!(out.join(STILL).exists() && !out.join(".songs").exists());

        // Ends every take straight away: the first frame is all there is.
        std::fs::write(&script, "function render(w, h) rect(0, 0, 9, 9, { r = 255, g = 0, b = 0 }) recording_done() end")
            .unwrap();
        let out = dir.join("quitter");
        assert_eq!(render(&script, &soundfont(), &out), Ok(Made::Still));
        assert!(out.join(STILL).exists() && !out.join(SHEET).exists());

        // Never draws anything: no preview.
        std::fs::write(&script, "function render() clear({ r = 0, g = 0, b = 0 }) end").unwrap();
        let out = dir.join("blank");
        assert_eq!(render(&script, &soundfont(), &out), Ok(Made::Nothing));
        assert!(!out.join(STILL).exists());
        assert_eq!(check(&script, &soundfont(), &out), Ok(()));

        // Broken after a few frames: its first frame, not black ones; and
        // it doesn't pass the check.
        std::fs::write(
            &script,
            "function render(w, h) clear({ r = 0, g = 80, b = 160 }) rect(0, 0, 20, 20, { r = 255, g = 255, b = 255 }) \
             if FRAME > 3 then error('oops') end end",
        )
        .unwrap();
        let out = dir.join("broken-later");
        assert_eq!(render(&script, &soundfont(), &out), Ok(Made::Still));
        let (_, _, first) = decode_png(&std::fs::read(out.join(STILL)).unwrap()).unwrap();
        assert_eq!(&first[(30 * WIDTH + 30) * 4..][..4], &[0, 80, 160, 255]);
        let failed = check(&script, &soundfont(), &out).unwrap_err();
        assert!(failed.starts_with(SCRIPT_ERROR) && failed.contains("oops"), "{failed}");

        std::fs::write(&script, "function render( end").unwrap();
        assert!(check(&script, &soundfont(), &dir).unwrap_err().starts_with(SCRIPT_ERROR));
        assert!(render(&script, &soundfont(), &dir.join("broken")).is_err());
        assert!(!dir.join("broken").join(STILL).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
