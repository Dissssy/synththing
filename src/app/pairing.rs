//! Signing in another device, in Preferences > Online Library
//! (`library/pairing.rs`): Sign in another device... shows a QR code and
//! a link for the other device to open; Sign in with a link... takes one
//! here. The handoff goes through the authority (the official server).

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui;

use super::library::host;
use super::App;
use crate::device::Device;
use crate::library::identity::{self, Identity};
use crate::library::pairing::{self, Link, PairStatus, Parked};
use crate::library::client;

/// How often a code on screen asks whether it's been taken.
const CHECK_EVERY: Duration = Duration::from_secs(2);

enum Event {
    Parked(Result<(Link, Parked), String>),
    Status(Result<PairStatus, String>),
    SignedIn(Result<Identity, String>),
}

enum Flow {
    /// A code for another device: being made, then shown until it's taken
    /// or expires.
    Show {
        code: Option<(Link, Parked, qrcode::QrCode)>,
        taken: Option<String>,
        checked: Option<Instant>,
        checking: bool,
    },
    /// A link from another device, pasted in.
    Enter { link: String },
}

struct FlowState {
    flow: Flow,
    busy: bool,
    error: Option<String>,
}

pub struct PairingState {
    events: (Sender<Event>, Receiver<Event>),
    flow: Option<FlowState>,
    device: Device,
}

impl Default for PairingState {
    fn default() -> Self {
        Self { events: mpsc::channel(), flow: None, device: Device::load() }
    }
}

impl App {
    fn spawn_pairing(&self, job: impl FnOnce() -> Event + Send + 'static) {
        let tx = self.pairing.events.0.clone();
        std::thread::spawn(move || {
            let _ = tx.send(job());
        });
    }

    pub(super) fn poll_pairing(&mut self) {
        while let Ok(event) = self.pairing.events.1.try_recv() {
            let Some(state) = self.pairing.flow.as_mut() else { continue };
            match (event, &mut state.flow) {
                (Event::Parked(result), Flow::Show { code, .. }) => {
                    state.busy = false;
                    match result.and_then(|(link, parked)| {
                        let qr = qrcode::QrCode::new(link.url()).map_err(|e| e.to_string())?;
                        Ok((link, parked, qr))
                    }) {
                        Ok(made) => *code = Some(made),
                        Err(e) => state.error = Some(e),
                    }
                }
                (Event::Status(result), Flow::Show { taken, checking, .. }) => {
                    *checking = false;
                    match result {
                        Ok(status) => *taken = status.taken,
                        Err(e) => state.error = Some(e),
                    }
                }
                (Event::SignedIn(result), Flow::Enter { .. }) => match result {
                    Ok(identity) => {
                        let id = identity.id();
                        log::info!("signed in as #{id} with a link");
                        self.pairing.flow = None;
                        self.set_library_identity(identity);
                        self.forget_identity_status();
                        self.library_identity_message(&format!("Signed in as #{id}."));
                    }
                    Err(e) => {
                        state.busy = false;
                        state.error = Some(e);
                    }
                },
                _ => {}
            }
        }
    }

    /// Preferences > Online Library, under the identity: this device's
    /// name, and signing in another device or this one.
    pub(super) fn pairing_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("This device");
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.pairing.device.name)
                    .char_limit(pairing::MAX_DEVICE_NAME)
                    .desired_width(160.0),
            );
            if response.lost_focus()
                && let Err(e) = self.pairing.device.save()
            {
                self.library_identity_message(&format!("Couldn't save this device's name: {e:#}"));
            }
            super::info_icon(ui, "What another device shows when this one signs in with its code.");
        });
        let mut open = None;
        ui.horizontal(|ui| {
            if self.library_identity().is_some()
                && ui
                    .button("Sign in another device...")
                    .on_hover_text("Show a code your phone or another computer opens to sign in as you")
                    .clicked()
            {
                open = Some(Flow::Show { code: None, taken: None, checked: None, checking: false });
            }
            if ui
                .button("Sign in with a link...")
                .on_hover_text("Sign in here with the link another device of yours shows")
                .clicked()
            {
                open = Some(Flow::Enter { link: String::new() });
            }
        });
        if let Some(flow) = open {
            let show = matches!(flow, Flow::Show { .. });
            self.pairing.flow = Some(FlowState { flow, busy: false, error: None });
            if show {
                self.make_pairing_code();
            }
        }
    }

    /// The app was opened with a sign-in link: ask to sign in with it.
    #[cfg_attr(not(target_os = "android"), expect(dead_code))]
    pub(super) fn open_pairing_link(&mut self, link: String) {
        self.pairing.flow = Some(FlowState { flow: Flow::Enter { link }, busy: false, error: None });
    }

    /// Seal the identity and park it on the authority, for `Flow::Show`.
    fn make_pairing_code(&mut self) {
        let (authority, key) = self.authority();
        let identity = self.library_identity();
        let Some(state) = self.pairing.flow.as_mut() else { return };
        let (Some(identity), Some(key)) = (identity, key) else {
            state.error = Some(format!("{} hasn't been reached yet: open the Online Library first.", host(&authority)));
            return;
        };
        state.busy = true;
        state.error = None;
        self.spawn_pairing(move || {
            Event::Parked((|| {
                let secret = Link::secret()?;
                let sealed = pairing::seal(&secret, identity.export("")?.as_bytes())?;
                let parked = client::pair_park(&authority, &key, &identity, &sealed)?;
                Ok((Link { server: authority, id: parked.id.clone(), secret }, parked))
            })())
        });
    }

    /// The window for showing a code or taking a link.
    pub(super) fn pairing_flow_ui(&mut self, ctx: &egui::Context) {
        let Some(mut state) = self.pairing.flow.take() else { return };
        let current = self.library_identity();
        let mut close = false;
        let mut go = false;
        let mut again = false;
        let mut check = None;
        egui::Modal::new(egui::Id::new("pairing_flow")).show(ctx, |ui| {
            ui.set_width(super::fit_width(ui, 360.0));
            match &mut state.flow {
                Flow::Show { code, taken, checked, checking } => {
                    ui.heading("Sign in another device");
                    match (code, taken) {
                        (_, Some(device)) => {
                            ui.label(format!("Signed in on {device}."));
                            ui.weak(
                                "Don't recognise it? New key... moves your identity to a new key, which signs \
                                 every other device out.",
                            );
                        }
                        (Some((link, parked, qr)), None) => {
                            let left = parked.expires - unix_now();
                            if left <= 0 {
                                ui.label("The code expired.");
                                again = ui.button("Show a new one").clicked();
                            } else {
                                ui.label("Scan it with your phone's camera, or copy the link to the other device.");
                                ui.vertical_centered(|ui| qr_ui(ui, qr, 220.0));
                                ui.horizontal(|ui| {
                                    if ui.button("Copy link").clicked() {
                                        ui.ctx().copy_text(link.url());
                                    }
                                    ui.weak(format!("Works once, for {}:{:02}.", left / 60, left % 60));
                                });
                                ui.weak("Anyone with this code can sign in as you: don't share it.");
                                if !*checking && checked.is_none_or(|c| c.elapsed() >= CHECK_EVERY) {
                                    *checking = true;
                                    *checked = Some(Instant::now());
                                    check = Some(link.clone());
                                }
                                ui.ctx().request_repaint_after(Duration::from_millis(500));
                            }
                        }
                        (None, None) => {
                            if state.busy {
                                ui.spinner();
                            }
                        }
                    }
                }
                Flow::Enter { link } => {
                    ui.heading("Sign in with a link");
                    ui.label("On a device that's signed in, choose Sign in another device... and paste its link here.");
                    ui.add(
                        egui::TextEdit::singleline(link)
                            .hint_text("https://.../pair#...")
                            .desired_width(f32::INFINITY),
                    );
                    if let Some(current) = &current {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            format!(
                                "This replaces #{} here. Export it first if you might want it back.",
                                current.id()
                            ),
                        );
                    }
                }
            }
            if let Some(error) = &state.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if let Flow::Enter { link } = &state.flow
                    && ui
                        .add_enabled(!state.busy && !link.trim().is_empty(), egui::Button::new("Sign in"))
                        .clicked()
                {
                    go = true;
                }
                let done = matches!(&state.flow, Flow::Show { taken: Some(_), .. });
                if ui.button(if done { "Close" } else { "Cancel" }).clicked() {
                    close = true;
                }
                if state.busy {
                    ui.spinner();
                }
            });
        });
        if let (Some(link), Some(identity), (_, Some(key))) = (check, current.clone(), self.authority()) {
            self.spawn_pairing(move || Event::Status(client::pair_status(&link.server, &key, &identity, &link.id)));
        }
        if go && let Flow::Enter { link } = &state.flow {
            state.error = None;
            match Link::parse(link) {
                Some(link) => {
                    state.busy = true;
                    let device = self.pairing.device.name.clone();
                    self.spawn_pairing(move || {
                        Event::SignedIn((|| {
                            let sealed = client::pair_take(&link.server, &link.id, &device)?;
                            let text = String::from_utf8(pairing::open(&link.secret, &sealed)?)
                                .map_err(|_| "the handoff is damaged")?;
                            identity::adopt(&text, "")
                        })())
                    });
                }
                None => state.error = Some("That isn't a sign-in link.".into()),
            }
        }
        if !close {
            self.pairing.flow = Some(state);
        }
        if again {
            if let Some(FlowState { flow: Flow::Show { code, .. }, .. }) = self.pairing.flow.as_mut() {
                *code = None;
            }
            self.make_pairing_code();
        }
    }
}

/// A QR code, `size` points square, dark on light whatever the theme (so
/// cameras read it), with the quiet border around it scanners want.
fn qr_ui(ui: &mut egui::Ui, qr: &qrcode::QrCode, size: f32) {
    let modules = qr.width();
    let cell = size / (modules + 8) as f32;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, egui::Color32::WHITE);
    for (i, color) in qr.to_colors().into_iter().enumerate() {
        if color == qrcode::Color::Dark {
            let (x, y) = ((i % modules + 4) as f32, (i / modules + 4) as f32);
            let min = rect.min + egui::vec2(x * cell, y * cell);
            painter.rect_filled(egui::Rect::from_min_size(min, egui::vec2(cell, cell)), 0.0, egui::Color32::BLACK);
        }
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}
