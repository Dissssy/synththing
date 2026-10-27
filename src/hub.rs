//! `synththing hub` (issue #192): a device with no window that keeps every
//! song, for an always-on machine (a home server, a NAS, a VPS, in Docker).
//! Signed in like any other device (`hub pair`, with a link from Sign in
//! another device), it syncs the settings, playlists, wishlist and song
//! library (`device_sync.rs`, the same as the app) and wants every song, so
//! each song a device sends streams straight to it, and it sends them on to
//! each device that wants them once that one's open.

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use crate::config::{self, Config};
use crate::device::Device;
use crate::device_sync::{self, DeviceSync, Here, Link, Report};
use crate::library::identity::{self, Identity};
use crate::library::{self, client, pairing};
use crate::playlist::Library;
use crate::settings_sync;
use crate::song_store::SongStore;
use crate::wishlist::Wishlist;
use crate::{applog, build_info, instance};

/// How often it looks at what's due (and how soon it notices a stop).
const TICK: Duration = Duration::from_millis(250);
/// How often the server's info (the relay's song limit) is read again.
const INFO_EVERY: Duration = Duration::from_secs(60 * 60);
/// How long before listening to the server again after it failed.
const RETRY: Duration = Duration::from_secs(30);

/// Sign this config folder in with a link from Sign in another device, as
/// `name` (else the computer's name).
pub fn pair(link: &str, name: Option<&str>) -> Result<(), String> {
    let link = pairing::Link::parse(link).ok_or("that isn't a sign-in link")?;
    let mut device = Device::load();
    if let Some(name) = name {
        device.name = name.to_string();
        device.save().map_err(|e| format!("saving device.json: {e:#}"))?;
    }
    let sealed = client::pair_take(&link.server, &link.id, &device.name)?;
    let text = String::from_utf8(pairing::open(&link.secret, &sealed)?).map_err(|_| "the handoff is damaged")?;
    let identity = identity::adopt(&text, "")?;
    println!("Signed in as #{} on {}, as \"{}\". Start the hub with: synththing hub", identity.id(), library::host(&link.server), device.name);
    Ok(())
}

/// Run the hub until it's stopped: the exit code.
pub fn run() -> i32 {
    let _instance = match instance::claim() {
        Ok(instance::Claim::First(guard)) => guard,
        Ok(instance::Claim::Running) => {
            eprintln!("error: synththing is already running with this config folder");
            return 2;
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            return 2;
        }
    };
    applog::init(true);
    log::info!("synththing hub v{} starting", build_info::describe());
    let Some(identity) = Identity::load() else {
        log::error!("not signed in: on a signed-in device, open Preferences > Online Library > Sign in another device, then run: synththing hub pair '<link>'");
        return 2;
    };
    if identity::sync_key().is_none() {
        log::error!("signed in without a sync key: sign in again with a link from a device that syncs");
        return 2;
    }
    let mut config = Config::load().unwrap_or_default();
    let songs_dir = match config::music_dir() {
        Ok(dir) => dir,
        Err(e) => {
            log::error!("{e:#}");
            return 2;
        }
    };
    let mut songs = SongStore::open(songs_dir.clone());
    let (mut playlists, errors) = Library::load(config::playlists_dir().unwrap_or_else(|_| songs_dir.with_file_name("playlists")), songs_dir);
    for error in errors {
        log::warn!("{error}");
    }
    let mut wishlist = Wishlist::load();
    let device = Device::load();
    log::info!("signed in as #{} as \"{}\", {} songs in {}", identity.id(), device.name, songs.known().count(), songs.dir().display());

    let server = library::official_url();
    let mut link = Link { server: server.clone(), server_key: library::server_key(), identity: Some(identity.clone()), relay_song_mb: None };
    let mut info_read: Option<Instant> = None;
    let seen = watch_server(&link, &identity);
    let mut sync = DeviceSync::default();
    let mut started = false;
    let mut error = None;
    loop {
        if info_read.is_none_or(|t| t.elapsed() >= INFO_EVERY) {
            info_read = Some(Instant::now());
            match client::info(&server) {
                Ok(info) if !info.sync => log::warn!("{} doesn't sync", library::host(&server)),
                Ok(info) => link.relay_song_mb = Some(info.relay_song_mb),
                Err(e) => log::warn!("reading {}'s info: {e}", library::host(&server)),
            }
        }
        while let Ok(changed) = seen.try_recv() {
            sync.server_seen(changed);
        }
        let here = &mut Here { songs: &mut songs, playlists: &mut playlists, wishlist: &mut wishlist };
        let reports = sync.poll(&link, here, false);
        if !started {
            started = true;
            start(&mut sync, &link, here, &config);
        }
        for report in reports {
            match report {
                Report::Settings(synced) => match device_sync::synced_config(&config, &synced) {
                    Ok((synced_config, applied)) => {
                        config = synced_config;
                        sync.settings_applied(synced, applied);
                        if let Err(e) = config.save() {
                            log::warn!("settings sync: {e:#}");
                        }
                    }
                    Err(e) => log::warn!("settings sync: {e}"),
                },
                Report::Playlists(synced) => {
                    let here = &mut Here { songs: &mut songs, playlists: &mut playlists, wishlist: &mut wishlist };
                    if let Err(e) = sync.apply_playlists(synced, here) {
                        log::warn!("couldn't save a synced playlist: {e}");
                    }
                }
                Report::Fetched(stored) => {
                    log::info!("got {}", stored.name);
                    songs.add(stored);
                    sync.songs_changed();
                }
                Report::LibraryChanged => {}
                Report::Status(text) | Report::Debug(text) => log::info!("{text}"),
            }
        }
        let now = sync.error().map(str::to_string);
        if now != error {
            if let Some(e) = &now {
                log::warn!("couldn't sync: {e}");
            }
            error = now;
        }
        std::thread::sleep(TICK);
    }
}

/// Turn sync on here if it isn't, and want every song.
fn start(sync: &mut DeviceSync, link: &Link, here: &Here, config: &Config) {
    let json = config.to_json().unwrap_or_default();
    if !sync.on() {
        log::info!("turning sync on");
        sync.turn_on(link, here, &json);
    }
    let wanted = [&settings_sync::SONGS, &settings_sync::EVERY_SONG];
    let off: Vec<_> = wanted.into_iter().filter(|s| !settings_sync::syncs(sync.choices(), s)).map(|s| (s, true)).collect();
    if !off.is_empty() {
        sync.choose(link, here, off, &json);
    }
}

/// Listen to the server for synced data changing, on a thread of its own
/// (until the receiver's dropped): `None` each time it's listened to again
/// (what changed meanwhile isn't known), the change otherwise.
fn watch_server(link: &Link, identity: &Identity) -> Receiver<Option<library::sync::Changed>> {
    let (tx, rx): (Sender<_>, _) = mpsc::channel();
    let (server, key, identity) = (link.server.clone(), link.server_key.clone(), identity.clone());
    std::thread::Builder::new()
        .name("server watch".into())
        .spawn(move || {
            loop {
                if tx.send(None).is_err() {
                    return;
                }
                let started = Instant::now();
                let streamed = client::notification_stream(&server, &key, &identity, |event| match event {
                    client::StreamEvent::Sync(changed) => tx.send(Some(changed)).is_ok(),
                    client::StreamEvent::Notification(_) => true,
                });
                if let Err(e) = streamed
                    && started.elapsed() < client::STREAM_FOR
                {
                    log::info!("listening to the server: {e}");
                }
                // (It ran its time, or nearly: listen again now.)
                if started.elapsed() < Duration::from_secs(60) {
                    std::thread::sleep(RETRY);
                }
            }
        })
        .ok();
    rx
}
