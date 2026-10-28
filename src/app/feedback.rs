//! Send feedback (Help > Send feedback..., and the speech bubble in the
//! menu bar): a note to the people making synththing, through the official
//! library server (docs/SERVER.md). One box to write in; the build and OS
//! go along, and the log and the last minute's frame times only if they
//! tick them. Branch builds send it too:
//! their feedback is about what's being tried in them.

use std::sync::mpsc::{self, Receiver};

use eframe::egui;

use super::App;
use crate::build_info::{self, Channel};
use crate::library::{self, client, FeedbackRequest, MAX_FEEDBACK_LOG, MAX_FEEDBACK_TEXT};

/// Lines of the log sent along, at most.
const LOG_LINES: usize = 300;

pub struct FeedbackState {
    open: bool,
    /// Just opened: the box takes the keyboard, ready to type in.
    focus: bool,
    text: String,
    include_log: bool,
    include_performance: bool,
    error: Option<String>,
    /// Being sent: how it went, when it has.
    sending: Option<Receiver<Result<i64, String>>>,
}

impl Default for FeedbackState {
    /// The frame times go along unless they untick it: nothing in them is
    /// about the person.
    fn default() -> Self {
        Self { open: false, focus: false, text: String::new(), include_log: false, include_performance: true, error: None, sending: None }
    }
}

impl FeedbackState {
    pub fn open(&self) -> bool {
        self.open
    }
}

/// The end of the log, as lines, within `MAX_FEEDBACK_LOG`.
fn recent_log() -> String {
    let entries = crate::applog::entries();
    let mut log = String::new();
    for entry in entries.iter().rev().take(LOG_LINES) {
        let line = entry.line();
        if log.len() + line.len() + 1 > MAX_FEEDBACK_LOG {
            break;
        }
        log.insert_str(0, &format!("{line}\n"));
    }
    log
}

fn request(text: &str, include_log: bool, include_performance: bool) -> FeedbackRequest {
    let channel = match build_info::channel() {
        Channel::Stable => "stable".to_string(),
        Channel::Nightly => "nightly".to_string(),
        Channel::Branch(name) => format!("branch-{name}"),
    };
    FeedbackRequest {
        text: text.trim().to_string(),
        app_version: build_info::describe(),
        channel,
        os: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        log: include_log.then(recent_log),
        performance: include_performance.then(crate::frame_timing::summary).flatten(),
    }
}

impl App {
    pub(super) fn open_feedback(&mut self) {
        self.feedback.open = true;
        self.feedback.focus = true;
        self.feedback.error = None;
    }

    /// The speech bubble in the menu bar.
    pub(super) fn feedback_button_ui(&mut self, ui: &mut egui::Ui) {
        if ui
            .add(super::title_bar::bar_icon("chat-circle-dots"))
            .on_hover_text("Send feedback")
            .clicked()
        {
            self.open_feedback();
        }
    }

    pub(super) fn poll_feedback(&mut self) {
        let Some(result) = self.feedback.sending.as_ref().and_then(|rx| rx.try_recv().ok()) else { return };
        self.feedback.sending = None;
        match result {
            Ok(number) => {
                log::info!("sent feedback #{number}");
                self.feedback = FeedbackState::default();
                self.status = "Feedback sent. Thank you!".into();
            }
            Err(e) => {
                self.status = format!("Couldn't send the feedback: {e}");
                self.feedback.error = Some(format!("Couldn't send it: {e}"));
            }
        }
    }

    pub(super) fn feedback_ui(&mut self, ctx: &egui::Context) {
        if !self.feedback.open {
            return;
        }
        let state = &mut self.feedback;
        let sending = state.sending.is_some();
        let mut send = false;
        let mut close = false;
        let response = egui::Modal::new(egui::Id::new("feedback")).show(ctx, |ui| {
            ui.set_width(super::fit_width(ui, 520.0));
            ui.heading("Send feedback");
            if let Channel::Branch(name) = build_info::channel() {
                ui.label(format!("This is the test build of \"{name}\": say how what it's trying works for you."));
            }
            ui.label("What's working, what isn't, what you wish it did:");
            let text = ui.add(
                egui::TextEdit::multiline(&mut state.text)
                    .desired_rows(6)
                    .desired_width(f32::INFINITY)
                    .char_limit(MAX_FEEDBACK_TEXT),
            );
            if std::mem::take(&mut state.focus) {
                text.request_focus();
            }
            ui.checkbox(&mut state.include_log, "Include the app's log (helps with bugs)")
                .on_hover_text(format!("The last {LOG_LINES} lines of Help > Log"));
            ui.checkbox(&mut state.include_performance, "Include how smoothly the app ran (helps with stutters)")
                .on_hover_text("How long the app took to draw over the last minute, on average and at worst, and in which parts");
            ui.weak(format!(
                "Goes to synththing's developers with the app's version ({}) and your OS. No account, and nothing else about you.",
                build_info::describe()
            ));
            if let Some(error) = &state.error {
                ui.colored_label(ui.visuals().warn_fg_color, error);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!sending && !state.text.trim().is_empty(), egui::Button::new("Send")).clicked() {
                    send = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
                if sending {
                    ui.spinner();
                }
            });
        });
        if send {
            let feedback = request(&state.text, state.include_log, state.include_performance);
            let (tx, rx) = mpsc::channel();
            state.sending = Some(rx);
            state.error = None;
            std::thread::spawn(move || {
                let _ = tx.send(client::feedback(&library::official_url(), &feedback));
            });
        }
        // (What's written stays for next time, unless it was sent.)
        if close || (response.should_close() && !sending) {
            state.open = false;
        }
    }
}
