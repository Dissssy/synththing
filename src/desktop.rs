//! The desktop app: the command line (`cli.rs`) and its commands, then the
//! window, with what only the desktop has (one app per config folder, the
//! tray, branch builds' own settings).

use anyhow::{anyhow, Result};
use clap::Parser;
use eframe::egui;

use crate::cli::{self, Cli, Command, HubCommand};
use crate::config::{self, Config};
use crate::{app, applog, build_info, channels, headless, hub, instance, library, lua_visualizer, preview, startup};

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
    match preview::render(&args.script, &soundfont, &args.out) {
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
    for &kind in &cli.online {
        build_info::set_online(kind, true);
    }
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
    if let Some(Command::Hub(args)) = &cli.command {
        match &args.command {
            Some(HubCommand::Pair { link, name }) => {
                if let Err(e) = hub::pair(link, name.as_deref()) {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
                return Ok(());
            }
            Some(HubCommand::Token { name: None, .. }) => {
                match crate::hub_api::list_tokens() {
                    Ok(apps) if apps.is_empty() => println!("No programs are let in. Add one: synththing hub token <name> --grant read,add_songs"),
                    Ok(apps) => apps.iter().for_each(|app| println!("{app}")),
                    Err(e) => {
                        eprintln!("error: {e}");
                        std::process::exit(1);
                    }
                }
                return Ok(());
            }
            Some(HubCommand::Token { name: Some(name), remove: true, .. }) => {
                if let Err(e) = crate::hub_api::remove_token(name) {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
                println!("{name} isn't let in any more.");
                return Ok(());
            }
            Some(HubCommand::Token { name: Some(name), grant, remove: false }) => {
                match crate::hub_api::make_token(name, grant) {
                    Ok(Some(token)) => println!("{token}\n\n{name}'s token: give it as \"Authorization: Bearer {token}\". It isn't shown again."),
                    Ok(None) => println!("{name} keeps its token, allowed: {}", grant.join(", ")),
                    Err(e) => {
                        eprintln!("error: {e}");
                        std::process::exit(1);
                    }
                }
                return Ok(());
            }
            None => std::process::exit(hub::run()),
        }
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
    if !cli.online.is_empty() {
        log::info!("turned on for this run: {:?}", cli.online);
    }
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
    #[cfg_attr(not(windows), expect(unused_mut))]
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(build_info::window_title())
            .with_inner_size(app.start_size())
            .with_icon(icon),
        ..Default::default()
    };
    // DX12 on Windows, unless WGPU_BACKEND picks another: wgpu's own pick
    // there, Vulkan, could hang the window for good while it was resized.
    #[cfg(windows)]
    if eframe::wgpu::Backends::from_env().is_none()
        && let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup
    {
        setup.instance_descriptor.backends = eframe::wgpu::Backends::DX12;
    }
    eframe::run_native(
        "synththing",
        options,
        Box::new(|cc| {
            if let Some(render) = &cc.wgpu_render_state {
                let info = render.adapter.get_info();
                log::info!("graphics: {:?} on {} ({} {})", info.backend, info.name, info.driver, info.driver_info);
            }
            app::fonts::install(&cc.egui_ctx);
            app.attach_tray(&cc.egui_ctx);
            app.attach_window(hwnd(cc));
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

/// The main window's HWND, which the system's media controls need on
/// Windows.
#[cfg(windows)]
fn hwnd(cc: &eframe::CreationContext) -> Option<usize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match cc.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as usize),
        _ => None,
    }
}

#[cfg(not(windows))]
fn hwnd(_cc: &eframe::CreationContext) -> Option<usize> {
    None
}
