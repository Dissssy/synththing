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

mod album_art;
mod api;
mod app;
#[cfg(any(target_os = "android", test))]
mod android_id;
mod applog;
mod audio;
mod build_info;
mod changelog;
mod channels;
mod cleanup;
mod cli;
mod code_edit;
mod commands;
mod config;
mod device;
mod device_sync;
mod device_messages;
mod duplicates;
mod credits;
mod decoded_audio;
mod engine;
mod fields;
mod file_tags;
mod frame_timing;
mod ffmpeg;
mod gamepad;
mod headless;
mod hub_api;
mod hub;
mod hub_server;
mod import;
mod instance;
mod layout;
mod library;
mod lookup;
mod live;
mod loader;
mod lua_analysis;
mod lua_api;
mod lua_completion;
mod lua_docs;
mod lua_highlight;
mod lua_visualizer;
mod marquee;
mod matching;
mod menu;
#[cfg(not(target_os = "android"))]
mod media_controls;
mod midi_notes;
mod offline;
mod bundled_fonts;
mod pixel_font;
mod panel;
mod panel_share;
mod playback_share;
mod playlist;
mod playlist_sync;
mod preview;
mod recorder;
mod script_host;
mod search;
mod search_box;
mod wishlist_search;
mod select_list;
mod settings_sync;
mod smart;
mod snippets;
mod song_info;
mod song_list;
mod song_repair;
mod staged;
mod song_sync;
mod song_store;
mod song_title;
mod spectrum;
mod sprite_code;
mod starter;
mod startup;
mod storage;
mod theme;
mod together;
mod tray;
mod typing;
mod updater;
mod visualizer;
mod visualizer_stream;
mod watch;
mod wishlist;

#[cfg(target_os = "android")]
pub mod android;
#[cfg(not(target_os = "android"))]
mod desktop;
#[cfg(not(target_os = "android"))]
pub use desktop::main;
