//! The local app API (`api.rs`) on a hub (`hub.rs`): what's about the
//! library answered as the app answers it (songs, playlists, tags, the
//! wishlist, adding and staging songs), what's about playing refused (a
//! hub plays nothing), and the library's events sent. On while any program
//! has a token (`synththing hub token`, which writes the config folder's
//! settings: they're read again when they change), on the port and
//! network the settings say, or `SYNTHTHING_API_PORT` and
//! `SYNTHTHING_API_LAN` (`true`: let in other machines, as Docker's other
//! containers are). Programs' panels it hosts for the user's devices
//! (`panel_share.rs`), as the app does: a hub shows none itself.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::time::{Instant, SystemTime};

use eframe::egui;

use crate::api::{self, Access, Call, Edit, PlaylistRef, SongInfo};
use crate::config::Config;
use crate::device::Device;
use crate::device_messages::{self, Envelope, Outbox, Route};
use crate::device_sync::DeviceSync;
use crate::hub_server::HubServer;
use crate::lookup::Kept;
use crate::panel_share::{self, Host, Message, Out};
use crate::playlist::{Library, PlaylistEntry};
use crate::song_store::{Online, SongStore, Stored};
use crate::wishlist::{Progress, Wishlist};

const PORT_VAR: &str = "SYNTHTHING_API_PORT";
const LAN_VAR: &str = "SYNTHTHING_API_LAN";
/// What a hub says to what only a player can do.
const NO_PLAYER: &str = "this is a hub: it doesn't play songs";

/// What the API answers about, as the hub has it.
pub struct Here<'a> {
    pub songs: &'a mut SongStore,
    pub playlists: &'a mut Library,
    pub wishlist: &'a mut Wishlist,
    pub sync: &'a mut DeviceSync,
    pub config: &'a Config,
}

#[derive(Default)]
pub struct HubApi {
    server: Option<api::Server>,
    /// The port and network it couldn't listen on, and why (not tried
    /// again until they change).
    failed: Option<((u16, bool), String)>,
    /// When the settings file last changed, as last read.
    read_at: Option<SystemTime>,
    apps: Vec<Access>,
    kept: Kept,
    /// What of each wish is here, worked out for these revisions (the
    /// wishlist's and the songs').
    progress: Option<((u64, u64), BTreeMap<String, Progress>)>,
    /// What the last events told of: the songs' revision, the playlists
    /// and the wishlist (hashes).
    seen: Option<(u64, u64, u64)>,
    /// The panels' revision as last taken, the shared ones as hosted, and
    /// what's sent about them.
    panels_revision: u64,
    host: Host,
    outbox: Outbox,
}

/// Where the hub's live messages go, and as which device: the server,
/// and the devices listening to the hub (`serving`) while it serves.
pub struct Live<'a> {
    pub route: &'a Route,
    pub me: &'a Device,
    pub serving: Option<&'a HubServer>,
}

impl HubApi {
    /// Each tick: the server on or off as the settings say, its calls
    /// answered, events sent.
    pub fn tick(&mut self, here: &mut Here) {
        self.read_apps(here.config);
        let (port, lan) = where_to_listen(here.config);
        if self.apps.is_empty() {
            self.server = None;
            self.failed = None;
        } else if self.server.as_ref().is_none_or(|s| s.addr().port() != port || s.addr().ip().is_loopback() == lan)
            && self.failed.as_ref().is_none_or(|(at, _)| *at != (port, lan))
        {
            self.server = None;
            let staging = crate::config::config_dir().map_or_else(|_| std::env::temp_dir().join("synththing-staged"), |dir| dir.join("staged"));
            match api::Server::start(port, lan, self.apps.clone(), egui::Context::default(), staging) {
                Ok(server) => {
                    log::info!("api: listening on {} for {} program(s)", server.addr(), self.apps.len());
                    self.server = Some(server);
                    self.failed = None;
                }
                Err(e) => {
                    log::warn!("api: {e}");
                    self.failed = Some(((port, lan), e));
                }
            }
        }
        let Some(server) = &self.server else { return };
        server.set_apps(&self.apps);
        let calls: Vec<Call> = server.calls().collect();
        for call in calls {
            self.answer(call, here);
        }
        self.events(here);
    }

    /// Each tick: the shared panels as the API has them, and what the
    /// user's devices are due to hear of them.
    pub fn panels_tick(&mut self, live: &Live) {
        match &self.server {
            Some(server) => {
                if let Some(shown) = server.panels(&mut self.panels_revision) {
                    self.host.take(&shown);
                }
            }
            None => self.host.take(&[]),
        }
        let server = self.server.as_ref();
        let out = self.host.tick(Instant::now(), |app| server.is_some_and(|s| s.connected(app)));
        self.send(live, out);
    }

    /// A live message came, sealed: a device asking for the panels, or
    /// what the user did with one.
    pub fn live_heard(&mut self, live: &Live, sealed: &str) {
        let Some(envelope) = device_messages::open(&live.route.sync_key, &live.me.id, sealed) else { return };
        if envelope.topic != panel_share::TOPIC {
            return;
        }
        let server = self.server.as_ref();
        match serde_json::from_value::<Message>(envelope.body) {
            Ok(Message::Hello) => {
                let out = self.host.hello(&envelope.from.id, |app| server.is_some_and(|s| s.connected(app)));
                self.send(live, out);
            }
            Ok(Message::Event { app, event }) => {
                if let (Some(server), Some(event)) = (server, self.host.event(envelope.from, &app, event)) {
                    server.panel_event(&app, &event);
                }
            }
            // (Other devices' panels: a hub shows none.)
            Ok(_) => {}
            Err(e) => log::info!("a panels message from {} doesn't read: {e}", envelope.from.name),
        }
    }

    fn send(&mut self, live: &Live, out: Vec<Out>) {
        for Out { to, message } in out {
            match serde_json::to_value(&message) {
                Ok(body) => {
                    let sent = self.outbox.send(live.route, &Envelope { from: live.me.clone(), to, topic: panel_share::TOPIC.to_string(), body });
                    if let (Some(sealed), Some(server)) = (sent, live.serving) {
                        server.pass_on(&sealed);
                    }
                }
                Err(e) => log::warn!("a panels message: {e}"),
            }
        }
    }

    /// The programs let in, read again when the settings file changes
    /// (a token made by `hub token` while it runs).
    fn read_apps(&mut self, config: &Config) {
        let changed = Config::file_path().ok().and_then(|path| std::fs::metadata(path).ok()).and_then(|m| m.modified().ok());
        if self.read_at.is_some() && changed == self.read_at {
            return;
        }
        self.read_at = changed;
        self.apps = match Config::load() {
            Ok(saved) => saved.api.apps,
            Err(_) => config.api.apps.clone(),
        };
    }

    fn progress(&mut self, songs: &SongStore, wishlist: &Wishlist) -> &BTreeMap<String, Progress> {
        let revisions = (wishlist.revision(), songs.revision());
        if self.progress.as_ref().is_none_or(|(at, _)| *at != revisions) {
            self.progress = Some((revisions, crate::wishlist::progress(wishlist, songs, &self.kept)));
        }
        &self.progress.as_ref().expect("just worked out").1
    }

    fn answer(&mut self, call: Call, here: &mut Here) {
        let refused = || Err((409, NO_PLAYER.to_string()));
        match call {
            Call::Command(_, reply) => drop(reply.send(Err(NO_PLAYER.into()))),
            Call::NowPlaying(reply) => drop(reply.send(api::NowPlaying::default())),
            Call::Play { reply, .. } => drop(reply.send(refused())),
            Call::Set(_, reply) | Call::Queue(_, reply) | Call::UseSoundfont(_, reply) | Call::UseScript(_, reply) => drop(reply.send(refused())),
            Call::Library(reply) => drop(reply.send(library(here))),
            Call::Edit(edit, reply) => drop(reply.send(self.edit(edit, here))),
            Call::NewPlaylist(name, reply) => {
                let made = here.playlists.create(&name, Vec::new()).map_err(|e| (500, format!("couldn't make it: {e}")));
                let made = made.map(|i| PlaylistRef { id: here.playlists.lists[i].id.clone(), name });
                if made.is_ok() {
                    here.sync.playlists_changed(here.songs, here.playlists);
                }
                drop(reply.send(made));
            }
            Call::Add { stored, program, online, reply } => drop(reply.send(add(here, stored, &program, online))),
            Call::Wishlist(query, reply) => {
                let progress = self.progress(here.songs, here.wishlist).clone();
                drop(reply.send(api::wishes(here.wishlist, &progress, &query)));
            }
            Call::Wish { wish, release, program, reply } => {
                let wished = api::wish(here.wishlist, wish, &program).map(|(id, new)| {
                    if let Some(release) = release {
                        self.kept.keep_release(release);
                    }
                    here.sync.wishlist_changed(here.wishlist);
                    let progress = self.progress(here.songs, here.wishlist);
                    (api::wish_info(here.wishlist, progress, &id), new)
                });
                drop(reply.send(wished));
            }
        }
    }

    fn edit(&mut self, edit: Edit, here: &mut Here) -> Result<(), (u16, String)> {
        let no_song = || (404, "there's no song with that key in the library".to_string());
        let no_playlist = || (404, "there's no playlist with that ID".to_string());
        let no_wish = || (404, "there's no wish with that ID".to_string());
        let find = |playlists: &Library, id: &str| playlists.find(id).ok_or_else(no_playlist);
        match edit {
            Edit::Star { key, on } => set_tag(here, &key, crate::song_store::STARRED, on).ok_or_else(no_song)?,
            Edit::Tag { key, tag, on } => {
                let id = here.songs.tags().iter().find(|(_, t)| t.name.eq_ignore_ascii_case(&tag)).map(|(id, _)| id.clone());
                let id = id.ok_or_else(|| (404, format!("there's no tag \"{tag}\" (make it in synththing first)")))?;
                set_tag(here, &key, &id, on).ok_or_else(no_song)?;
            }
            Edit::Playlist { playlist, key, on } => {
                let list = find(here.playlists, &playlist)?;
                if here.playlists.lists[list].playlist.smart.is_some() {
                    return Err((409, "that's a smart playlist: its rules pick its songs".into()));
                }
                if !here.songs.has(&key) {
                    return Err(no_song());
                }
                let path = here.songs.path(&key);
                let entries = &mut here.playlists.lists[list].playlist.entries;
                if on {
                    entries.push(PlaylistEntry::new(path));
                } else {
                    let before = entries.len();
                    entries.retain(|e| e.path != path);
                    if entries.len() == before {
                        return Err((404, "that song isn't in that playlist".into()));
                    }
                }
                self.save_playlist(here, list)?;
            }
            Edit::Rename { playlist, name } => {
                let list = find(here.playlists, &playlist)?;
                here.playlists.lists[list].playlist.name = name;
                self.save_playlist(here, list)?;
            }
            Edit::Delete { playlist } => {
                let list = find(here.playlists, &playlist)?;
                here.playlists.delete(list).map_err(|e| (500, format!("couldn't delete it: {e}")))?;
                here.sync.playlists_changed(here.songs, here.playlists);
            }
            Edit::Move { playlist, from, to } => {
                let list = find(here.playlists, &playlist)?;
                if here.playlists.lists[list].playlist.smart.is_some() {
                    return Err((409, "that's a smart playlist: its rules order its songs".into()));
                }
                let entries = &mut here.playlists.lists[list].playlist.entries;
                if from >= entries.len() || to >= entries.len() {
                    return Err((404, format!("that playlist has {} songs", entries.len())));
                }
                let entry = entries.remove(from);
                entries.insert(to, entry);
                self.save_playlist(here, list)?;
            }
            Edit::Unwish { id } => {
                if !here.wishlist.wishes.contains_key(&id) {
                    return Err(no_wish());
                }
                here.wishlist.remove(&[id]);
                here.sync.wishlist_changed(here.wishlist);
            }
            Edit::Note { id, note } => {
                if !here.wishlist.wishes.contains_key(&id) {
                    return Err(no_wish());
                }
                here.wishlist.set_note(&id, &note);
                here.sync.wishlist_changed(here.wishlist);
            }
        }
        Ok(())
    }

    fn save_playlist(&self, here: &mut Here, list: usize) -> Result<(), (u16, String)> {
        here.playlists.save(list).map_err(|e| (500, format!("couldn't save it: {e}")))?;
        here.sync.playlists_changed(here.songs, here.playlists);
        Ok(())
    }

    /// While a stream's open, what changed since last time.
    fn events(&mut self, here: &Here) {
        if !self.server.as_ref().is_some_and(api::Server::listened_to) {
            self.seen = None;
            return;
        }
        let playlists = hash(&api::playlist_infos(here.playlists, here.songs).iter().map(|p| (&p.id, &p.name, &p.songs)).collect::<Vec<_>>());
        let wishlist = {
            let revision = here.wishlist.revision();
            let progress: Vec<(String, usize, usize)> = self.progress(here.songs, here.wishlist).iter().map(|(id, p)| (id.clone(), p.have, p.total)).collect();
            hash(&(revision, progress))
        };
        let server = self.server.as_ref().expect("listened to");
        let now = (here.songs.revision(), playlists, wishlist);
        let Some(last) = self.seen.replace(now) else { return };
        let empty = serde_json::json!({});
        if now.0 != last.0 {
            server.event("library_changed", &empty);
        }
        if now.1 != last.1 {
            server.event("playlists_changed", &empty);
        }
        if now.2 != last.2 {
            server.event("wishlist_changed", &empty);
        }
    }
}

/// The port and network to listen on: the environment's, else the
/// settings'.
fn where_to_listen(config: &Config) -> (u16, bool) {
    let port = std::env::var(PORT_VAR).ok().and_then(|p| p.trim().parse().ok()).unwrap_or_else(|| config.api.port());
    let lan = std::env::var(LAN_VAR).ok().map_or(config.api.lan, |v| matches!(v.trim(), "1" | "true" | "yes" | "on"));
    (port, lan)
}

fn hash(value: &impl Hash) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// The library as `/v1/songs` and the rest search it: no queue,
/// soundfonts or scripts on a hub.
fn library(here: &Here) -> api::Library {
    let config = here.config;
    api::Library {
        store: here.songs.clone(),
        wishes: here.wishlist.wishes.clone(),
        playlists: api::playlist_infos(here.playlists, here.songs),
        rulesets: crate::smart::rulesets(&config.rulesets),
        group_ignore: config.group_ignore.clone(),
        articles: config.sort_with_articles,
        online: config.online_lookup && crate::build_info::online(crate::build_info::Online::Lookup),
        queue: Vec::new(),
        soundfonts: Vec::new(),
        scripts: Vec::new(),
    }
}

/// Give the song `key` the tag `id` or take it off: `None` if there's no
/// such song.
fn set_tag(here: &mut Here, key: &str, id: &str, on: bool) -> Option<()> {
    let mut tags = here.songs.get_by_key(key).filter(|_| here.songs.has(key))?.tags.clone();
    tags.retain(|t| t != id);
    if on {
        tags.push(id.to_string());
    }
    here.songs.set_song_tags(key, tags);
    here.sync.songs_changed();
    Some(())
}

/// A song a program sent into the library, its file read (the hub has no
/// window reading them as they come): marked as the program's if it's new,
/// matched as `online` says unless it's matched already.
fn add(here: &mut Here, stored: Stored, program: &str, online: Option<Online>) -> Result<(SongInfo, bool), String> {
    let key = stored.key.clone();
    let new = here.songs.get_by_key(&key).is_none();
    here.songs.add(stored);
    let path = here.songs.path(&key);
    match crate::song_info::read(&path) {
        Ok(info) => {
            here.songs.note_meta([(key.clone(), info.meta)]);
        }
        Err(e) => log::warn!("api: couldn't read {}: {e}", path.display()),
    }
    if new {
        here.songs.set_added_by(&key, program);
    }
    let unmatched = here.songs.get_by_key(&key).is_some_and(|song| song.online.as_ref().is_none_or(|o| o.release.is_none() && o.recording.is_none()));
    if online.is_some() && unmatched {
        here.songs.set_online(vec![(key.clone(), online)]);
    }
    here.sync.songs_changed();
    let song = here.songs.get_by_key(&key).ok_or("the song didn't stay in the library")?;
    Ok((SongInfo::of(&key, song, here.songs), new))
}

/// Let the program `name` in with a new token (`synththing hub token`),
/// allowed `grants`: the token, shown once. A program let in already keeps
/// its token and is allowed `grants` from now on instead (`None`). Saved
/// in the settings, which a running hub reads again.
pub fn make_token(name: &str, grants: &[String]) -> Result<Option<String>, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("give the program a name".into());
    }
    let mut config = Config::load().map_err(|e| format!("{e:#}"))?;
    let mut made = None;
    let at = match config.api.apps.iter().position(|a| a.name.eq_ignore_ascii_case(name)) {
        Some(at) => at,
        None => {
            let (access, token) = Access::new(name);
            made = Some(token);
            config.api.apps.push(access);
            config.api.apps.len() - 1
        }
    };
    let access = &mut config.api.apps[at];
    (access.read, access.add_songs, access.download_songs, access.edit_library, access.control) = (false, false, false, false, false);
    (access.panel, access.share) = (false, false);
    for grant in grants.iter().flat_map(|g| g.split(',')).map(str::trim).filter(|g| !g.is_empty()) {
        match grant {
            "read" => access.read = true,
            "add_songs" | "add-songs" => access.add_songs = true,
            "download_songs" | "download-songs" => access.download_songs = true,
            "edit_library" | "edit-library" => access.edit_library = true,
            "control" => access.control = true,
            "panel" => access.panel = true,
            "share" => access.share = true,
            other => return Err(format!("\"{other}\" isn't a permission: read, add_songs, download_songs, edit_library, control, panel or share")),
        }
    }
    config.save().map_err(|e| format!("{e:#}"))?;
    Ok(made)
}

/// Stop letting the program `name` in.
pub fn remove_token(name: &str) -> Result<(), String> {
    let mut config = Config::load().map_err(|e| format!("{e:#}"))?;
    let before = config.api.apps.len();
    config.api.apps.retain(|a| !a.name.eq_ignore_ascii_case(name.trim()));
    if config.api.apps.len() == before {
        return Err(format!("there's no program named \"{name}\""));
    }
    config.save().map_err(|e| format!("{e:#}"))
}

/// The programs let in, and what each may do.
pub fn list_tokens() -> Result<Vec<String>, String> {
    let config = Config::load().map_err(|e| format!("{e:#}"))?;
    Ok(config
        .api
        .apps
        .iter()
        .map(|a| {
            let grants: Vec<&str> = [(a.read, "read"), (a.add_songs, "add_songs"), (a.download_songs, "download_songs"), (a.edit_library, "edit_library"), (a.control, "control"), (a.panel, "panel"), (a.share, "share")]
                .into_iter()
                .filter_map(|(on, name)| on.then_some(name))
                .collect();
            format!("{}: {}", a.name, if grants.is_empty() { "nothing".to_string() } else { grants.join(", ") })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn asked<T>(api: &mut HubApi, here: &mut Here, call: impl FnOnce(mpsc::Sender<T>) -> Call) -> T {
        let (reply, answer) = mpsc::channel();
        api.answer(call(reply), here);
        answer.recv().expect("answered")
    }

    #[test]
    fn a_hub_answers_about_its_library_and_refuses_to_play() {
        let dir = std::env::temp_dir().join(format!("synththing-hub-api-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let music = dir.join("music");
        std::fs::create_dir_all(&music).unwrap();
        let (mut songs, mut playlists) = (SongStore::open(music.clone()), Library::load(dir.join("playlists"), music.clone()).0);
        let (mut wishlist, mut sync, config) = (Wishlist::default(), DeviceSync::default(), Config::default());
        let here = &mut Here { songs: &mut songs, playlists: &mut playlists, wishlist: &mut wishlist, sync: &mut sync, config: &config };
        let mut api = HubApi::default();

        let refused = asked(&mut api, here, |reply| Call::Play { song: None, playlist: None, reply });
        assert_eq!(refused.unwrap_err().0, 409);
        assert!(asked(&mut api, here, |reply| Call::Command(crate::commands::Command::Pause, reply)).is_err());

        // (A song a program sent: in the library, marked as its, its file read.)
        let midi = b"MThd\0\0\0\x06\0\0\0\x01\0\x60MTrk\0\0\0\x04\0\xff\x2f\0";
        let stored = crate::song_store::write_in(midi, "Theme.mid", &music).unwrap();
        let key = stored.key.clone();
        let (song, new) = asked(&mut api, here, |reply| Call::Add { stored, program: "bridge".into(), online: None, reply }).unwrap();
        assert!(new);
        assert_eq!(song.added_by.as_deref(), Some("bridge"));
        assert!(here.songs.get_by_key(&key).is_some_and(|s| s.meta.is_some()), "its file was read");
        let library = asked(&mut api, here, Call::Library);
        assert!(library.store.has(&key));
        assert!(library.queue.is_empty() && library.soundfonts.is_empty());

        // (Starred, put in a new playlist.)
        asked(&mut api, here, |reply| Call::Edit(Edit::Star { key: key.clone(), on: true }, reply)).unwrap();
        assert!(here.songs.get_by_key(&key).unwrap().tags.iter().any(|t| t == crate::song_store::STARRED));
        let made = asked(&mut api, here, |reply| Call::NewPlaylist("Mix".into(), reply)).unwrap();
        asked(&mut api, here, |reply| Call::Edit(Edit::Playlist { playlist: made.id.clone(), key: key.clone(), on: true }, reply)).unwrap();
        assert_eq!(api::playlist_infos(here.playlists, here.songs)[0].songs, std::slice::from_ref(&key));
        let missing = asked(&mut api, here, |reply| Call::Edit(Edit::Star { key: "nope".into(), on: true }, reply));
        assert_eq!(missing.unwrap_err().0, 404);

        // (Wished for, noted, unwished.)
        let wish = crate::wishlist::Wish::new(crate::wishlist::Kind::Album, "Nonagon Infinity");
        let (info, new) = asked(&mut api, here, |reply| Call::Wish { wish, release: None, program: "bridge".into(), reply }).unwrap();
        assert!(new);
        assert_eq!(info.wish.added_by.as_deref(), Some("bridge"));
        asked(&mut api, here, |reply| Call::Edit(Edit::Note { id: info.id.clone(), note: "from the bridge".into() }, reply)).unwrap();
        let listed = asked(&mut api, here, |reply| Call::Wishlist(String::new(), reply));
        assert_eq!((listed.len(), listed[0].wish.note.as_str()), (1, "from the bridge"));
        asked(&mut api, here, |reply| Call::Edit(Edit::Unwish { id: info.id.clone() }, reply)).unwrap();
        assert!(asked(&mut api, here, |reply| Call::Wishlist(String::new(), reply)).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
