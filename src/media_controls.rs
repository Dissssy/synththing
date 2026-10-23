//! The system's media controls (souvlaki): MPRIS on Linux, the media
//! overlay and media keys on Windows, Now Playing on macOS. Desktop only;
//! the Android app has its own media session.
//!
//! One more way into the local API's core (`app/api.rs`): what's playing
//! goes out to the system, and what's pressed there comes back as
//! `Control`s for the app to carry out, as it does API commands.

use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use eframe::egui;
use souvlaki::{MediaControlEvent, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig, SeekDirection};

/// What the system asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Control {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
    /// Seconds: relative with `relative`, else where to go.
    Seek {
        seconds: f64,
        relative: bool,
    },
    /// 0 to 1.
    Volume(f32),
    ShowWindow,
    Quit,
}

/// What the system is shown.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shown {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// Seconds.
    pub length: f64,
    pub position: f64,
    pub playing: bool,
    /// A song's loaded at all.
    pub loaded: bool,
    pub volume: f32,
}

pub struct MediaControls {
    controls: souvlaki::MediaControls,
    events: Receiver<Control>,
}

/// How far the system's plain seek buttons go.
const SEEK_STEP: f64 = 5.0;

impl MediaControls {
    /// Show synththing in the system's media controls; `window` is the
    /// main window's handle (an HWND, which Windows needs).
    pub fn new(window: Option<usize>, waker: egui::Context) -> Result<MediaControls, String> {
        let config = PlatformConfig { display_name: "synththing", dbus_name: "synththing", hwnd: window.map(|w| w as *mut std::ffi::c_void) };
        let mut controls = souvlaki::MediaControls::new(config).map_err(|e| format!("{e:?}"))?;
        let (tx, events) = mpsc::channel();
        controls
            .attach(move |event| {
                if let Some(control) = control(event) {
                    let _ = tx.send(control);
                    waker.request_repaint();
                }
            })
            .map_err(|e| format!("{e:?}"))?;
        Ok(MediaControls { controls, events })
    }

    /// What's been pressed since last asked.
    pub fn controls(&self) -> impl Iterator<Item = Control> + '_ {
        self.events.try_iter()
    }

    pub fn show(&mut self, shown: &Shown) {
        let metadata = MediaMetadata {
            title: shown.title.as_deref(),
            artist: shown.artist.as_deref(),
            album: shown.album.as_deref(),
            cover_url: None,
            duration: (shown.length > 0.0).then(|| Duration::from_secs_f64(shown.length)),
        };
        let progress = Some(MediaPosition(Duration::from_secs_f64(shown.position.max(0.0))));
        let playback = match (shown.loaded, shown.playing) {
            (false, _) => MediaPlayback::Stopped,
            (true, true) => MediaPlayback::Playing { progress },
            (true, false) => MediaPlayback::Paused { progress },
        };
        let shown_ok = self.controls.set_metadata(metadata).and_then(|()| self.controls.set_playback(playback));
        #[cfg(all(unix, not(target_os = "macos")))]
        let shown_ok = shown_ok.and_then(|()| self.controls.set_volume(shown.volume as f64));
        if let Err(e) = shown_ok {
            log::warn!("media controls: {e:?}");
        }
    }
}

fn control(event: MediaControlEvent) -> Option<Control> {
    let step = |direction| if direction == SeekDirection::Forward { 1.0 } else { -1.0 };
    Some(match event {
        MediaControlEvent::Play => Control::Play,
        MediaControlEvent::Pause | MediaControlEvent::Stop => Control::Pause,
        MediaControlEvent::Toggle => Control::Toggle,
        MediaControlEvent::Next => Control::Next,
        MediaControlEvent::Previous => Control::Previous,
        MediaControlEvent::Seek(direction) => Control::Seek { seconds: step(direction) * SEEK_STEP, relative: true },
        MediaControlEvent::SeekBy(direction, by) => Control::Seek { seconds: step(direction) * by.as_secs_f64(), relative: true },
        MediaControlEvent::SetPosition(MediaPosition(at)) => Control::Seek { seconds: at.as_secs_f64(), relative: false },
        MediaControlEvent::SetVolume(volume) => Control::Volume(volume.clamp(0.0, 1.0) as f32),
        MediaControlEvent::Raise => Control::ShowWindow,
        MediaControlEvent::Quit => Control::Quit,
        MediaControlEvent::OpenUri(_) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_presses_become_controls() {
        assert_eq!(control(MediaControlEvent::Stop), Some(Control::Pause));
        assert_eq!(control(MediaControlEvent::Seek(SeekDirection::Backward)), Some(Control::Seek { seconds: -SEEK_STEP, relative: true }));
        assert_eq!(control(MediaControlEvent::SetPosition(MediaPosition(Duration::from_secs(30)))), Some(Control::Seek { seconds: 30.0, relative: false }));
        assert_eq!(control(MediaControlEvent::SetVolume(1.5)), Some(Control::Volume(1.0)));
        assert_eq!(control(MediaControlEvent::OpenUri("x".into())), None);
    }
}
