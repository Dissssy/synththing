//! Updates: checking on launch, the Updates window, What's new, and branch
//! builds.

use std::time::Duration;

use eframe::egui;

use crate::updater::{self, Track, UpdateState};
use crate::changelog;

use super::{App, close, confirm_button, flow, icon, inline_code_job, setting_row, settings_group};

impl App {
    /// What the updater follows: this branch build's branch, or canary or
    /// releases as chosen in Preferences.
    pub(super) fn update_track(&self) -> Track {
        Track::current(self.config.canary_updates())
    }

    /// Look for a new release in the background, if that's enabled. Not in
    /// development builds (`cargo run`), which shouldn't offer to replace
    /// themselves with a release.
    pub fn check_for_updates_on_launch(&mut self) {
        if cfg!(debug_assertions) || !self.config.check_updates_on_launch.unwrap_or(true) {
            return;
        }
        self.launch_update_check = true;
        self.updater.check(self.update_track());
    }

    /// After an update, show what's new since the version that ran last,
    /// and remember this one. A fresh install shows nothing; a config from
    /// before versions were recorded (`ran_before`, no `last_version`)
    /// gets this version's changes.
    pub fn whats_new_on_launch(&mut self, ran_before: bool) {
        let Some(current) = changelog::current_version() else { return };
        let last = match self.config.last_version.as_deref() {
            Some(text) => changelog::parse_version(text),
            None if ran_before => changelog::previous_version(current),
            None => None,
        };
        if let Some(last) = last
            && last < current
            && changelog::newer_than(last).next().is_some()
        {
            self.flow.add(flow::Step::Changelog(Some(last)));
        }
        let version = changelog::format_version(current);
        if self.config.last_version.as_deref() != Some(version.as_str()) {
            self.config.last_version = Some(version);
            if let Err(e) = self.config.save() {
                log::warn!("couldn't save the config: {e:#}");
            }
        }
    }

    /// The startup update check: what it found goes in the flow, if it's
    /// new (not a skipped version), or the branch build is gone.
    pub(super) fn poll_launch_update_check(&mut self, ctx: &egui::Context) {
        let state = self.updater.state();
        if self.launch_update_check {
            match &state {
                UpdateState::Checking => {}
                UpdateState::Available(release) => {
                    self.launch_update_check = false;
                    let skipped = self.config.skipped_update.as_deref() == Some(release.version.to_string().as_str());
                    if !skipped {
                        self.flow.add(flow::Step::Update);
                    }
                }
                UpdateState::Gone => {
                    self.launch_update_check = false;
                    self.flow.add(flow::Step::Update);
                }
                _ => self.launch_update_check = false,
            }
        }
        if matches!(state, UpdateState::Checking | UpdateState::Downloading { .. }) || self.branches.busy() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    /// An update is offered (the flow's Updates step, or the phone's
    /// Updates page, with one available) or downloading: sync and the online lookup wait, so the
    /// download has the connection. Closing it, or installing it (the app
    /// restarts without it open), lets them go on.
    pub(super) fn update_offered(&self) -> bool {
        match self.updater.state() {
            UpdateState::Downloading { .. } => true,
            UpdateState::Available(_) => self.flow.has(flow::Step::Update) || (self.mobile() && self.on_updates_page()),
            _ => false,
        }
    }

    /// What the update check found, release notes, and installing. On a
    /// page there's no Close (back leaves), and the release notes aren't
    /// a scroll area of their own; whether a way out (Close, Not now,
    /// Skip, Later) was clicked.
    pub(super) fn updates_body_ui(&mut self, ui: &mut egui::Ui, page: bool) -> bool {
        let state = self.updater.state();
        let current = updater::current_version();
        let mut close = false;
        let branch = match crate::build_info::channel() {
            crate::build_info::Channel::Branch(name) => Some(name),
            _ => None,
        };
        if let Some(name) = branch {
            ui.label(format!(
                "This is a test build of the {name} branch. It updates from that branch and keeps settings of \
                 its own, so your usual synththing isn't affected."
            ));
            ui.add_space(4.0);
        } else if self.config.canary_updates() {
            ui.weak("Canary updates: the newest build from master (Preferences > App).");
            ui.add_space(4.0);
        }
        match &state {
            UpdateState::Idle => {
                if ui.button("Check for updates").clicked() {
                    self.updater.check(self.update_track());
                }
            }
            UpdateState::Checking => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Checking for updates...");
                });
            }
            UpdateState::UpToDate => {
                ui.label(format!("You're on the latest version (v{current})."));
            }
            UpdateState::Available(release) => {
                ui.heading(format!("synththing v{} is available", release.version));
                ui.weak(format!("You have v{current}."));
                if !release.notes.trim().is_empty() || release.changes.is_some() {
                    ui.add_space(4.0);
                    if page {
                        release_notes_ui(ui, release);
                    } else {
                        egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| release_notes_ui(ui, release));
                    }
                }
                ui.hyperlink_to("Release page", &release.page_url);
                ui.add_space(6.0);
                let dev_build = cfg!(debug_assertions);
                if let Some(announce) = self.android_update_ui(ui) {
                    if announce {
                        // Shown this once: next time, only if there's a newer one.
                        self.config.skipped_update = Some(release.version.to_string());
                        if let Err(e) = self.config.save() {
                            self.status = format!("Couldn't save preferences: {e}");
                        }
                        close = true;
                    }
                } else {
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(!dev_build, egui::Button::new("Install"))
                            .on_disabled_hover_text(
                                "This is a development build (cargo run); updates install into release builds.",
                            )
                            .clicked()
                        {
                            self.updater.install(release.clone());
                        }
                        if ui.button("Skip this version").clicked() {
                            self.config.skipped_update = Some(release.version.to_string());
                            if let Err(e) = self.config.save() {
                                self.status = format!("Couldn't save preferences: {e}");
                            }
                            close = true;
                        }
                        if ui.button("Not now").clicked() {
                            close = true;
                        }
                    });
                }
            }
            UpdateState::Downloading { release, downloaded } => {
                ui.label(format!("Downloading v{}...", release.version));
                let total = release.size().max(1);
                ui.add(
                    egui::ProgressBar::new(*downloaded as f32 / total as f32)
                        .text(format!("{:.1} / {:.1} MB", *downloaded as f64 / 1e6, total as f64 / 1e6)),
                );
            }
            UpdateState::Installed(version) if cfg!(target_os = "android") => {
                ui.label(format!("Android asks you to install v{version}; synththing restarts with it."));
                if ui.button("OK").clicked() {
                    close = true;
                }
            }
            UpdateState::Installed(version) => {
                ui.label(format!("v{version} is installed. It runs the next time synththing starts."));
                ui.horizontal(|ui| {
                    if ui.button("Restart now").clicked() {
                        match updater::relaunch() {
                            // (Quit, not close: closing may only hide it in the tray.)
                            Ok(()) => self.close_with(&ui.ctx().clone(), close::CloseAction::Quit),
                            Err(e) => self.updates_message = Some((format!("Couldn't restart: {e}"), true)),
                        }
                    }
                    if ui.button("Later").clicked() {
                        close = true;
                    }
                });
            }
            UpdateState::Gone if cfg!(target_os = "android") => {
                let name = branch.unwrap_or_default();
                ui.label(format!(
                    "The {name} branch's build is gone: the branch was merged or deleted, so this app won't get \
                     updates any more. Uninstall it when you're done with it."
                ));
            }
            UpdateState::Gone => {
                let name = branch.unwrap_or_default();
                ui.label(format!(
                    "The {name} branch's build is gone: the branch was merged or deleted, so this build won't \
                     get updates any more. Go back to your usual synththing, and remove this one there, under \
                     Help > Check for updates > Branch builds."
                ));
                if let Some(stable) = crate::channels::installed_by()
                    && ui.button("Open the usual synththing").clicked()
                {
                    match std::process::Command::new(&stable).spawn() {
                        Ok(_) => self.close_with(&ui.ctx().clone(), close::CloseAction::Quit),
                        Err(e) => {
                            self.updates_message = Some((format!("Couldn't start {}: {e}", stable.display()), true));
                        }
                    }
                }
            }
            UpdateState::Failed(error) => {
                ui.colored_label(egui::Color32::from_rgb(220, 90, 90), error);
                if ui.button("Try again").clicked() {
                    self.updater.check(self.update_track());
                }
            }
        }
        if branch.is_none() {
            ui.add_space(8.0);
            self.branch_builds_ui(ui);
        } else if !cfg!(target_os = "android") {
            ui.add_space(8.0);
            self.recopy_settings_ui(ui);
        }
        if let Some((message, error)) = &self.updates_message {
            ui.add_space(6.0);
            if *error {
                ui.colored_label(egui::Color32::from_rgb(220, 90, 90), message);
            } else {
                ui.label(message);
            }
        }
        // Every state but Available has its own way out above, so a
        // plain Close for the rest. (A download carries on in the
        // background if this closes, and Help > Check for updates
        // shows how it went.)
        if !page && !matches!(state, UpdateState::Available(_) | UpdateState::Installed(_)) {
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        }
        close
    }

    /// In a branch build's Updates window: start its settings over from a
    /// copy of the usual synththing's, for a tester who's changed something
    /// there since. Takes a restart.
    pub(super) fn recopy_settings_ui(&mut self, ui: &mut egui::Ui) {
        if !self.confirm_recopy {
            if ui
                .button("Copy settings from your usual synththing again...")
                .on_hover_text("This build's settings started as a copy of them; take a fresh copy")
                .clicked()
            {
                self.confirm_recopy = true;
            }
            return;
        }
        ui.label(
            "Copy your usual synththing's settings over this build's, and restart it? Scripts and other files only \
             this build has are kept.",
        );
        ui.horizontal(|ui| {
            if ui.button("Copy and restart").clicked() {
                self.confirm_recopy = false;
                match crate::channels::recopy_settings_on_restart().and_then(|()| updater::relaunch()) {
                    Ok(()) => self.close_with(&ui.ctx().clone(), close::CloseAction::Quit),
                    Err(e) => self.updates_message = Some((format!("Couldn't copy the settings: {e:#}"), true)),
                }
            }
            if ui.button("Cancel").clicked() {
                self.confirm_recopy = false;
            }
        });
    }

    /// The Updates window's Branch builds section: trying a feature before
    /// it's merged. Each installs into a folder of its own with a copy of
    /// the settings, or on Android as an app of its own (`channels.rs`), so
    /// this install isn't touched.
    pub(super) fn branch_builds_ui(&mut self, ui: &mut egui::Ui) {
        let response = egui::CollapsingHeader::new("Branch builds").id_salt("branch_builds").show(ui, |ui| {
            if cfg!(target_os = "android") {
                ui.label(
                    "Try a feature before it's merged. Each branch build installs as an app of its own, with \
                     settings of its own, and its online features are off. This synththing isn't touched.",
                );
            } else {
                ui.label(
                    "Try a feature before it's merged. Each branch build installs separately, starting with a \
                     copy of your settings, and its online features are off. This synththing isn't touched.",
                );
            }
            // Android has to let this app install others first.
            #[cfg(target_os = "android")]
            let can_install = crate::android::can_install_packages();
            #[cfg(not(target_os = "android"))]
            let can_install = true;
            #[cfg(target_os = "android")]
            if !can_install {
                ui.label("Android doesn't allow synththing to install apps yet.");
                if ui.button("Allow it...").on_hover_text("Settings > Install unknown apps, for synththing").clicked() {
                    crate::android::open_install_settings();
                }
            }
            ui.add_space(4.0);
            let installed = crate::channels::installed();
            let list = self.branches.list();
            let install = self.branches.install_state();
            let installing = matches!(install, crate::channels::InstallState::Installing { .. });
            let builds: &[updater::Release] = match &list {
                crate::channels::ListState::Loaded(builds) => builds,
                _ => &[],
            };
            match &list {
                crate::channels::ListState::Idle => {}
                crate::channels::ListState::Loading => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Looking for branch builds...");
                    });
                }
                crate::channels::ListState::Failed(error) => {
                    ui.colored_label(egui::Color32::from_rgb(220, 90, 90), error);
                }
                crate::channels::ListState::Loaded(builds) if builds.is_empty() && installed.is_empty() => {
                    ui.weak("There are no branch builds right now.");
                }
                crate::channels::ListState::Loaded(_) => {}
            }
            // Every branch with a build, then installed ones whose build is
            // gone.
            let mut rows: Vec<(String, Option<&updater::Release>)> =
                builds.iter().map(|b| (b.tag.clone(), Some(b))).collect();
            for channel in &installed {
                if !builds.iter().any(|b| &b.tag == channel) {
                    rows.push((channel.clone(), None));
                }
            }
            // A grid, or on a narrow screen (a phone's) each branch's name
            // over its version and buttons.
            let narrow = ui.available_width() < 480.0;
            if narrow {
                for (channel, build) in rows {
                    ui.strong(crate::channels::branch_name(&channel));
                    ui.horizontal_wrapped(|ui| self.branch_build_row_ui(ui, &channel, build, &installed, installing, can_install));
                    ui.add_space(4.0);
                }
            } else {
                egui::Grid::new("branch_builds_grid").num_columns(3).spacing([12.0, 4.0]).show(ui, |ui| {
                    for (channel, build) in rows {
                        ui.strong(crate::channels::branch_name(&channel));
                        self.branch_build_row_ui(ui, &channel, build, &installed, installing, can_install);
                        ui.end_row();
                    }
                });
            }
            match &install {
                crate::channels::InstallState::Idle => {}
                crate::channels::InstallState::Installing { channel, downloaded, size } => {
                    ui.label(format!("Installing the {} build...", crate::channels::branch_name(channel)));
                    ui.add(
                        egui::ProgressBar::new(*downloaded as f32 / (*size).max(1) as f32)
                            .text(format!("{:.1} / {:.1} MB", *downloaded as f64 / 1e6, *size as f64 / 1e6)),
                    );
                }
                crate::channels::InstallState::Done(channel) if cfg!(target_os = "android") => {
                    ui.label(format!(
                        "Android asks you to install the {} build. Once it's in, open it here or from your apps.",
                        crate::channels::branch_name(channel)
                    ));
                }
                crate::channels::InstallState::Done(channel) => {
                    ui.label(format!("The {} build is installed and starting.", crate::channels::branch_name(channel)));
                }
                crate::channels::InstallState::Failed(error) => {
                    ui.colored_label(egui::Color32::from_rgb(220, 90, 90), error);
                }
            }
            if ui.add_enabled(!self.branches.busy(), egui::Button::new("Refresh")).clicked() {
                self.branches.refresh();
            }
        });
        // The list is fetched the first time the section opens.
        if response.body_returned.is_some() && matches!(self.branches.list(), crate::channels::ListState::Idle) {
            self.branches.refresh();
        }
    }

    /// A branch build's version and its buttons (Install, or Open and
    /// Remove), side by side.
    pub(super) fn branch_build_row_ui(
        &mut self,
        ui: &mut egui::Ui,
        channel: &str,
        build: Option<&updater::Release>,
        installed: &[String],
        installing: bool,
        can_install: bool,
    ) {
        match build {
            Some(b) => {
                let commit = b.commit.as_deref().map(|c| &c[..c.len().min(7)]).unwrap_or_default();
                ui.weak(format!("{} {commit}", b.version)).on_hover_text(b.notes.trim());
            }
            None => {
                ui.weak("its build is gone (merged?)");
            }
        }
        ui.horizontal(|ui| {
            if installed.iter().any(|c| c == channel) {
                if ui.add_enabled(!installing, egui::Button::new("Open")).clicked()
                    && let Err(e) = crate::channels::launch(channel)
                {
                    self.updates_message = Some((format!("{e:#}"), true));
                }
                if ui
                    .add_enabled_ui(!installing, |ui| {
                        confirm_button(ui, ("branch_build", channel), egui::Button::new("Remove"), "Delete this branch build and its settings", "Really remove?")
                    })
                    .inner
                {
                    let name = crate::channels::branch_name(channel);
                    self.updates_message = Some(match crate::channels::remove(channel) {
                        Ok(()) if cfg!(target_os = "android") => {
                            (format!("Android asks you to uninstall the {name} build."), false)
                        }
                        Ok(()) => (format!("Removed the {name} build."), false),
                        Err(e) => (format!("{e:#}"), true),
                    });
                }
            } else if let Some(b) = build
                && ui.add_enabled(!installing && can_install, egui::Button::new("Install")).clicked()
            {
                self.branches.install(b.clone());
            }
        });
    }
}

/// What an offered update brings: for a nightly the pull requests not in
/// a release yet, then its changelog's versions newer than this build's,
/// each linking to its page. Its notes, when its changelog couldn't be
/// read.
fn release_notes_ui(ui: &mut egui::Ui, release: &updater::Release) {
    let Some(changes) = &release.changes else {
        if !release.notes.trim().is_empty() {
            ui.label(release.notes.trim());
        }
        return;
    };
    if let Some(unreleased) = &changes.unreleased {
        let hint = format!("Merged since {}, newest first", unreleased.since);
        settings_group("Not released yet").icon("git-merge").hint(&hint).show(ui, |ui| match &unreleased.merged {
            Some(merged) if merged.is_empty() => {
                setting_row(ui, |ui| ui.weak("No pull requests merged since then."));
            }
            Some(merged) => {
                for pr in merged {
                    setting_row(ui, |ui| {
                        ui.hyperlink_to(format!("#{}", pr.number), pr.url());
                        ui.label(&pr.title);
                    });
                }
            }
            None => {
                setting_row(ui, |ui| ui.weak("Couldn't list them: see the release page."));
            }
        });
    }
    for version in &changes.releases {
        changelog_release_ui(ui, version, true);
    }
}

/// The changelog in the settings look: a section per version, newest
/// first, its groups (Added, Changed, Fixed) as dividers and a row per
/// line. With `since` (What's new after an update), the versions after it
/// are open and a marker shows where the version you had starts; the rest
/// fold away. Without, only the newest is open.
pub(super) fn changelog_body_ui(ui: &mut egui::Ui, since: Option<changelog::Version>) {
    let current = changelog::current_version();
    let mut marked = false;
    for (i, release) in changelog::releases().iter().enumerate() {
        // (Unreleased changes aren't in this build unless it's newer.)
        let new = match (since, release.version) {
            (Some(since), Some(v)) => v > since && current.is_none_or(|c| v <= c),
            (Some(_), None) => false,
            (None, _) => i == 0,
        };
        if let (Some(since), Some(v)) = (since, release.version)
            && v <= since
            && !marked
        {
            marked = true;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(icon("map-pin")).color(ui.visuals().selection.stroke.color));
                ui.label(
                    egui::RichText::new(format!("You had v{} before this update", changelog::format_version(since)))
                        .color(ui.visuals().selection.stroke.color),
                );
            });
        }
        changelog_release_ui(ui, release, new);
    }
}

/// One version of a changelog as a settings group: its groups (Added,
/// Changed, Fixed) as dividers and a row per line. Folds away by its
/// heading unless `open`.
fn changelog_release_ui(ui: &mut egui::Ui, release: &changelog::Release, open: bool) {
    let (title, date) = match release.version {
        Some(v) => {
            let date = release.heading.split_once('(').map(|(_, d)| d.trim_end_matches(')').trim().to_string());
            (format!("v{}", changelog::format_version(v)), date)
        }
        None => (release.heading.clone(), Some("Not in a release yet".to_string())),
    };
    let mut group = settings_group(&title).icon("tag");
    if let Some(date) = &date {
        group = group.hint(date);
    }
    if !open {
        group = group.closable(&release.heading);
    }
    group.show(ui, |ui| {
        for block in &release.blocks {
            match block {
                changelog::Block::Heading(text) => {
                    setting_row(ui, |ui| {
                        let kind = match text.as_str() {
                            "Added" | "New" => "plus-circle",
                            "Changed" => "arrows-clockwise",
                            "Fixed" => "wrench",
                            "Removed" => "minus-circle",
                            _ => "dot-outline",
                        };
                        ui.label(egui::RichText::new(icon(kind)).color(ui.visuals().weak_text_color()));
                        ui.strong(text);
                    });
                }
                changelog::Block::Bullet(text) | changelog::Block::Paragraph(text) => {
                    setting_row(ui, |ui| ui.label(inline_code_job(ui, "", text)));
                }
            }
        }
    });
}
