//! One running app per install: per config folder, so stable and a branch
//! build (docs/design/release-channels.md) can run side by side, but two
//! copies of the same one can't save over each other's settings.
//!
//! The first to start holds an exclusive lock on `synththing.lock` in the
//! config folder for as long as it runs (the OS lets go of it however the
//! process ends, a crash too). A second one finds it locked, leaves a
//! `show-request` file there and quits; the first, which checks for that
//! file a few times a second, brings its window back (out of the tray,
//! say). A file rather than a socket: nothing to open on the network, no
//! firewall prompt. Command-line commands (`run-script`, `serve`, ...)
//! don't take part.
//!
//! A restart (after an update, `updater::relaunch`) starts the new app
//! before the old one has closed; started with `RELAUNCH_VAR`, it waits for
//! the lock instead of handing over to the app that's on its way out.

use std::fs::{self, File};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

/// How often the running app looks for a second launch's request.
const POLL: Duration = Duration::from_millis(300);

/// Set on an app started to replace this one (a restart): it waits for the
/// lock.
pub const RELAUNCH_VAR: &str = "SYNTHTHING_RELAUNCH";

/// How long a restarted app waits for the old one to let go.
const RELAUNCH_WAIT: Duration = Duration::from_secs(10);

/// Held for as long as this app runs: the lock is let go when it's dropped
/// (or the process ends).
pub struct Guard {
    _file: File,
    request: PathBuf,
}

/// Whether this is the app for its config folder.
pub enum Claim {
    /// It is: keep the guard until the app closes.
    First(Guard),
    /// Another one's running; it was asked to come to the front.
    Running,
}

/// Take the lock for this config folder, or, if another app has it, ask
/// that one to show itself.
pub fn claim() -> Result<Claim> {
    let dir = crate::config::config_dir()?;
    fs::create_dir_all(&dir).with_context(|| format!("making {}", dir.display()))?;
    let path = dir.join("synththing.lock");
    let request = dir.join("show-request");
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    let deadline = std::env::var_os(RELAUNCH_VAR).map(|_| Instant::now() + RELAUNCH_WAIT);
    loop {
        match file.try_lock() {
            Ok(()) => {
                // (Left over from a launch the last app never saw.)
                let _ = fs::remove_file(&request);
                return Ok(Claim::First(Guard { _file: file, request }));
            }
            Err(fs::TryLockError::WouldBlock) if deadline.is_some_and(|d| Instant::now() < d) => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(fs::TryLockError::WouldBlock) => {
                fs::write(&request, std::process::id().to_string())
                    .with_context(|| format!("writing {}", request.display()))?;
                return Ok(Claim::Running);
            }
            Err(fs::TryLockError::Error(e)) => return Err(e).with_context(|| format!("locking {}", path.display())),
        }
    }
}

impl Guard {
    /// Watch for a second launch, in the background: the flag is raised
    /// and the app woken (it may be hidden, with nothing else happening)
    /// each time one asks.
    pub fn listen(&self, ctx: &eframe::egui::Context) -> Arc<AtomicBool> {
        let asked = Arc::new(AtomicBool::new(false));
        let (flag, request, ctx) = (Arc::clone(&asked), self.request.clone(), ctx.clone());
        let spawned = std::thread::Builder::new().name("second-launch".into()).spawn(move || {
            loop {
                std::thread::sleep(POLL);
                if fs::remove_file(&request).is_ok() {
                    flag.store(true, Ordering::Relaxed);
                    ctx.request_repaint();
                }
            }
        });
        if let Err(e) = spawned {
            log::warn!("not watching for a second launch: {e}");
        }
        asked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The second claim on a folder finds the first and leaves it a
    /// request; once the first is gone, the folder can be claimed again.
    #[test]
    fn one_app_per_folder() {
        let dir = std::env::temp_dir().join(format!("synththing-instance-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        crate::config::set_config_dir(&dir).unwrap();
        // (Other tests may have set the folder first: use whichever won.)
        let dir = crate::config::config_dir().unwrap();
        let _ = fs::remove_file(dir.join("show-request"));

        let Claim::First(guard) = claim().unwrap() else { panic!("the first claim gets it") };
        assert!(matches!(claim().unwrap(), Claim::Running));
        assert!(dir.join("show-request").exists(), "the second asked the first to show");
        drop(guard);
        assert!(matches!(claim().unwrap(), Claim::First(_)), "free again");
        assert!(!dir.join("show-request").exists(), "a stale request is cleared");
    }
}
