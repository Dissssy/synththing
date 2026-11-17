//! Last.fm: the song playing shown on the user's profile there, and the
//! songs they listen to added to their listening history (what Last.fm's
//! API calls "now playing" and "scrobbles"; the app never says the
//! latter). Off until the user connects an account (Preferences >
//! Integrations), and then on each device that plays, as that device has
//! it. The app talks to Last.fm directly, from a thread of its own;
//! listens wait in a file (`lastfm-queue.json`) while it can't be reached.
//!
//! Connecting is Last.fm's desktop flow: the app gets a token, the user
//! approves it on last.fm in their browser, and the app trades it for a
//! session key, which doesn't expire (the user can revoke it there). The
//! user's password never passes through the app.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};

const API: &str = "https://ws.audioscrobbler.com/2.0/";

/// SynthThing's API account at Last.fm. The secret signs calls; like
/// every desktop app's, it ships in the app.
const API_KEY: &str = "dc0e1fd943fbabe1c1842de232cf6593";
const SECRET: &str = "1c3925d0e34d8a9f92c14ae7c7d0b591";

/// The live message (`device_messages.rs`) that hands an `Account` to the
/// user's other devices, when they ask to connect those too. Sealed with
/// the sync key like every live message, and never stored.
pub const TOPIC: &str = "lastfm";

/// Where the user approves the app: `?api_key=…&token=…`.
const APPROVE: &str = "https://www.last.fm/api/auth/";

/// Listens sent together, at most (Last.fm takes 50 a call).
const BATCH: usize = 50;

/// Last.fm ignores listens older than this, so they aren't kept.
const OLDEST: i64 = 14 * 24 * 60 * 60;

/// How long it's waited before trying again after Last.fm couldn't be
/// reached: doubling from the first, up to the last.
const RETRY_FIRST: Duration = Duration::from_secs(30);
const RETRY_LAST: Duration = Duration::from_secs(30 * 60);

/// How often an approval's checked for while connecting, and for how long.
const APPROVAL_CHECKS: Duration = Duration::from_secs(3);
const APPROVAL_WAIT: Duration = Duration::from_secs(10 * 60);

/// Last.fm's error codes the app acts on.
const NOT_APPROVED_YET: u32 = 14;
const SESSION_REVOKED: u32 = 9;

/// The user's Last.fm preferences (`Config::lastfm`), kept on this device.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The connected account, if one is.
    pub account: Option<Account>,
    /// Show the song playing on the user's profile.
    pub now_playing: bool,
    /// Add the songs listened to to their history.
    pub history: bool,
}

impl Settings {
    /// The account, while anything's sent to it.
    pub fn sending(&self) -> Option<&Account> {
        self.account.as_ref().filter(|_| self.now_playing || self.history)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Account {
    /// Their Last.fm user name.
    pub name: String,
    /// The session key the app signs calls with.
    pub key: String,
}

/// A song, as Last.fm's told of it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub artist: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_artist: Option<String>,
    /// Whole seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<u32>,
    /// Its MusicBrainz recording.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mbid: Option<String>,
}

impl Track {
    /// The params for `track.updateNowPlaying`, or the `i`th song of a
    /// `track.scrobble` (`Some(i)`).
    fn params(&self, i: Option<usize>, into: &mut BTreeMap<String, String>) {
        let name = |n: &str| i.map_or_else(|| n.to_string(), |i| format!("{n}[{i}]"));
        into.insert(name("artist"), self.artist.clone());
        into.insert(name("track"), self.title.clone());
        let optional = [("album", self.album.clone()), ("albumArtist", self.album_artist.clone()), ("duration", self.length.map(|l| l.to_string())), ("mbid", self.mbid.clone())];
        for (key, value) in optional {
            if let Some(value) = value {
                into.insert(name(key), value);
            }
        }
    }
}

/// A song listened to, and when it started (Unix seconds).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Listen {
    pub track: Track,
    pub at: i64,
}

/// Last.fm's rule for a song that counts as listened to: longer than 30
/// seconds, and played half way or 4 minutes in, whichever's first.
pub fn counts(position: f64, length: f64) -> bool {
    length > 30.0 && position >= (length / 2.0).min(240.0)
}

/// The song playing, as Last.fm would count it: like
/// `play_stats::Listening`, but always to Last.fm's rule (`counts`), not
/// the user's play counts.
#[derive(Debug, Default)]
pub struct Heard {
    counted: bool,
}

impl Heard {
    /// A song started.
    pub fn start(&mut self) {
        self.counted = false;
    }

    /// Where it is now (seconds in, of `length`; `finished` at its end):
    /// whether it just came to count. Going back before that (looping, or
    /// seeking) lets it count again.
    pub fn at(&mut self, position: f64, length: f64, finished: bool) -> bool {
        let past = if finished { length > 30.0 } else { counts(position, length) };
        let newly = past && !self.counted;
        self.counted = past;
        newly
    }
}

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}

/// How the thread's doing, for Preferences.
#[derive(Clone, Debug, Default)]
pub struct Status {
    /// Listens not sent yet.
    pub waiting: usize,
    /// When some last went.
    pub sent: Option<Instant>,
    /// Why the last try failed, until one works.
    pub problem: Option<String>,
    /// Last.fm said the account's no longer connected (revoked there).
    pub revoked: bool,
}

enum Message {
    NowPlaying(Track),
    Listened(Listen),
}

/// The thread that tells Last.fm, while this is kept.
pub struct Lastfm {
    tx: Sender<Message>,
    status: Arc<Mutex<Status>>,
    /// The account it sends to.
    pub key: String,
}

impl Lastfm {
    pub fn new(key: String) -> Lastfm {
        let (tx, rx) = mpsc::channel();
        let status = Arc::new(Mutex::new(Status::default()));
        let worker = Worker { key: key.clone(), queue: Queue::load(), status: Arc::clone(&status), retry: None };
        std::thread::Builder::new().name("lastfm".into()).spawn(move || worker.run(rx)).expect("spawn the Last.fm thread");
        Lastfm { tx, status, key }
    }

    pub fn now_playing(&self, track: Track) {
        let _ = self.tx.send(Message::NowPlaying(track));
    }

    pub fn listened(&self, listen: Listen) {
        let _ = self.tx.send(Message::Listened(listen));
    }

    pub fn status(&self) -> Status {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

/// Forget the listens waiting (the account was disconnected).
pub fn forget_waiting() {
    if let Some(path) = Queue::path() {
        let _ = std::fs::remove_file(path);
    }
}

/// The listens waiting to be sent, kept in a file so they outlast the app.
struct Queue(Vec<Listen>);

impl Queue {
    fn path() -> Option<PathBuf> {
        crate::config::config_dir().ok().map(|dir| dir.join("lastfm-queue.json"))
    }

    fn load() -> Queue {
        let listens = Self::path().and_then(|p| std::fs::read(p).ok()).and_then(|data| serde_json::from_slice(&data).ok());
        Queue(listens.unwrap_or_default())
    }

    fn save(&self) {
        let Some(path) = Self::path() else { return };
        let saved = if self.0.is_empty() {
            std::fs::remove_file(&path).or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) })
        } else {
            serde_json::to_vec(&self.0).map_err(std::io::Error::other).and_then(|data| std::fs::write(&path, data))
        };
        if let Err(e) = saved {
            log::warn!("lastfm: saving the listens waiting: {e}");
        }
    }

    /// Drop the ones Last.fm would ignore for their age.
    fn drop_old(&mut self) {
        let oldest = now() - OLDEST;
        self.0.retain(|listen| listen.at >= oldest);
    }
}

struct Worker {
    key: String,
    queue: Queue,
    status: Arc<Mutex<Status>>,
    /// After a failure: when to try again, and the wait after that.
    retry: Option<(Instant, Duration)>,
}

impl Worker {
    fn run(mut self, rx: Receiver<Message>) {
        self.queue.drop_old();
        self.report(|_| {});
        loop {
            let wait = match self.retry {
                Some((at, _)) => at.saturating_duration_since(Instant::now()),
                None if self.queue.0.is_empty() => Duration::MAX,
                None => Duration::ZERO,
            };
            let mut playing = None;
            match rx.recv_timeout(wait) {
                Ok(message) => self.take(message, &mut playing),
                Err(RecvTimeoutError::Timeout) => {}
                // (Turned off, or the app's closing.)
                Err(RecvTimeoutError::Disconnected) => break,
            }
            while let Ok(message) = rx.try_recv() {
                self.take(message, &mut playing);
            }
            if self.status.lock().is_ok_and(|s| s.revoked) {
                continue;
            }
            if let Some(track) = playing {
                let mut params = BTreeMap::new();
                track.params(None, &mut params);
                if let Err(e) = self.call("track.updateNowPlaying", params) {
                    // (Not tried again: it'd be out of date by then.)
                    log::info!("lastfm: now playing: {e}");
                    self.failed(&e, false);
                }
            }
            if self.retry.is_none_or(|(at, _)| at <= Instant::now()) {
                self.send_waiting();
            }
        }
    }

    fn take(&mut self, message: Message, playing: &mut Option<Track>) {
        match message {
            Message::NowPlaying(track) => *playing = Some(track),
            Message::Listened(listen) => {
                self.queue.0.push(listen);
                self.queue.save();
                // (Sent now, unless it's waiting to try again.)
                self.report(|_| {});
            }
        }
    }

    fn send_waiting(&mut self) {
        self.queue.drop_old();
        while !self.queue.0.is_empty() {
            let count = self.queue.0.len().min(BATCH);
            let mut params = BTreeMap::new();
            for (i, listen) in self.queue.0[..count].iter().enumerate() {
                listen.track.params(Some(i), &mut params);
                params.insert(format!("timestamp[{i}]"), listen.at.to_string());
            }
            match self.call("track.scrobble", params) {
                // (Each is taken or ignored, as the reply says: either
                // way, it's done with.)
                Ok(_) => {
                    self.queue.0.drain(..count);
                    self.queue.save();
                    self.retry = None;
                    self.report(|s| {
                        s.sent = Some(Instant::now());
                        s.problem = None;
                    });
                }
                Err(e) => {
                    log::info!("lastfm: sending {count} listens: {e}");
                    self.failed(&e, true);
                    return;
                }
            }
        }
        self.retry = None;
        self.queue.save();
        self.report(|_| {});
    }

    /// A call failed: wait before sending the listens again (`backoff`),
    /// and say why.
    fn failed(&mut self, e: &Error, backoff: bool) {
        let revoked = matches!(e, Error::Api(SESSION_REVOKED, _));
        if backoff {
            let wait = self.retry.map_or(RETRY_FIRST, |(_, wait)| (wait * 2).min(RETRY_LAST));
            self.retry = Some((Instant::now() + wait, wait));
        }
        self.report(|s| {
            s.problem = Some(e.to_string());
            s.revoked |= revoked;
        });
    }

    fn report(&self, change: impl FnOnce(&mut Status)) {
        if let Ok(mut status) = self.status.lock() {
            status.waiting = self.queue.0.len();
            change(&mut status);
        }
    }

    fn call(&self, method: &str, mut params: BTreeMap<String, String>) -> Result<serde_json::Value, Error> {
        params.insert("sk".into(), self.key.clone());
        call(method, params)
    }
}

#[derive(Debug)]
pub enum Error {
    /// Last.fm said no: its error code and message.
    Api(u32, String),
    /// It couldn't be reached, or answered oddly.
    Http(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Api(code, message) => write!(f, "{message} ({code})"),
            Error::Http(e) => write!(f, "{e}"),
        }
    }
}

/// `params` signed as Last.fm has it: every param but `format`, sorted by
/// name, each name then value, then the secret; MD5, in hex.
fn signature(params: &BTreeMap<String, String>, secret: &str) -> String {
    let mut md5 = Md5::new();
    for (name, value) in params {
        md5.update(name.as_bytes());
        md5.update(value.as_bytes());
    }
    md5.update(secret.as_bytes());
    md5.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Call a Last.fm method, signed.
fn call(method: &str, mut params: BTreeMap<String, String>) -> Result<serde_json::Value, Error> {
    params.insert("method".into(), method.into());
    params.insert("api_key".into(), API_KEY.into());
    let signature = signature(&params, SECRET);
    params.insert("api_sig".into(), signature);
    params.insert("format".into(), "json".into());
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(20)))
        .user_agent(format!("synththing/{}", crate::build_info::VERSION))
        .build()
        .into();
    let mut response = agent.post(API).send_form(params.iter().map(|(k, v)| (k.as_str(), v.as_str()))).map_err(|e| Error::Http(e.to_string()))?;
    let status = response.status();
    let body: serde_json::Value = response.body_mut().read_json().map_err(|e| Error::Http(format!("{status}: {e}")))?;
    if let Some(code) = body.get("error").and_then(|c| c.as_u64()) {
        let message = body.get("message").and_then(|m| m.as_str()).unwrap_or("Last.fm said no");
        return Err(Error::Api(code as u32, message.to_string()));
    }
    if !status.is_success() {
        return Err(Error::Http(status.to_string()));
    }
    Ok(body)
}

/// Connecting an account, from asking Last.fm for a token to the session
/// key, on a thread of its own; dropping it gives up.
pub struct Connecting {
    state: Arc<Mutex<Connect>>,
}

#[derive(Clone, Debug)]
pub enum Connect {
    /// Asking Last.fm for a token.
    Starting,
    /// Waiting for the user to approve the app at `url`.
    Approving { url: String },
    Done(Result<Account, String>),
}

impl Connecting {
    /// Start; `wake` is called when it's moved on.
    pub fn start(wake: impl Fn() + Send + 'static) -> Connecting {
        let state = Arc::new(Mutex::new(Connect::Starting));
        let shared = Arc::clone(&state);
        std::thread::Builder::new()
            .name("lastfm-connect".into())
            .spawn(move || {
                let result = connect(&shared, &wake);
                if let Ok(mut state) = shared.lock() {
                    *state = Connect::Done(result);
                }
                wake();
            })
            .expect("spawn the Last.fm connect thread");
        Connecting { state }
    }

    pub fn state(&self) -> Connect {
        self.state.lock().map(|s| s.clone()).unwrap_or(Connect::Starting)
    }
}

fn connect(state: &Arc<Mutex<Connect>>, wake: &impl Fn()) -> Result<Account, String> {
    let token = call("auth.getToken", BTreeMap::new()).map_err(|e| e.to_string())?;
    let token = token.get("token").and_then(|t| t.as_str()).ok_or("Last.fm sent no token")?.to_string();
    if let Ok(mut state) = state.lock() {
        *state = Connect::Approving { url: format!("{APPROVE}?api_key={API_KEY}&token={token}") };
    }
    wake();
    let started = Instant::now();
    while started.elapsed() < APPROVAL_WAIT {
        std::thread::sleep(APPROVAL_CHECKS);
        // (Given up on: nothing's waiting for it.)
        if Arc::strong_count(state) == 1 {
            return Err("cancelled".into());
        }
        match call("auth.getSession", BTreeMap::from([("token".to_string(), token.clone())])) {
            Ok(reply) => {
                let session = reply.get("session").ok_or("Last.fm sent no session")?;
                let field = |name: &str| session.get(name).and_then(|v| v.as_str()).map(str::to_string);
                return match (field("name"), field("key")) {
                    (Some(name), Some(key)) => Ok(Account { name, key }),
                    _ => Err("Last.fm sent no session".into()),
                };
            }
            Err(Error::Api(NOT_APPROVED_YET, _)) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("it wasn't approved in time".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_as_lastfm_documents() {
        let params = BTreeMap::from([("method".to_string(), "auth.getSession".to_string()), ("api_key".to_string(), "k".to_string()), ("token".to_string(), "t".to_string())]);
        let mut md5 = Md5::new();
        md5.update(b"api_keykmethodauth.getSessiontokentsecret");
        let expected: String = md5.finalize().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(signature(&params, "secret"), expected);
    }

    #[test]
    fn half_way_or_four_minutes_counts() {
        assert!(!counts(29.0, 30.0), "too short");
        assert!(!counts(49.0, 100.0));
        assert!(counts(50.0, 100.0));
        assert!(counts(240.0, 1000.0), "four minutes is enough");
    }

    #[test]
    fn heard_once_until_it_goes_back() {
        let mut heard = Heard::default();
        assert!(!heard.at(10.0, 100.0, false));
        assert!(heard.at(60.0, 100.0, false));
        assert!(!heard.at(90.0, 100.0, false), "already counted");
        assert!(!heard.at(5.0, 100.0, false));
        assert!(heard.at(60.0, 100.0, false), "looped");
        heard.start();
        assert!(heard.at(100.0, 100.0, true), "played to the end");
    }

    #[test]
    fn batch_params_are_numbered() {
        let track = Track { artist: "A".into(), title: "T".into(), album: None, album_artist: None, length: Some(200), mbid: None };
        let mut params = BTreeMap::new();
        track.params(Some(3), &mut params);
        assert_eq!(params.get("artist[3]").map(String::as_str), Some("A"));
        assert_eq!(params.get("duration[3]").map(String::as_str), Some("200"));
        assert!(!params.contains_key("album[3]"));
    }
}
