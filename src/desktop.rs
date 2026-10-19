//! The desktop app: the command line (`cli.rs`) and its commands, then the
//! window, with what only the desktop has (one app per config folder, the
//! tray, branch builds' own settings).

use anyhow::{anyhow, Result};
use clap::Parser;
use eframe::egui;

use crate::cli::{self, Cli, Command};
use crate::config::{self, Config};
use crate::{app, applog, build_info, channels, headless, instance, library, lua_visualizer, preview, startup};

/// `synththing preview`: 0 made (an animation, or a still), 1 the script
/// has an error, 2 something couldn't be loaded.
fn preview_command(args: &cli::PreviewArgs, config: &Config) -> i32 {
    let Some(soundfont) = args.soundfont.clone().or_else(|| config.soundfonts.iter().find(|p| p.exists()).cloned())
    else {
        eprintln!("error: previews play MIDI songs: pass --soundfont, or add a soundfont in the app");
        return 2;
    };
    lua_visualizer::set_memory_limit(preview::MEMORY_LIMIT);
    if args.check {
        return match preview::check(&args.script, &soundfont, &args.out) {
            Ok(()) => {
                println!("it runs");
                0
            }
            Err(e) => {
                eprintln!("error: {e}");
                if e.starts_with(preview::SCRIPT_ERROR) { 1 } else { 2 }
            }
        };
    }
    let made = match args.song_start {
        Some(start) => preview::render_song(&args.script, &soundfont, &args.out, start),
        None => preview::render(&args.script, &soundfont, &args.out),
    };
    match made {
        Ok(made) => {
            let what = match made {
                preview::Made::Animated => format!("{} and {}", preview::STILL, preview::SHEET),
                preview::Made::Still => format!("{} (it didn't draw for long enough to animate)", preview::STILL),
                preview::Made::Nothing => {
                    println!("no preview: it never drew anything");
                    return 0;
                }
            };
            println!("wrote {what} in {}", args.out.display());
            0
        }
        Err(e) if e.starts_with(preview::SCRIPT_ERROR) => {
            eprintln!("error: {e}");
            1
        }
        Err(e) => {
            eprintln!("error: {e}");
            2
        }
    }
}

/// The desktop app: a command-line command if one was given, else the window.
pub fn main() -> Result<()> {
    // Arguments mean someone ran this from a terminal: attach to it first,
    // so --help, errors and run-script output show up there.
    if std::env::args_os().len() > 1 && cli::attach_console() {
        println!();
    }
    let cli = Cli::parse();
    if let Some(dir) = &cli.config_dir
        && let Err(e) = config::set_config_dir(dir)
    {
        eprintln!("error: --config-dir: {e:#}");
        std::process::exit(2);
    }
    // A branch build's first launch: its own settings, copied from the
    // stable ones, before anything reads them. (Logged once the log is up.)
    let prepared = channels::prepare_own_config();

    if let Some(Command::RunScript(args)) = &cli.command {
        let config = Config::load().unwrap_or_default();
        std::process::exit(headless::run(args, &config));
    }
    if let Some(Command::Preview(args)) = &cli.command {
        let config = Config::load().unwrap_or_default();
        std::process::exit(preview_command(args, &config));
    }
    if let Some(Command::PublishBundled { server, dir }) = &cli.command {
        if let Err(e) = library::publish_bundled(server, dir.as_deref()) {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
        return Ok(());
    }
    if let Some(Command::PublisherRotate { server }) = &cli.command {
        if let Err(e) = library::publisher_rotate(server) {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
        return Ok(());
    }
    if let Some(Command::Admin(args)) = &cli.command {
        if let Err(e) = library::admin::run(args) {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
        return Ok(());
    }
    if let Some(Command::Serve(args)) = &cli.command {
        let data = args.data.clone().unwrap_or_else(library::server::default_data_dir);
        if let Err(e) = library::server::run(&data, args.bind.clone()) {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
        return Ok(());
    }

    // One app per config folder: a second launch brings the first one's
    // window back instead (before the log, which the first one is writing).
    let instance = match instance::claim() {
        Ok(instance::Claim::First(guard)) => Some(guard),
        Ok(instance::Claim::Running) => {
            println!("synththing is already running here: brought its window to the front.");
            return Ok(());
        }
        Err(e) => {
            eprintln!("couldn't check for another synththing running ({e:#}); starting anyway");
            None
        }
    };

    // (Only now, with any app it replaces closed.)
    let recopied = channels::recopy_settings_if_asked();

    applog::init(cli.console);
    log::info!("synththing v{} starting", build_info::describe());
    match recopied {
        Some(Ok(done)) => log::info!("settings copied again: {done}"),
        Some(Err(e)) => log::warn!("couldn't copy the settings again: {e:#}"),
        None => {}
    }
    match prepared {
        Some(Ok(done)) => log::info!("{done}"),
        Some(Err(e)) => log::warn!("couldn't copy the settings for this branch build: {e:#}"),
        None => {}
    }

    let (mut app, stream) = startup::start()?;
    if cli.visualizer {
        app.set_visualizer_open(true);
    }

    // The app icon (assets/synththing-icon.svg, drawn by build.rs) for the
    // title bar and taskbar; the exe has it as a resource too.
    let icon = egui::IconData {
        rgba: include_bytes!(concat!(env!("OUT_DIR"), "/icon_256.rgba")).to_vec(),
        width: 256,
        height: 256,
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(build_info::window_title())
            .with_inner_size([1100.0, 750.0])
            .with_icon(icon),
        ..Default::default()
    };
    eframe::run_native(
        "synththing",
        options,
        Box::new(|cc| {
            app::install_fonts(&cc.egui_ctx);
            app.attach_tray(&cc.egui_ctx);
            if let Some(guard) = &instance {
                app.listen_for_second_launch(guard.listen(&cc.egui_ctx));
            }
            Ok(Box::new(app))
        }),
    )
        .map_err(|e| anyhow!("failed to run the GUI: {e}"))?;

    // App drops here, closing the command channel; the render thread sees the
    // disconnect and exits. Keep the audio stream alive until then.
    drop(stream);
    Ok(())
}
