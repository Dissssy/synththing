//! Command-line interface. Documented for users in docs/cli.md; keep the
//! two in step (`synththing --help` is generated from the doc comments
//! here).
//!
//! Release builds are Windows GUI programs (`windows_subsystem =
//! "windows"`), which start with no console attached, so anything printed
//! would vanish. When there are command-line arguments, `attach_console`
//! re-attaches to the terminal that launched the program before anything
//! is printed.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// A MIDI and audio player with a Lua-scriptable visualizer.
///
/// With no command, opens the app.
#[derive(Parser, Debug)]
#[command(name = "synththing", version, after_help = "Full documentation: docs/cli.md in the repository.")]
pub struct Cli {
    /// Open with the Visualizer tab showing.
    #[arg(long, visible_alias = "viz")]
    pub visualizer: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run a visualizer script without a window, to check it for errors.
    ///
    /// Plays a song into the script frame by frame (the same engine and
    /// script code as the app, with DT exactly 1/fps), then prints any
    /// errors, the script's log, and a summary. Exit code 0 if it ran
    /// cleanly, 1 if the script had errors, 2 if something couldn't be
    /// loaded. The script's saved data (store_set) isn't written unless
    /// --save-data is given.
    RunScript(RunScriptArgs),
}

#[derive(Args, Debug)]
pub struct RunScriptArgs {
    /// The script to run (a .lua file).
    pub script: PathBuf,

    /// A song to play into it: MIDI (.mid, .midi) or audio (.wav, .mp3,
    /// .ogg, .flac, ...). Without one, the script runs with no song.
    #[arg(long, short)]
    pub song: Option<PathBuf>,

    /// Soundfont for a MIDI song. Defaults to the first one in the app's
    /// Soundfonts list.
    #[arg(long)]
    pub soundfont: Option<PathBuf>,

    /// How many frames to run.
    #[arg(long, short, default_value_t = 600)]
    pub frames: u32,

    /// Frames per second (sets how much song time passes per frame, and DT).
    #[arg(long, default_value_t = 60.0)]
    pub fps: f64,

    /// Visualizer width in pixels.
    #[arg(long, default_value_t = 640)]
    pub width: usize,

    /// Visualizer height in pixels.
    #[arg(long, default_value_t = 360)]
    pub height: usize,

    /// Start this many seconds into the song.
    #[arg(long, default_value_t = 0.0)]
    pub start: f64,

    /// Let the script save its data (store_set) to its .store.json file.
    /// Off by default, so test runs don't overwrite real saved data.
    #[arg(long)]
    pub save_data: bool,

    /// Only print errors and the summary, not the script's log.
    #[arg(long, short)]
    pub quiet: bool,

    /// Save the last frame as a PNG image (to see what the script drew).
    #[arg(long, value_name = "FILE")]
    pub screenshot: Option<std::path::PathBuf>,
}

/// If launched with arguments from a terminal, attach to it so output and
/// `--help` show up there. Returns whether a console was attached (output
/// then starts on a fresh line, since the shell has already printed its
/// prompt). Does nothing if output is already going somewhere (a debug
/// build's own console, or redirected to a file or pipe).
#[cfg(windows)]
pub fn attach_console() -> bool {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::{AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE};
    // SAFETY: both are plain Win32 calls with no pointer arguments.
    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
            return false;
        }
        AttachConsole(ATTACH_PARENT_PROCESS) != 0
    }
}

#[cfg(not(windows))]
pub fn attach_console() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_run_script() {
        let cli = Cli::try_parse_from(["synththing", "run-script", "a.lua", "--song", "b.mid", "-f", "10"]).unwrap();
        match cli.command {
            Some(Command::RunScript(args)) => {
                assert_eq!(args.script, PathBuf::from("a.lua"));
                assert_eq!(args.song, Some(PathBuf::from("b.mid")));
                assert_eq!(args.frames, 10);
                assert!(!args.save_data);
            }
            other => panic!("{other:?}"),
        }
        assert!(Cli::try_parse_from(["synththing", "--viz"]).unwrap().visualizer);
    }
}
