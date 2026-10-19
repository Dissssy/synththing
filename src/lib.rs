//! synththing, a small GUI MIDI player built on rustysynth + rodio + egui.
//!
//! Play/pause, seek, and playback-speed control for a MIDI file, a Lua-scripted
//! visualizer, and a hot-swappable list of soundfonts retained between runs.
//! Synthesis runs on its own thread (`audio.rs`), rendering ahead into a ring
//! buffer so a dense passage can't stall the audio callback or the GUI.
//!
//! The app is this library; each platform starts it its own way. The
//! desktop binary (`main.rs`) calls [`main`] (`desktop.rs`: the command line,
//! then the window); the Android app (`android/`) calls `android::main`
//! from its `android_main`. What they share, starting playback and the
//! [`App`](app::App), is `startup.rs`.

// Android uses the same app without what only the desktop has (the command
// line, the library server, the tray, script editing...): unused there,
// not dead.
#![cfg_attr(target_os = "android", allow(dead_code))]

mod app;
mod applog;
mod audio;
mod build_info;
mod changelog;
mod channels;
mod cli;
mod code_edit;
mod config;
mod credits;
mod engine;
mod ffmpeg;
mod filebrowser;
mod gamepad;
mod headless;
mod import;
mod instance;
mod layout;
mod library;
mod live;
mod loader;
mod lua_analysis;
mod lua_completion;
mod lua_docs;
mod lua_highlight;
mod lua_visualizer;
mod midi_notes;
mod offline;
mod pixel_font;
mod playlist;
mod preview;
mod recorder;
mod script_host;
mod select_list;
mod snippets;
mod song_info;
mod song_title;
mod song_preview;
mod spectrum;
mod sprite_code;
mod starter;
mod startup;
mod theme;
mod tray;
mod typing;
mod updater;
mod visualizer;
mod watch;

#[cfg(target_os = "android")]
pub mod android;
#[cfg(not(target_os = "android"))]
mod desktop;
#[cfg(not(target_os = "android"))]
pub use desktop::main;
