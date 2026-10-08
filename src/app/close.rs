//! Closing the window: quitting, or (where there's a tray) hiding to it and
//! carrying on, music and all. The first close asks which, once; after
//! that it's Preferences > General. Either way, a recording running asks
//! first what to do with it: recording needs the window, so it never
//! carries on hidden.

use eframe::egui;

use super::App;

/// What closing does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CloseAction {
    Quit,
    /// Hide the window, still running in the tray.
    Hide,
}

impl CloseAction {
    /// "quit" / "go to the tray", for the prompts' buttons.
    fn verb(self) -> &'static str {
        match self {
            CloseAction::Quit => "quit",
            CloseAction::Hide => "go to the tray",
        }
    }
}

/// A question closing asks before it goes ahead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClosePrompt {
    /// The first close: quit, or keep running in the tray?
    FirstTime,
    /// A recording (or a render) is running; then `CloseAction`.
    Recording(CloseAction),
}

impl App {
    /// The window's close button (or Alt+F4) was pressed: do what closing
    /// does, asking first when it's the first time or a recording's on.
    pub(super) fn close_requested_ui(&mut self, ctx: &egui::Context) {
        if self.quitting || !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        let action = match (self.tray.is_some(), self.config.close_to_tray) {
            (false, _) | (true, Some(false)) => CloseAction::Quit,
            (true, Some(true)) => CloseAction::Hide,
            (true, None) => {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.close_prompt = Some(ClosePrompt::FirstTime);
                return;
            }
        };
        if action == CloseAction::Hide || self.recording_running() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        self.close_with(ctx, action);
    }

    /// Quit or hide, unless a recording's running: then ask first.
    pub(super) fn close_with(&mut self, ctx: &egui::Context, action: CloseAction) {
        if self.recording_running() {
            self.show_window(ctx);
            self.close_prompt = Some(ClosePrompt::Recording(action));
            return;
        }
        match action {
            CloseAction::Quit => self.quit(ctx),
            CloseAction::Hide => self.hide_window(ctx),
        }
    }

    /// Quit now (the app saves what it keeps on the way out, `on_exit`).
    pub(super) fn quit(&mut self, ctx: &egui::Context) {
        self.quitting = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Hide the window; the tray icon brings it back.
    pub(super) fn hide_window(&mut self, ctx: &egui::Context) {
        self.hidden = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    /// A recording or a background render is going.
    fn recording_running(&self) -> bool {
        self.recording.recorder.is_some() || self.recording.render.is_some()
    }

    /// Per frame: a close waiting for a render to finish goes ahead once it
    /// has.
    pub(super) fn close_after_render_tick(&mut self, ctx: &egui::Context) {
        if self.recording.render.is_none()
            && let Some(action) = self.close_after_render.take()
        {
            self.close_with(ctx, action);
        }
    }

    /// The question closing asked, if any.
    pub(super) fn close_prompt_ui(&mut self, ctx: &egui::Context) {
        let Some(prompt) = self.close_prompt else { return };
        let mut answer: Option<Answer> = None;
        let response = egui::Modal::new(egui::Id::new("close_prompt")).show(ctx, |ui| {
            ui.set_width(460.0);
            match prompt {
                ClosePrompt::FirstTime => first_time_ui(ui, &mut answer),
                ClosePrompt::Recording(action) => {
                    if self.recording.recorder.is_some() {
                        live_recording_ui(ui, action, &mut answer);
                    } else {
                        render_ui(ui, action, &mut answer);
                    }
                }
            }
        });
        if response.should_close() && answer.is_none() {
            answer = Some(Answer::Stay);
        }
        let Some(answer) = answer else { return };
        self.close_prompt = None;
        match answer {
            Answer::Stay => {}
            Answer::Remember(action) => {
                self.config.close_to_tray = Some(action == CloseAction::Hide);
                if let Err(e) = self.config.save() {
                    self.status = format!("Couldn't save preferences: {e}");
                }
                self.close_with(ctx, action);
            }
            Answer::SaveRecording(action) => {
                self.stop_recording();
                self.close_with(ctx, action);
            }
            Answer::DiscardRecording(action) => {
                self.recording.discard();
                self.status = "Recording discarded.".to_string();
                self.close_with(ctx, action);
            }
            Answer::StopRender(action) => {
                if let Some(render) = self.recording.render.take() {
                    render.stop_and_wait();
                }
                self.close_with(ctx, action);
            }
            Answer::WhenRendered(action) => {
                self.close_after_render = Some(action);
                self.status = format!("synththing will {} when the render's done.", action.verb());
            }
        }
    }
}

/// What the person picked in a close prompt.
enum Answer {
    /// Don't close.
    Stay,
    /// The first close's choice, kept for every close after.
    Remember(CloseAction),
    SaveRecording(CloseAction),
    DiscardRecording(CloseAction),
    /// Stop the render here (what's rendered stays) and close.
    StopRender(CloseAction),
    /// Close once the render's finished.
    WhenRendered(CloseAction),
}

fn first_time_ui(ui: &mut egui::Ui, answer: &mut Option<Answer>) {
    ui.heading("Keep synththing running in the tray?");
    ui.add_space(4.0);
    ui.label(
        "Closing the window can quit synththing, or hide it in the tray (the icons by the clock) with the \
         music still playing.",
    );
    ui.add_space(4.0);
    ui.label(format!("{}  Click its icon to bring synththing back.", super::icon("cursor-click")));
    ui.label(format!(
        "{}  Right-click it for a small panel: play, pause and skip, the volume, soundfonts and playlists, \
         without opening the window.",
        super::icon("list")
    ));
    ui.add_space(4.0);
    ui.weak("Quit (in the synththing menu, or the tray panel) always closes it completely. You can change this any time in Preferences > General.");
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button("Keep running in the tray").clicked() {
            *answer = Some(Answer::Remember(CloseAction::Hide));
        }
        if ui.button("Quit").clicked() {
            *answer = Some(Answer::Remember(CloseAction::Quit));
        }
        if ui.button("Cancel").on_hover_text("Leave the window open").clicked() {
            *answer = Some(Answer::Stay);
        }
    });
}

fn live_recording_ui(ui: &mut egui::Ui, action: CloseAction, answer: &mut Option<Answer>) {
    ui.heading("A recording is running");
    ui.add_space(4.0);
    ui.label("Recording needs the window open, so it can't carry on once synththing's closed.");
    ui.add_space(8.0);
    ui.horizontal_wrapped(|ui| {
        if ui.button(format!("Save it, then {}", action.verb())).clicked() {
            *answer = Some(Answer::SaveRecording(action));
        }
        if ui.button(format!("Discard it and {}", action.verb())).clicked() {
            *answer = Some(Answer::DiscardRecording(action));
        }
        if ui.button("Keep recording").clicked() {
            *answer = Some(Answer::Stay);
        }
    });
}

fn render_ui(ui: &mut egui::Ui, action: CloseAction, answer: &mut Option<Answer>) {
    ui.heading("A render is running");
    ui.add_space(4.0);
    ui.label("synththing is still rendering a video.");
    ui.add_space(8.0);
    ui.horizontal_wrapped(|ui| {
        if ui
            .button(format!("{} when it's done", capitalized(action.verb())))
            .on_hover_text("The window stays until the render finishes")
            .clicked()
        {
            *answer = Some(Answer::WhenRendered(action));
        }
        if ui
            .button(format!("Stop it here and {}", action.verb()))
            .on_hover_text("What's rendered so far is kept as the video")
            .clicked()
        {
            *answer = Some(Answer::StopRender(action));
        }
        if ui.button("Keep it open").clicked() {
            *answer = Some(Answer::Stay);
        }
    });
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}
