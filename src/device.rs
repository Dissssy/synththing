//! This install of the app, as other devices signed in as the same
//! identity see it (`device.json` in the config folder). Per device: it
//! never leaves it, except as the name a sign-in shows.

use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Device {
    /// Shown on the device that signed this one in ("Signed in on ...").
    pub name: String,
    /// Tells this device apart from the others in synced data (16 hex
    /// digits, made the first time).
    pub id: String,
}

impl Device {
    fn path() -> Result<PathBuf> {
        Ok(crate::config::config_dir()?.join("device.json"))
    }

    /// The saved one, or (the first time) one named after the computer,
    /// with an ID made and saved if it has none.
    pub fn load() -> Self {
        let mut device = Self::load_saved();
        if device.id.is_empty() {
            device.id = crate::library::new_nonce()[..16].to_string();
            if let Err(e) = device.save() {
                log::warn!("saving device.json: {e:#}");
            }
        }
        device
    }

    fn load_saved() -> Self {
        let saved = Self::path().and_then(|p| crate::config::read_json_or_set_aside::<Self>(&p)).unwrap_or_else(|e| {
            log::warn!("{e:#}");
            None
        });
        saved.unwrap_or_else(|| {
            #[cfg(target_os = "android")]
            let name = crate::android::device_model().unwrap_or_else(|| "Android phone".into());
            #[cfg(not(target_os = "android"))]
            let name = gethostname::gethostname().to_string_lossy().into_owned();
            Self { name, id: String::new() }
        })
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}
