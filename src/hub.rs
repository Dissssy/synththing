//! `synththing hub` (issue #192): a device with no window that keeps every
//! song, for an always-on machine (a home server, a NAS, a VPS, in Docker).
//! Signed in like any other device (`hub pair`, with a link from Sign in
//! another device), it syncs the settings, playlists, wishlist and song
//! library (`device_sync.rs`, the same as the app) and wants every song, so
//! each song a device sends streams straight to it, and it sends them on to
//! each device that wants them once that one's open.
//!
//! It names itself the hub in the transfers (`song_sync::HubLeaf`: its own
//! key, and where it's reached), and serves the songs to the user's devices
//! directly (`hub_server.rs`), on `PORT_VAR` (7390), at its address on its
//! network and at the addresses `URL_VAR` gives (its public address, say,
//! with the port forwarded to it). Devices that reach it take the songs it
//! has from it, and send it those it lacks; the relay stays for the rest.
//! Another device made the hub after it, it stops serving and says so.
//! As it starts and every 6 hours, it has the server check that its
//! addresses work from the internet (`library::reach`), and logs what to
//! fix if they don't (`hub check` does it now).

use std::io::IsTerminal;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use crate::config::{self, Config};
use crate::device::Device;
use crate::device_messages::{self, Outbox, Route};
use crate::device_sync::{self, DeviceSync, Here, Link, Report};
use crate::hub_api::{self, HubApi};
use crate::hub_server::{self, Came, HubServer};
use crate::song_sync::{AddressCheck, HubLeaf};
use crate::library::identity::{self, Identity};
use crate::library::{self, client, pairing, reach};
use crate::playlist::Library;
use crate::settings_sync;
use crate::song_store::SongStore;
use crate::wishlist::Wishlist;
use crate::{applog, build_info, instance};

/// How often it looks at what's due (and how soon it notices a stop).
const TICK: Duration = Duration::from_millis(250);
/// How often the server's info (the relay's song limit) is read again.
const INFO_EVERY: Duration = Duration::from_secs(60 * 60);
/// A sign-in link to sign in with on start, if it isn't signed in yet.
const PAIR_LINK_VAR: &str = "SYNTHTHING_PAIR_LINK";
/// The name to sign in as with it (default: the computer's name).
const NAME_VAR: &str = "SYNTHTHING_HUB_NAME";
/// How often it looks for a sign-in while it waits for one.
const SIGN_IN_CHECK: Duration = Duration::from_secs(5);
/// The port it serves songs on, and the addresses devices reach it at
/// (comma-separated, nearest first) besides its own on its network.
const PORT_VAR: &str = "SYNTHTHING_HUB_PORT";
const URL_VAR: &str = "SYNTHTHING_HUB_URL";
const PORT: u16 = 7390;
/// How long before listening to the server again after it failed.
const RETRY: Duration = Duration::from_secs(30);
/// How often the server checks that its addresses work from the internet.
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// What a device sends the hub, as a live message, to have it check its
/// addresses from the internet again (its sync settings' Check again).
pub const CHECK_TOPIC: &str = "hub check";
/// The soonest a device's asking checks again (the server allows an
/// identity 6 checks an hour).
const ASKED_GAP: Duration = Duration::from_secs(10 * 60);

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
    let identity = signed_in();
    let Some(sync_key) = identity::sync_key() else {
        log::error!("signed in without a sync key: sign in again with a link from a device that syncs");
        return 2;
    };
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
    let route = Route { server: server.clone(), server_key: link.server_key.clone(), identity: identity.clone(), sync_key, hub: None };
    // (Live messages from devices listening here, passed on to the server.)
    let mut outbox = Outbox::default();
    let heard = watch_server(&link, &identity);
    let (mut serving, came) = match serve(&identity, sync_key, &songs, &device) {
        Ok((server, came, leaf)) => (Some((server, leaf)), Some(came)),
        Err(e) => {
            log::error!("couldn't serve songs to your devices directly: {e}");
            (None, None)
        }
    };
    let mut sync = DeviceSync::default();
    // (Named the hub in the transfers yet: until then, another named there
    // is the one before it.)
    let mut named = false;
    let mut api = HubApi::default();
    let mut started = false;
    let mut error = None;
    let mut checked: Option<Instant> = None;
    let checks = mpsc::channel();
    loop {
        if info_read.is_none_or(|t| t.elapsed() >= INFO_EVERY) {
            info_read = Some(Instant::now());
            match client::info(&server) {
                Ok(info) if !info.sync => log::warn!("{} doesn't sync", library::host(&server)),
                Ok(info) => link.relay_song_mb = Some(info.relay_song_mb),
                Err(e) => log::warn!("reading {}'s info: {e}", library::host(&server)),
            }
        }
        let live = hub_api::Live { route: &route, me: &device, serving: serving.as_ref().map(|(server, _)| server) };
        while let Ok(heard) = heard.try_recv() {
            match heard {
                Heard::Sync(changed) => sync.server_seen(changed),
                // (Passed on to the devices listening here, once: the server
                // sends back those passed on to it from here.)
                Heard::Live(sealed) => {
                    if asks_check(&route, &device, &sealed) && checked.is_some_and(|t| t.elapsed() >= ASKED_GAP) {
                        checked = None;
                    }
                    if live.serving.is_none_or(|server| server.pass_on(&sealed)) {
                        api.live_heard(&live, &sealed);
                    }
                }
            }
        }
        for came in came.iter().flat_map(|came| came.try_iter()) {
            match came {
                Came::Song(key) => sync.received(&mut songs, key),
                Came::Live(sealed) => {
                    if asks_check(&route, &device, &sealed) && checked.is_some_and(|t| t.elapsed() >= ASKED_GAP) {
                        checked = None;
                    }
                    api.live_heard(&live, &sealed);
                    outbox.send_sealed(&route, sealed);
                }
            }
        }
        let here = &mut Here { songs: &mut songs, playlists: &mut playlists, wishlist: &mut wishlist };
        let reports = sync.poll(&link, here, false);
        if !started {
            started = true;
            start(&mut sync, &link, here, &config);
            if let Some((_, leaf)) = &serving {
                sync.be_hub(leaf.clone());
            }
        }
        if let Some((_, leaf)) = &serving
            && checked.is_none_or(|t| t.elapsed() >= CHECK_EVERY)
        {
            checked = Some(Instant::now());
            let (identity, leaf, tx) = (identity.clone(), leaf.clone(), checks.0.clone());
            std::thread::Builder::new()
                .name("hub check".into())
                .spawn(move || {
                    let checked = check(&identity, &leaf.key, &leaf.addrs);
                    for (checked, line) in &checked {
                        if checked.works {
                            log::info!("{line}");
                        } else {
                            log::warn!("{line}");
                        }
                    }
                    let _ = tx.send(checked.into_iter().map(|(checked, _)| checked).collect());
                })
                .ok();
        }
        // (For the other devices to show.)
        while let Ok(checked) = checks.1.try_recv() {
            sync.hub_checks(checked);
        }
        match sync.hub() {
            Some(hub) if hub.device == device.id => named = true,
            Some(_) if named && serving.is_some() => {
                log::warn!("another device is the hub now: not serving songs any more");
                serving = None;
            }
            _ => {}
        }
        let live = hub_api::Live { route: &route, me: &device, serving: serving.as_ref().map(|(server, _)| server) };
        for report in reports {
            match report {
                Report::Settings(synced) => match device_sync::synced_config(&config, &synced) {
                    Ok((synced_config, applied)) => {
                        config = synced_config;
                        sync.settings_applied(synced, applied);
                        // (Programs let in since it started, by `hub token`,
                        // stay let in.)
                        if let Ok(saved) = Config::load() {
                            config.api = saved.api;
                        }
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
                Report::LibraryChanged | Report::ImageFetched => {}
                Report::Status(text) | Report::Debug(text) => log::info!("{text}"),
            }
        }
        api.tick(&mut hub_api::Here { songs: &mut songs, playlists: &mut playlists, wishlist: &mut wishlist, sync: &mut sync, config: &config });
        api.panels_tick(&live);
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

/// The identity it's signed in as, once it is: signed in with the link in
/// `PAIR_LINK_VAR` if there is one, else waiting for `hub pair` or, with a
/// terminal (`docker attach`), a link pasted in (so a container waits to
/// be signed in instead of stopping).
fn signed_in() -> Identity {
    if let Some(identity) = Identity::load() {
        return identity;
    }
    let name = std::env::var(NAME_VAR).ok().filter(|n| !n.trim().is_empty());
    if let Ok(link) = std::env::var(PAIR_LINK_VAR)
        && !link.trim().is_empty()
        && let Err(e) = pair(link.trim(), name.as_deref())
    {
        log::error!("signing in with {PAIR_LINK_VAR}: {e}");
    }
    let pasted = std::io::stdin().is_terminal().then(read_links);
    let mut told = false;
    loop {
        if let Some(identity) = Identity::load() {
            return identity;
        }
        if !told {
            told = true;
            log::warn!(
                "not signed in: on a signed-in device, open Preferences > Account > Sign in another device, then run synththing hub pair '<link>' (in Docker: docker exec synththing-hub synththing hub pair '<link>'), or set {PAIR_LINK_VAR}. Waiting for it."
            );
            if pasted.is_some() {
                println!("Or paste the link here:");
            }
        }
        let Some(pasted) = &pasted else {
            std::thread::sleep(SIGN_IN_CHECK);
            continue;
        };
        match pasted.recv_timeout(SIGN_IN_CHECK) {
            Ok(link) => {
                if let Err(e) = pair(&link, name.as_deref()) {
                    println!("{e}. Paste another link:");
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => std::thread::sleep(SIGN_IN_CHECK),
        }
    }
}

/// The lines typed or pasted into the terminal, from a thread of its own
/// (until the receiver's dropped), leaving out blank ones.
fn read_links() -> Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("sign-in link".into())
        .spawn(move || {
            for line in std::io::stdin().lines().map_while(Result::ok) {
                let line = line.trim();
                if !line.is_empty() && tx.send(line.to_string()).is_err() {
                    return;
                }
            }
        })
        .ok();
    rx
}

/// Serve the songs in `songs` to the user's devices directly: the server,
/// the songs that come to it, and the hub's leaf, saying where it's
/// reached.
fn serve(identity: &Identity, sync_key: [u8; 32], songs: &SongStore, device: &Device) -> Result<(HubServer, Receiver<Came>, HubLeaf), String> {
    let port = match std::env::var(PORT_VAR) {
        Ok(port) => port.trim().parse().map_err(|_| format!("{PORT_VAR} isn't a port: {port}"))?,
        Err(_) => PORT,
    };
    let key = hub_server::hub_key()?;
    let hub_key = key.public();
    let (server, came) = HubServer::start(port, key, identity, sync_key, songs.dir().to_path_buf())?;
    let port = server.addr().port();
    let given = given();
    // (In a container, its own address is the container's: only those
    // given reach it.)
    let own = (std::env::var_os("SYNTHTHING_DOCKER").is_none()).then(own_address).flatten().map(|ip| format!("http://{}", std::net::SocketAddr::new(ip, port)));
    let addrs: Vec<String> = own.into_iter().chain(given).collect();
    if addrs.is_empty() {
        log::warn!("serving songs on port {port}, but your devices can't reach it directly until {URL_VAR} says where (http://192.168.1.20:{port}, say)");
    } else {
        log::info!("serving songs on port {port}, for your devices at {}", addrs.join(", "));
    }
    let leaf = HubLeaf { device: device.id.clone(), key: hub_key, addrs, checks: Vec::new() };
    Ok((server, came, leaf))
}

/// Whether `sealed` asks this hub to check its addresses again.
fn asks_check(route: &Route, me: &Device, sealed: &str) -> bool {
    device_messages::open(&route.sync_key, &me.id, sealed).is_some_and(|envelope| envelope.topic == CHECK_TOPIC)
}

/// The addresses `URL_VAR` gives.
fn given() -> Vec<String> {
    std::env::var(URL_VAR).unwrap_or_default().split(',').map(|url| url.trim().trim_end_matches('/').to_string()).filter(|url| !url.is_empty()).collect()
}

/// `synththing hub check`: whether the addresses `URL_VAR` gives work from
/// the internet, as the server finds now: the exit code.
pub fn check_now() -> i32 {
    let Some(identity) = Identity::load() else {
        eprintln!("error: the hub isn't signed in yet");
        return 2;
    };
    let hub_key = match hub_server::hub_key() {
        Ok(key) => key.public(),
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let lines = check(&identity, &hub_key, &given());
    if lines.is_empty() {
        println!("No address to check: {URL_VAR} gives none the internet reaches.");
    }
    lines.iter().for_each(|(_, line)| println!("{line}"));
    i32::from(lines.iter().any(|(checked, _)| !checked.works))
}

/// Whether each of `addrs` the internet should reach (not those on a
/// network of their own) works from it, as the server finds: each check,
/// and a line on it.
fn check(identity: &Identity, hub_key: &str, addrs: &[String]) -> Vec<(AddressCheck, String)> {
    let server = library::official_url();
    let server_key = library::server_key();
    addrs
        .iter()
        .filter(|url| !reach::local(url))
        .map(|url| {
            let at = settings_sync::now();
            let checked = |works, hint| AddressCheck { url: url.clone(), at, works, hint };
            match client::reach(&server, &server_key, identity, url, hub_key) {
                Ok(found) => match reach::hint(url, &found) {
                    None => {
                        let times: Vec<String> = [("IPv4", &found.v4), ("IPv6", &found.v6)]
                            .into_iter()
                            .filter_map(|(family, probe)| match probe.as_ref()?.verdict {
                                reach::Verdict::Ok { ms } => Some(format!("{family} {ms} ms")),
                                _ => None,
                            })
                            .collect();
                        (checked(true, None), format!("{url} works from the internet ({})", times.join(", ")))
                    }
                    Some(hint) => {
                        let line = format!("{url} doesn't work from the internet: {hint}");
                        (checked(false, Some(hint)), line)
                    }
                },
                Err(e) => (checked(false, Some(format!("couldn't check it: {e}"))), format!("couldn't check {url}: {e}")),
            }
        })
        .collect()
}

/// This computer's address on its network (the one it reaches the internet
/// from: nothing is sent to find it).
fn own_address() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    socket.local_addr().ok().map(|addr| addr.ip()).filter(|ip| !ip.is_loopback() && !ip.is_unspecified())
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

/// What the server said, listened to.
enum Heard {
    /// Synced data changed: `None` each time it's listened to again (what
    /// changed meanwhile isn't known), the change otherwise.
    Sync(Option<library::sync::Changed>),
    /// A live message from one of the user's devices, sealed.
    Live(String),
}

/// Listen to the server, on a thread of its own (until the receiver's
/// dropped).
fn watch_server(link: &Link, identity: &Identity) -> Receiver<Heard> {
    let (tx, rx): (Sender<_>, _) = mpsc::channel();
    let (server, key, identity) = (link.server.clone(), link.server_key.clone(), identity.clone());
    std::thread::Builder::new()
        .name("server watch".into())
        .spawn(move || {
            loop {
                if tx.send(Heard::Sync(None)).is_err() {
                    return;
                }
                let started = Instant::now();
                let streamed = client::notification_stream(&server, &key, &identity, |event| match event {
                    client::StreamEvent::Sync(changed) => tx.send(Heard::Sync(Some(changed))).is_ok(),
                    client::StreamEvent::Live(sealed) => tx.send(Heard::Live(sealed)).is_ok(),
                    client::StreamEvent::Notification(_) | client::StreamEvent::Opened => true,
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
