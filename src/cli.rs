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

    /// Also print the app's log (Help > Log...) to the terminal as it
    /// happens: script errors, warnings, panics. For watching a release
    /// build run.
    #[arg(long)]
    pub console: bool,

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

    /// Run a script library server (docs/SERVER.md).
    ///
    /// Serves the library's API over HTTP until stopped (Ctrl+C). Its data
    /// (settings in server.json, its key, the database) lives in the --data
    /// folder, made on first run with default settings to edit. For the
    /// internet, run it behind a reverse proxy for HTTPS (Caddy, nginx) and
    /// set behind_proxy in server.json.
    Serve(ServeArgs),

    /// Make a script's preview: a short looping animation and a still.
    ///
    /// The script plays a stretch of each starter song at 320x180, in Auto
    /// if it can play itself, and the liveliest frames are kept: the still
    /// (preview.png) and the animation (preview-sheet.png, a sprite sheet
    /// of 48 frames, 8 to a row, played at 12 per second). The first frame
    /// with anything on it is written as the still straight away; a script
    /// that never draws anything gets none. Library servers make these for
    /// every upload; the app makes them for the script picker. With
    /// --check, only checks it runs (what a library server does before
    /// taking an upload). Exit code 0 if it made one (or checked out), 1 if
    /// the script has an error, 2 if something couldn't be loaded.
    Preview(PreviewArgs),

    /// Moderate a script library server, from the machine it runs on.
    ///
    /// Talks to the running server (at server.json's address, or --server),
    /// signed with the server's own key from its data folder, so it works
    /// over SSH, under systemd and in Docker (docker exec). Admins can do
    /// the same from the app's Library tab.
    Admin(AdminArgs),

    /// Move the official publisher to a new key (if the old one got out).
    ///
    /// Reads the current secret key from SYNTHTHING_PUBLISH_KEY, has the
    /// identity authority (the official server) rotate it to a new key,
    /// and prints the new secret key, and nothing else, to stdout, for
    /// `gh secret set SYNTHTHING_PUBLISH_KEY`. The official scripts move to
    /// the new key, the old one stops working, and apps follow along.
    PublisherRotate {
        /// The authority's address.
        #[arg(long, default_value = "https://synththing.p51.nl")]
        server: String,
    },

    /// Publish the scripts bundled with this build to a library server.
    ///
    /// For CI: each visualizer, game and example (not the templates) is
    /// uploaded by its slug (its file name), signed with the secret key in
    /// the SYNTHTHING_PUBLISH_KEY environment variable; a changed one
    /// becomes a new version, an unchanged one stays as it is.
    PublishBundled {
        /// The server's address.
        #[arg(long, default_value = "https://synththing.p51.nl")]
        server: String,

        /// Publish the scripts in this folder (laid out like
        /// assets/visualizers) instead of the ones built in. A script using
        /// something this build doesn't know is skipped.
        #[arg(long, value_name = "DIR")]
        dir: Option<PathBuf>,
    },
}

#[derive(Args, Debug)]
pub struct ServeArgs {
    /// The folder the server keeps its data in. Defaults to "server" in
    /// the app's config folder.
    #[arg(long, value_name = "DIR")]
    pub data: Option<PathBuf>,

    /// Address and port to listen on, instead of server.json's (default
    /// 127.0.0.1:7381).
    #[arg(long, value_name = "ADDR")]
    pub bind: Option<String>,
}

#[derive(Args, Debug)]
pub struct AdminArgs {
    /// The server's data folder (as for serve).
    #[arg(long, value_name = "DIR")]
    pub data: Option<PathBuf>,

    /// The server's address, instead of http:// and server.json's bind.
    #[arg(long, value_name = "URL")]
    pub server: Option<String>,

    #[command(subcommand)]
    pub command: AdminCommand,
}

#[derive(Subcommand, Debug)]
pub enum AdminCommand {
    /// Open reports, newest first (all of them with --all).
    Reports {
        #[arg(long)]
        all: bool,
    },
    /// A script: its uploader's key and addresses, versions, reports.
    Info { script: String },
    /// Hide a script: it's no longer listed or downloadable.
    Hide {
        script: String,
        #[arg(long, default_value = "")]
        reason: String,
    },
    Unhide { script: String },
    /// Delete a script, every version, for good.
    Delete { script: String },
    /// Ban a key (hex, or an ID the server has seen) or an address.
    Ban {
        target: String,
        #[arg(long)]
        reason: String,
        /// For this many hours (for good without).
        #[arg(long)]
        hours: Option<u64>,
        /// Also hide everything the key uploaded.
        #[arg(long)]
        hide_scripts: bool,
        /// Also ban the addresses the key was seen on in the last 30 days,
        /// for 30 days, and any new one it shows up from while banned.
        #[arg(long)]
        ban_addresses: bool,
    },
    Unban { target: String },
    /// The bans in force.
    Bans,
    /// Make a key an admin (hex, or an ID the server has seen).
    AddAdmin { key: String },
    RemoveAdmin { key: String },
    Admins,
    /// Mark a report dealt with.
    Resolve {
        report: i64,
        #[arg(long, default_value = "")]
        note: String,
    },
}

#[derive(Args, Debug)]
pub struct PreviewArgs {
    /// The script (a .lua file).
    pub script: PathBuf,

    /// The folder to write preview.png and preview-sheet.png in.
    #[arg(long, value_name = "DIR", default_value = ".")]
    pub out: PathBuf,

    /// Soundfont for the songs. Defaults to the first one in the app's
    /// Soundfonts list.
    #[arg(long)]
    pub soundfont: Option<PathBuf>,

    /// Only check that it runs: it compiles, and plays a few seconds of a
    /// song without an error. Writes no images.
    #[arg(long)]
    pub check: bool,
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

    /// Record the run to a video (.mp4) with its sound, at --video-size
    /// (instead of --width and --height) and --fps. Needs ffmpeg: on the
    /// PATH, or downloaded by the app (record once from the app).
    #[arg(long, value_name = "FILE")]
    pub video: Option<std::path::PathBuf>,

    /// The video's size: 720p, 1080p, 1440p or 4k; vertical (1080x1920),
    /// 720p-vertical or square (1080x1080); or any WIDTHxHEIGHT.
    #[arg(long, value_name = "SIZE", default_value = "1080p")]
    pub video_size: String,

    /// The video's encoding: standard (plays everywhere), sharp (4:4:4,
    /// exact pixel edges) or lossless.
    #[arg(long, value_name = "QUALITY", default_value = "standard")]
    pub video_quality: String,

    /// With --video: the script plays itself, if it can
    /// (script_options record_auto); recording().mode is "auto".
    #[arg(long, requires = "video")]
    pub auto: bool,
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
        assert!(Cli::try_parse_from(["synththing", "--console"]).unwrap().console);
    }
}
