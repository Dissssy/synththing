//! Where this program was started from, read once at startup. An update
//! swaps the file while it runs, and on Linux asking afterwards gives
//! "<path> (deleted)", which can't be started, copied or pointed to.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{anyhow, Result};

static PATH: LazyLock<std::io::Result<PathBuf>> = LazyLock::new(std::env::current_exe);

/// Read the path now, before anything can replace the file.
pub fn capture() {
    LazyLock::force(&PATH);
}

/// This program's path, as it was at startup.
pub fn path() -> Result<&'static Path> {
    PATH.as_deref().map_err(|e| anyhow!("finding this program: {e}"))
}

/// The path for texts: empty when it couldn't be found.
pub fn display() -> String {
    path().map(|p| p.display().to_string()).unwrap_or_default()
}
