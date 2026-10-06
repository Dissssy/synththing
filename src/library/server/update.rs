//! Updating a running server from an admin's request (docs/SERVER.md):
//! `synththing admin update`, or the Moderation window's Server tab.
//!
//! How depends on how it's run. Under Docker, it can't: the image is what
//! updates (pull it, recreate the container). Under systemd (set up as in
//! docs/DEPLOY.md), the service can't write its own binary, so it asks the
//! root-run update script to: it writes `update.request` in its data
//! folder, which `synththing-update.path` watches. Run by hand, it updates
//! itself with the app's updater (download, check the SHA-256 GitHub
//! publishes, swap the binary), starts the new version and exits.

use std::path::Path;
use std::time::Duration;

use super::{now, State};
use crate::library::ServerVersion;
use crate::updater;

/// How this server's run: `"docker"`, `"systemd"` or `"by hand"`.
pub(super) fn how_run() -> &'static str {
    if std::env::var_os("SYNTHTHING_DOCKER").is_some() {
        "docker"
    } else if std::env::var_os("INVOCATION_ID").is_some() {
        "systemd"
    } else {
        "by hand"
    }
}

/// The version running, the newest release, and how it's run.
pub(super) fn check() -> ServerVersion {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let (latest, error) = match updater::latest_release() {
        Ok(release) => (Some(release.version.to_string()), None),
        Err(e) => (None, Some(format!("{e:#}"))),
    };
    let newer = latest.as_deref().is_some_and(|l| !crate::library::runs_on(l, &current));
    ServerVersion { current, latest, newer, how: how_run().to_string(), error }
}

/// The systemd unit that runs the update script when asked.
const PATH_UNIT: &str = "/etc/systemd/system/synththing-update.path";

/// Start updating to the newest release: what's happening, or why not.
pub(super) fn start(state: &State) -> Result<String, String> {
    let version = check();
    if let Some(error) = version.error {
        return Err(format!("couldn't check for a newer release: {error}"));
    }
    let latest = version.latest.unwrap_or_default();
    if !version.newer {
        return Err(format!("it's up to date ({})", version.current));
    }
    match version.how.as_str() {
        "docker" => Err(format!(
            "it runs in Docker: pull the new image ({latest}) and recreate the container (docker compose pull && \
             docker compose up -d)"
        )),
        "systemd" => {
            let request = state.data.join("update.request");
            std::fs::write(&request, now().to_string())
                .map_err(|e| format!("couldn't ask for the update ({}): {e}", request.display()))?;
            if Path::new(PATH_UNIT).exists() {
                println!("update to {latest} asked for: the update service takes it from here");
                Ok(format!("Updating to {latest}: the update service is on it, and the server restarts in a moment."))
            } else {
                Ok(format!(
                    "Asked for {latest}, but synththing-update.path isn't installed: the update timer will do it within \
                     the hour (or install the path unit, docs/DEPLOY.md)."
                ))
            }
        }
        _ => {
            println!("updating to {latest} (by hand)");
            let reply = format!("Updating to {latest}: it restarts by itself once it's in.");
            std::thread::spawn(move || {
                let installed = updater::latest_release().and_then(|release| updater::install_now(&release));
                match installed.and_then(|()| updater::relaunch()) {
                    Ok(()) => {
                        println!("updated to {latest}: starting it");
                        // (A moment for the reply to go out.)
                        std::thread::sleep(Duration::from_millis(500));
                        std::process::exit(0);
                    }
                    Err(e) => println!("the update to {latest} failed: {e:#}"),
                }
            });
            Ok(reply)
        }
    }
}
