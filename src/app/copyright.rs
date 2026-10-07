//! A copyright notice on one of the user's uploads (docs/SERVER.md): the
//! window its notification opens. It shows the notice (who sent it, for
//! whom, the work; not their contact details) and when it stands, and
//! offers Dispute... (a counter-notice, which the claimant is sent) or
//! letting it stand.

use std::sync::mpsc::{self, Receiver, Sender};

use eframe::egui;

use super::library::host;
use super::App;
use crate::library::{self, client, CounterNotice, NoticeView};

enum Event {
    Loaded(String, i64, Box<Result<NoticeView, String>>),
    /// Disputed or let stand: what was done (the notification's resolution).
    Done(String, i64, Result<&'static str, String>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Dispute,
    LetStand,
}

struct Open {
    server: String,
    id: i64,
    view: Option<Result<NoticeView, String>>,
    mode: Mode,
    counter: CounterNotice,
    busy: bool,
    error: Option<String>,
}

pub struct CopyrightState {
    events: (Sender<Event>, Receiver<Event>),
    open: Option<Open>,
}

impl Default for CopyrightState {
    fn default() -> Self {
        Self { events: mpsc::channel(), open: None }
    }
}

impl CopyrightState {
    pub fn open(&self) -> bool {
        self.open.is_some()
    }
}

impl App {
    fn spawn_copyright(&self, job: impl FnOnce() -> Event + Send + 'static) {
        let tx = self.copyright.events.0.clone();
        std::thread::spawn(move || {
            let _ = tx.send(job());
        });
    }

    /// Open notice `id` on `server` (from its notification).
    pub(super) fn open_notice(&mut self, server: String, id: i64, mode_dispute: bool) {
        let (Some(identity), Some(key)) = (self.library_identity(), self.config.library.pinned.get(&server).cloned()) else {
            self.status = "That server's key or your identity isn't known.".into();
            return;
        };
        self.copyright.open = Some(Open {
            server: server.clone(),
            id,
            view: None,
            mode: if mode_dispute { Mode::Dispute } else { Mode::LetStand },
            counter: CounterNotice::default(),
            busy: false,
            error: None,
        });
        self.spawn_copyright(move || Event::Loaded(server.clone(), id, Box::new(client::notice(&server, &key, &identity, id))));
    }

    pub(super) fn poll_copyright(&mut self) {
        while let Ok(event) = self.copyright.events.1.try_recv() {
            match event {
                Event::Loaded(server, id, view) => {
                    if let Some(open) = &mut self.copyright.open
                        && open.server == server
                        && open.id == id
                    {
                        open.view = Some(*view);
                    }
                }
                Event::Done(server, id, result) => match result {
                    Ok(resolution) => {
                        self.notification_resolved(&server, "notice", id, resolution);
                        self.copyright.open = None;
                        self.status = match resolution {
                            "disputed" => "Disputed: the moderators will decide, and the claimant has your counter-notice.".into(),
                            _ => "You let the takedown stand.".into(),
                        };
                    }
                    Err(e) => {
                        if let Some(open) = &mut self.copyright.open {
                            open.busy = false;
                            open.error = Some(e);
                        }
                    }
                },
            }
        }
    }

    pub(super) fn copyright_ui(&mut self, ctx: &egui::Context) {
        let Some(mut open) = self.copyright.open.take() else { return };
        let mut close = false;
        let mut send = false;
        let response = egui::Modal::new(egui::Id::new("copyright_notice")).show(ctx, |ui| {
            ui.set_width(600.0);
            let view = match &open.view {
                None => {
                    ui.spinner();
                    None
                }
                Some(Err(e)) => {
                    ui.colored_label(ui.visuals().warn_fg_color, format!("Couldn't get the notice: {e}"));
                    None
                }
                Some(Ok(view)) => Some(view.clone()),
            };
            if let Some(view) = &view {
                ui.heading(format!("Copyright notice on \"{}\"", view.item_name));
                ui.weak(format!("Notice #{} on {}, {}", view.id, host(&open.server), library::ago(view.created)));
                ui.add_space(6.0);
                let c = &view.notice;
                ui.label(format!("From {}, for {}.", c.name, c.on_behalf_of));
                ui.label(format!("The work: {}", c.work));
                if !c.work_location.is_empty() {
                    ui.weak(format!("Where it's from: {}", c.work_location));
                }
                ui.add_space(6.0);
                if view.status != "taken_down" {
                    ui.label(format!("It's {} now.", view.status.replace('_', " ")));
                } else {
                    match open.mode {
                        Mode::Dispute => counter_form_ui(ui, &mut open.counter),
                        Mode::LetStand => {
                            let left = crate::song_info::format_length((view.deadline - unix_now()).max(0) as f64);
                            ui.label(format!(
                                "If you don't dispute it, the takedown stands in {left}. Letting it stand now does \
                                 that at once: the script stays down, the same code can't be published here again, \
                                 and it counts against your identity (three takedowns that stand, and it's banned \
                                 from this server)."
                            ));
                            ui.horizontal(|ui| {
                                ui.selectable_value(&mut open.mode, Mode::Dispute, "I want to dispute it");
                                ui.selectable_value(&mut open.mode, Mode::LetStand, "Let it stand");
                            });
                        }
                    }
                }
            }
            if let Some(error) = &open.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let actionable = view.as_ref().is_some_and(|v| v.status == "taken_down");
                if actionable {
                    let (label, ready) = match open.mode {
                        Mode::Dispute => ("Send dispute", counter_ready(&open.counter)),
                        _ => ("Let it stand", true),
                    };
                    if ui.add_enabled(ready && !open.busy, egui::Button::new(label)).clicked() {
                        send = true;
                    }
                }
                if ui.button(if actionable { "Cancel" } else { "Close" }).clicked() {
                    close = true;
                }
                if open.busy {
                    ui.spinner();
                }
            });
        });
        if send {
            match (self.library_identity(), self.config.library.pinned.get(&open.server).cloned()) {
                (Some(identity), Some(key)) => {
                    open.busy = true;
                    open.error = None;
                    let (server, id, counter, dispute) = (open.server.clone(), open.id, open.counter.clone(), open.mode == Mode::Dispute);
                    self.spawn_copyright(move || {
                        let result = if dispute {
                            client::dispute_notice(&server, &key, &identity, id, &counter).map(|()| "disputed")
                        } else {
                            client::accept_notice(&server, &key, &identity, id).map(|()| "let stand")
                        };
                        Event::Done(server, id, result)
                    });
                }
                _ => open.error = Some("Your identity or the server's key isn't known.".into()),
            }
        }
        if !(close || (response.should_close() && !open.busy)) {
            self.copyright.open = Some(open);
        }
    }
}

fn counter_ready(counter: &CounterNotice) -> bool {
    [&counter.name, &counter.address, &counter.email, &counter.signature].iter().all(|f| !f.trim().is_empty())
        && counter.mistake
        && counter.consent
}

/// A dispute's form: what a DMCA counter-notice needs.
fn counter_form_ui(ui: &mut egui::Ui, counter: &mut CounterNotice) {
    ui.colored_label(
        ui.visuals().warn_fg_color,
        "A dispute is a legal counter-notice: the moderators decide, and the person who sent the notice is sent \
         your name, address and email (they may take it to court). Only dispute it if you have the right to share \
         this.",
    );
    ui.add_space(4.0);
    egui::Grid::new("counter_form").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        for (label, field) in [
            ("Your full name", &mut counter.name),
            ("Postal address", &mut counter.address),
            ("Email", &mut counter.email),
            ("Phone (optional)", &mut counter.phone),
        ] {
            ui.label(label);
            ui.add(egui::TextEdit::singleline(field).desired_width(380.0));
            ui.end_row();
        }
    });
    ui.label("Why it's yours to share (for the moderators):");
    ui.add(egui::TextEdit::multiline(&mut counter.explanation).desired_rows(3).desired_width(f32::INFINITY));
    ui.checkbox(
        &mut counter.mistake,
        "Under penalty of perjury, I have a good faith belief it was taken down by mistake or misidentification.",
    );
    ui.checkbox(
        &mut counter.consent,
        "I consent to the jurisdiction of the courts where I live (or, outside the US, any where the server's \
         provider may be found), and will accept service of process from the person who sent the notice.",
    );
    ui.horizontal(|ui| {
        ui.label("Signature (your full name, typed)");
        ui.add(egui::TextEdit::singleline(&mut counter.signature).desired_width(220.0));
    });
}

fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}
