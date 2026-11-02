//! What's playing, shown on Discord as "Listening to synththing" (rich
//! presence). Desktop only. Talks to the Discord app running on this
//! computer over its local pipe (`discord-rich-presence`), from a thread of
//! its own, so a missing or slow Discord never holds up the app.
//!
//! Fed the same `media_controls::Shown` the system's media controls are,
//! when it changes (`app/api.rs`).

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use discord_rich_presence::activity::{Activity, ActivityType, Assets, Timestamps};
use discord_rich_presence::{DiscordIpc, DiscordIpcClient};

use crate::media_controls::Shown;

/// The synththing application in Discord's Developer Portal. Not a secret.
const CLIENT_ID: &str = "1566746063392477246";

/// Its uploaded logo (Rich Presence > Art Assets), shown when the song
/// has no cover online.
const LOGO: &str = "logo";

/// How often Discord's looked for again while it isn't running.
const RETRY: Duration = Duration::from_secs(15);

/// The least time between two updates: Discord takes about 5 in 20 s.
const MIN_GAP: Duration = Duration::from_secs(4);

/// What Discord's shown: nothing (`None` to `Discord::show`) when
/// nothing's playing.
#[derive(Clone, Debug, PartialEq)]
pub struct Presence {
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// Its cover, as a picture online.
    pub cover: Option<String>,
    /// When the song started and will end (Unix milliseconds), as it's
    /// playing now; no end when its length isn't known.
    pub start: i64,
    pub end: Option<i64>,
}

impl Presence {
    /// What `shown` looks like on Discord, at `speed`: nothing unless a
    /// song's playing.
    pub fn of(shown: &Shown, speed: f64, cover: Option<String>) -> Option<Presence> {
        if !shown.loaded || !shown.playing {
            return None;
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as i64;
        let speed = if speed > 0.0 { speed } else { 1.0 };
        let ms = |seconds: f64| (seconds / speed * 1000.0) as i64;
        Some(Presence {
            title: shown.title.clone().unwrap_or_else(|| "Something".into()),
            artist: shown.artist.clone(),
            album: shown.album.clone(),
            cover,
            start: now - ms(shown.position),
            end: (shown.length > 0.0).then(|| now + ms(shown.length - shown.position)),
        })
    }
}

/// The thread that keeps Discord up to date, while this is kept.
pub struct Discord {
    tx: Sender<Option<Presence>>,
}

impl Discord {
    pub fn new() -> Discord {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("discord".into())
            .spawn(move || run(rx))
            .expect("spawn the Discord thread");
        Discord { tx }
    }

    pub fn show(&self, presence: Option<Presence>) {
        let _ = self.tx.send(presence);
    }
}

fn run(rx: Receiver<Option<Presence>>) {
    let mut client: Option<DiscordIpcClient> = None;
    let mut wanted: Option<Presence> = None;
    // What Discord was last shown (`Some(None)`: nothing), and when.
    let mut sent: Option<Option<Presence>> = None;
    let mut last_try: Option<Instant> = None;
    let mut last_sent: Option<Instant> = None;
    loop {
        let wait = if client.is_none() { RETRY } else { MIN_GAP };
        match rx.recv_timeout(wait) {
            Ok(presence) => wanted = presence,
            Err(RecvTimeoutError::Timeout) => {}
            // (Turned off, or the app's closing.)
            Err(RecvTimeoutError::Disconnected) => break,
        }
        // (The newest only.)
        while let Ok(presence) = rx.try_recv() {
            wanted = presence;
        }
        if client.is_none() {
            if last_try.is_some_and(|at| at.elapsed() < RETRY) {
                continue;
            }
            last_try = Some(Instant::now());
            let mut new = DiscordIpcClient::new(CLIENT_ID);
            if new.connect().is_err() {
                continue;
            }
            log::info!("discord: connected");
            client = Some(new);
            sent = None;
        }
        if sent.as_ref() == Some(&wanted) || last_sent.is_some_and(|at| at.elapsed() < MIN_GAP) {
            continue;
        }
        let Some(connected) = client.as_mut() else {
            continue;
        };
        let result = match &wanted {
            Some(presence) => connected.set_activity(activity(presence)),
            None => connected.clear_activity(),
        };
        last_sent = Some(Instant::now());
        match result {
            Ok(()) => sent = Some(wanted.clone()),
            Err(e) => {
                log::info!("discord: lost ({e})");
                client = None;
            }
        }
    }
    if let Some(mut client) = client {
        let _ = client.clear_activity();
        let _ = client.close();
    }
}

fn activity(presence: &Presence) -> Activity<'_> {
    let mut timestamps = Timestamps::new().start(presence.start);
    if let Some(end) = presence.end {
        timestamps = timestamps.end(end);
    }
    let mut assets = Assets::new().large_image(presence.cover.as_deref().unwrap_or(LOGO));
    if let Some(album) = &presence.album {
        assets = assets.large_text(album.as_str());
    }
    let mut activity = Activity::new()
        .activity_type(ActivityType::Listening)
        .details(presence.title.as_str())
        .timestamps(timestamps)
        .assets(assets);
    if let Some(artist) = &presence.artist {
        activity = activity.state(artist.as_str());
    }
    activity
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(playing: bool) -> Shown {
        Shown {
            title: Some("Song".into()),
            length: 100.0,
            position: 40.0,
            playing,
            loaded: true,
            ..Shown::default()
        }
    }

    #[test]
    fn only_a_playing_song_shows() {
        assert_eq!(Presence::of(&shown(false), 1.0, None), None);
        assert_eq!(Presence::of(&Shown::default(), 1.0, None), None);
        let presence = Presence::of(&shown(true), 1.0, None).unwrap();
        assert_eq!(presence.end.unwrap() - presence.start, 100_000);
    }

    #[test]
    fn speed_shortens_the_song() {
        let presence = Presence::of(&shown(true), 2.0, None).unwrap();
        assert_eq!(presence.end.unwrap() - presence.start, 50_000);
    }
}
