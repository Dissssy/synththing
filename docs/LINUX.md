# Porting synththing to Linux

synththing has only ever been built and run on Windows. This file lists what's known (from reading the code and the CI build, not from running the app on Linux) about getting it going there: what should already work, what's likely to need changes, and what has to be decided. It's meant as a starting point for whoever does the port, person or AI: check each item off (or correct it) as it's actually tried.

Status: CI's `linux` job (ubuntu-latest) builds it, and all but one test passed on the first try (2026-10-03, v0.3.1); that one was a test using Windows paths, fixed since. So it compiles and the logic holds up; nobody has opened the window or heard it play on Linux yet. That's the real first step.

## 1. Get it compiling

On a Linux machine with Rust (https://rustup.rs):

```
sudo apt install build-essential pkg-config libasound2-dev libudev-dev libgtk-3-dev \
    libxkbcommon-dev libwayland-dev libx11-dev libxcursor-dev libxrandr-dev libxi-dev libgl1-mesa-dev
cargo build
cargo test --bin synththing
```

(Package names are Debian/Ubuntu's; Fedora's are `alsa-lib-devel`, `systemd-devel`, `gtk3-devel`, `libxkbcommon-devel`, `wayland-devel`, `libX11-devel` and friends.) What each is for:

| Crate | Needs | Why |
|---|---|---|
| rodio (cpal) | `libasound2-dev` | audio output through ALSA (PipeWire and PulseAudio provide an ALSA device) |
| gilrs | `libudev-dev` | game controllers |
| rfd | `libgtk-3-dev` | the file and folder pickers (or switch rfd to its `xdg-portal` feature, no GTK needed) |
| eframe / winit | xkbcommon, Wayland, X11 libraries, OpenGL | the window |
| mlua (`vendored`) | a C compiler | builds Lua 5.4 from source |

CI (`.github/workflows/ci.yml`) has a `linux` job doing this on every push to master, allowed to fail (`continue-on-error`) until Linux is supported: its log shows how far the build gets. Once it passes, drop `continue-on-error` so it stays that way. It runs:

```yaml
  linux:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - run: sudo apt-get update && sudo apt-get install -y libasound2-dev libudev-dev libgtk-3-dev libxkbcommon-dev libwayland-dev
      - run: cargo build
      - run: cargo test --bin synththing
```

## 2. Already handled in the code

These have non-Windows branches already (written blind, so still worth checking):

- **Terminal output.** Release builds on Windows are GUI programs that re-attach to the launching terminal (`cli::attach_console`). On Linux there's nothing to do, and `attach_console` returns false. The `windows_subsystem` attribute in `main.rs` has no effect elsewhere.
- **ffmpeg** (`ffmpeg.rs`). Linux uses `ffmpeg` from the PATH; the download offer is Windows-only (`CAN_DOWNLOAD`), and Preferences > Recording says it isn't set up. Distributions all package ffmpeg.
- **Recording file names** (`recorder.rs`). `local_timestamp` uses UTC off Windows (no time zone lookup without another dependency); a crate like `time` or `chrono` with local offsets would fix it.
- **Show in folder** (`app.rs`, `open_in_file_manager`). Uses `xdg-open` on the folder (it can't select the file the way Explorer does).
- **The updater** (`updater.rs`). `ASSET_NAME` is per platform: on Linux it looks for `synththing-linux-x86_64`, which no release has yet, so it reports no update rather than downloading the Windows exe. See section 4.
- **Folders.** `directories` gives `~/.config/synththing` for the config (scripts, playlists, themes, logs), and `~/Music/synththing/Starter songs` for the starter songs (falling back to the home folder when there's no XDG music folder).
- **Paths and file names.** Extensions are compared case-insensitively. Script renames reject names Windows wouldn't allow, which is stricter than Linux needs but harmless.

## 3. Likely to need work or testing

- **Wayland.** winit runs on Wayland when it's available, else X11. Known differences there:
  - Dragging files in from the file manager (onto Songs, Playlists or Soundfonts) may not arrive; winit's Wayland drag-and-drop support has been limited. Under X11 (or `WAYLAND_DISPLAY= ./synththing` to force XWayland) it should work.
  - The Themes window asks to stay on top (`with_always_on_top` in `app/themes.rs`); Wayland compositors may ignore that, which only means it can end up behind the main window.
  - Scripts can ask to keep the cursor inside the window in the dedicated fullscreen (`CursorGrab::Confined` in `apply_cursor_confinement`); some compositors only support locking, not confining. It's optional, so failing quietly is fine.
- **Extra windows.** The Themes window is a second OS window (an immediate egui viewport). If a backend can't do that, egui falls back to drawing it inside the main window, which still works.
- **Audio latency.** The audio buffer (`buffer_ms`, a slider in the controls) was tuned on Windows. ALSA through PipeWire or PulseAudio may want a bigger default; the visuals already follow what's actually heard (`AudioRing` backlog), so only the latency changes.
- **Game controllers.** gilrs reads `/dev/input/event*`, which needs the user in the `input` group or a udev rule on some distributions. No controller means no pad input, nothing worse.
- **Keyboard.** F5, F9, F10 and F11 are the app's own keys (restart the script, recording, fullscreen). Some desktops take F11 or F10 for themselves.
- **Fonts and text.** All fonts are built in (egui's, plus Monogram for scripts), so nothing depends on what's installed.
- **The release build without a console.** `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]` only matters on Windows. On Linux, launched from a desktop entry, stdout goes nowhere; `--console` still prints the log when run from a terminal.

## 4. Decisions for whoever ports it

- **How it's distributed.** Options: a plain `x86_64` binary on the release (simplest; the updater can swap it in place like on Windows, as `self-replace` supports Linux), an AppImage (one file that carries its libraries; the updater would have to replace the AppImage rather than the binary inside it), or Flatpak (sandboxed: folder access goes through portals, and Flathub does its own updates, so the in-app updater would be turned off). A plain binary plus a `.desktop` file is the closest to how it works on Windows.
- **The release workflow.** `.github/workflows/release.yml` builds on `windows-latest` only. A Linux job would build on `ubuntu-latest` (an older Ubuntu image gives a binary that runs on more distributions), name the file to match `ASSET_NAME`, and attach it to the same release. The release notes come from `CHANGELOG.md` already.
- **The license list.** Help > Credits & licenses lists the libraries compiled into the Windows build (`examples/gen_licenses.rs`, `TARGET`). A Linux build pulls in some different ones (ALSA bindings, Wayland and X11 crates, no `windows-sys`); the generator could take the target as an argument and the app could embed the list for the platform it's built for.
- **Docs.** README.md, docs/cli.md and the in-app text say Windows in places (`%APPDATA%`, "Explorer", Windows SmartScreen, `Music\synththing`). Most of it should say where things are on each platform once Linux works.

## 5. Things that should just work

The audio engine and synth (rustysynth, vendored, pure Rust), MIDI and audio file loading (symphonia), the Lua scripting and its sandbox, the editor (analysis, completion, StyLua formatting), the visualizer and recording pipeline (frames go to ffmpeg over a pipe), playlists and .m3u files, folder watching (`notify` uses inotify on Linux), themes, the starter pack download, and `run-script` (headless, no window at all: a good first thing to try, `cargo run -- run-script assets/visualizers/visualizers/waveform.lua --frames 60`).
