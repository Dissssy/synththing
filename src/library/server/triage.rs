//! Triage (docs/SERVER.md, docs/feedback-triage.md): each new note of
//! feedback is pushed to a Claude Code routine, whose run files it as a
//! GitHub issue (or adds it to one), when `triage` is set in
//! `server.json`. The routine's API trigger is a URL and a bearer token;
//! the token is read from a file in the data folder, like SendGrid's key.
//!
//! The feedback table is the queue: a note is pushed once the routine has
//! taken it (`pushed`). One thread pushes, oldest first, when feedback
//! comes in and every 10 minutes, so notes sent while the routine can't
//! be reached (or is over its limit) go once it can.

use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;

use rusqlite::params;

use super::{now, State, TriageConfig};

pub(super) const SCHEMA_V14: &str = "
BEGIN;
ALTER TABLE feedback ADD COLUMN pushed INTEGER;
PRAGMA user_version = 14;
COMMIT;
";

/// How often notes that couldn't be pushed are tried again.
const RETRY: Duration = Duration::from_secs(10 * 60);
/// How much of the end of a note's log goes with it.
const LOG_TAIL: usize = 8 * 1024;

/// Where to push, and with what token.
pub(super) struct Routine {
    url: String,
    token: String,
}

impl Routine {
    /// From the settings: the token from `token_file` (in `data` unless
    /// it's absolute), or none if triage isn't set.
    pub(super) fn new(config: Option<&TriageConfig>, data: &Path) -> Result<Option<Self>, String> {
        let Some(config) = config else { return Ok(None) };
        if config.url.is_empty() {
            return Err("triage needs the routine's url".into());
        }
        let path = data.join(&config.token_file);
        let token = std::fs::read_to_string(&path)
            .map_err(|e| format!("couldn't read the triage routine's token from {}: {e}", path.display()))?;
        Ok(Some(Self { url: config.url.clone(), token: token.trim().to_string() }))
    }
}

/// Start the thread that pushes, woken by `woken` (`State::triage`) and
/// at once, for whatever was waiting when the server last stopped.
pub(super) fn start(state: &Arc<State>, routine: Routine, woken: Receiver<()>) -> Result<(), String> {
    let state = Arc::clone(state);
    std::thread::Builder::new()
        .name("triage".into())
        .spawn(move || {
            push_waiting(&state, &routine);
            push_loop(&state, &routine, &woken);
        })
        .map(drop)
        .map_err(|e| format!("couldn't start: {e}"))
}

/// Wake the pushing thread: there's new feedback.
pub(super) fn wake(state: &State) {
    if let Some(wake) = &state.triage {
        let _ = wake.send(());
    }
}

fn push_loop(state: &State, routine: &Routine, woken: &Receiver<()>) {
    loop {
        match woken.recv_timeout(RETRY) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => push_waiting(state, routine),
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Push the notes not pushed yet (and not dealt with), oldest first,
/// stopping at the first the routine doesn't take.
fn push_waiting(state: &State, routine: &Routine) {
    loop {
        let next = state
            .db()
            .query_row(
                "SELECT id, text, app_version, channel, os, log, created FROM feedback
                 WHERE pushed IS NULL AND handled IS NULL ORDER BY id LIMIT 1",
                [],
                |r| {
                    let note = Note {
                        id: r.get(0)?,
                        text: r.get(1)?,
                        app_version: r.get(2)?,
                        channel: r.get(3)?,
                        os: r.get(4)?,
                        log: r.get(5)?,
                        created: r.get(6)?,
                    };
                    Ok(note)
                },
            )
            .ok();
        let Some(note) = next else { return };
        if let Err(e) = fire(routine, &note.payload()) {
            println!("couldn't push feedback #{} to triage (trying again later): {e}", note.id);
            return;
        }
        println!("pushed feedback #{} to triage", note.id);
        if let Err(e) = state.db().execute("UPDATE feedback SET pushed = ? WHERE id = ?", params![now(), note.id]) {
            println!("couldn't mark feedback #{} pushed: {e}", note.id);
            return;
        }
    }
}

struct Note {
    id: i64,
    text: String,
    app_version: String,
    channel: String,
    os: String,
    log: Option<String>,
    created: i64,
}

impl Note {
    /// What the routine's run is given (as untrusted text): the note, its
    /// build and the end of its log. Not its address.
    fn payload(&self) -> String {
        let mut payload = format!(
            "synththing feedback #{}\nsent: {} (unix time)\nchannel: {}\nversion: {}\nos: {}\n\n--- feedback ---\n{}\n",
            self.id, self.created, self.channel, self.app_version, self.os, self.text
        );
        if let Some(log) = &self.log {
            let mut start = log.len().saturating_sub(LOG_TAIL);
            while !log.is_char_boundary(start) {
                start += 1;
            }
            payload.push_str(&format!("\n--- the app's log (its last {} bytes) ---\n{}\n", log.len() - start, &log[start..]));
        }
        payload
    }
}

/// Fire the routine with `text`.
fn fire(routine: &Routine, text: &str) -> Result<(), String> {
    let agent: ureq::Agent =
        ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(30))).build().into();
    let mut response = agent
        .post(&routine.url)
        .header("Authorization", format!("Bearer {}", routine.token))
        .header("anthropic-beta", "experimental-cc-routine-2026-04-01")
        .header("anthropic-version", "2023-06-01")
        .send_json(serde_json::json!({ "text": text }))
        .map_err(|e| format!("couldn't reach it: {e}"))?;
    let status = response.status().as_u16();
    if status < 300 {
        return Ok(());
    }
    let said: String = response.body_mut().read_to_string().unwrap_or_default().chars().take(300).collect();
    Err(format!("it said {status}: {said}"))
}
