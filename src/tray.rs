//! The notification-area (tray) icon: synththing's icon. Clicking it shows
//! and hides the mini player (`app/tray_panel.rs`); right-clicking it brings
//! the main window back. Windows only for now: on other systems there's
//! no tray, and closing the window quits as it always has.
//!
//! The icon's events arrive on the tray library's own callback, which
//! queues them and wakes the app (`request_repaint`), so a click is seen
//! even while the window is hidden and nothing else is happening.

/// What a click on the tray icon asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TrayClick {
    /// Left button: the mini player, by `cursor` the first time (physical
    /// screen pixels).
    Primary { cursor: [f32; 2] },
    /// Right button: the main window.
    Secondary,
}

#[cfg(windows)]
mod imp {
    use super::TrayClick;
    use std::sync::mpsc::{Receiver, channel};
    use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

    /// The 32 px icon build.rs draws from assets/synththing-icon.svg.
    const ICON_32: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/icon_32.rgba"));

    pub struct Tray {
        /// Kept alive: dropping it takes the icon away.
        _icon: TrayIcon,
        clicks: Receiver<TrayClick>,
    }

    impl Tray {
        /// Put the icon in the tray. Call on the main thread, once the
        /// window exists (its event loop runs the icon's messages).
        pub fn new(ctx: &eframe::egui::Context) -> Option<Self> {
            let icon = tray_icon::Icon::from_rgba(ICON_32.to_vec(), 32, 32)
                .map_err(|e| log::warn!("no tray icon: {e}"))
                .ok()?;
            let (sender, clicks) = channel();
            let ctx = ctx.clone();
            TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
                let click = match event {
                    TrayIconEvent::Click {
                        button: MouseButton::Left, button_state: MouseButtonState::Up, position, ..
                    } => TrayClick::Primary { cursor: [position.x as f32, position.y as f32] },
                    TrayIconEvent::Click { button: MouseButton::Right, button_state: MouseButtonState::Up, .. } => {
                        TrayClick::Secondary
                    }
                    _ => return,
                };
                let _ = sender.send(click);
                ctx.request_repaint();
            }));
            let icon = TrayIconBuilder::new()
                .with_icon(icon)
                .with_tooltip("synththing")
                .build()
                .map_err(|e| log::warn!("no tray icon: {e}"))
                .ok()?;
            Some(Self { _icon: icon, clicks })
        }

        /// The clicks since the last call, oldest first.
        pub fn clicks(&self) -> Vec<TrayClick> {
            self.clicks.try_iter().collect()
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::TrayClick;

    /// No tray here (see the module docs).
    pub struct Tray;

    impl Tray {
        pub fn new(_ctx: &eframe::egui::Context) -> Option<Self> {
            None
        }

        pub fn clicks(&self) -> Vec<TrayClick> {
            Vec::new()
        }
    }
}

pub use imp::Tray;
