//! Updates: checking on launch, the Updates window, What's new, and branch
//! builds.

use std::time::Duration;

use eframe::egui;

use crate::updater::{self, Track, UpdateState};
use crate::changelog;

use super::{App, card, card_badge, close, confirm_button, flow, icon, inline_code_job, setting_row, settings_group, status_pill};

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

    /// The Updates page: a card with what the update check found and its
    /// buttons, then for an update what it brings (its changelog back to
    /// this version), then branch builds. Whether a way out (Not now,
    /// Skip, Later, OK) was clicked.
    pub(super) fn updates_body_ui(&mut self, ui: &mut egui::Ui) -> bool {
        let state = self.updater.state();
        let current = updater::current_version();
        let mut close = false;
        let branch = match crate::build_info::channel() {
            crate::build_info::Channel::Branch(name) => Some(name),
            _ => None,
        };
        let accent = ui.visuals().selection.stroke.color;
        let test_color = ui.visuals().warn_fg_color;
        let error_color = ui.visuals().error_fg_color;
        let (badge, badge_color) = match &state {
            UpdateState::Idle | UpdateState::Checking => ("arrows-clockwise", accent),
            UpdateState::UpToDate => ("check-circle", crate::theme::palette(ui.ctx()).playing.0),
            UpdateState::Available(_) | UpdateState::Downloading { .. } => ("download-simple", accent),
            UpdateState::Installed(_) => ("check-circle", crate::theme::palette(ui.ctx()).playing.0),
            UpdateState::Gone => ("git-branch", test_color),
            UpdateState::Failed(_) => ("warning", error_color),
        };
        // What this build follows, said under each card's title.
        let follows = match branch {
            Some(name) => format!(
                "A test build of the {name} branch: it updates from that branch and keeps settings of its own, so \
                 your usual synththing isn't affected."
            ),
            None if self.config.canary_updates() => "Canary updates are on (Preferences > App).".to_string(),
            None => String::new(),
        };
        card(ui, |ui| {
            ui.horizontal_top(|ui| {
                card_badge(ui, badge_color.gamma_multiply(0.2), badge, badge_color);
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    let title = |ui: &mut egui::Ui, text: &str| {
                        ui.label(egui::RichText::new(text).size(20.0).strong());
                    };
                    match &state {
                        UpdateState::Idle => {
                            title(ui, "Updates");
                            ui.weak(format!("You have v{current}."));
                        }
                        UpdateState::Checking => {
                            ui.horizontal(|ui| {
                                title(ui, "Checking for updates");
                                ui.spinner();
                            });
                            ui.weak(format!("You have v{current}."));
                        }
                        UpdateState::UpToDate => {
                            title(ui, "You're up to date");
                            ui.weak(format!("v{current} is the newest."));
                        }
                        UpdateState::Available(release) => {
                            ui.horizontal(|ui| {
                                title(ui, &format!("v{}", release.version));
                                match release.tag.as_str() {
                                    "nightly" => status_pill(ui, "Nightly", test_color),
                                    tag if tag.starts_with("branch-") => status_pill(ui, "Branch build", test_color),
                                    _ => {}
                                }
                            });
                            if release.tag == "nightly" {
                                ui.weak("The newest build from master. For testing: it may be broken.");
                            }
                            ui.weak(format!("You have v{current} · {:.0} MB", release.size() as f64 / 1e6));
                        }
                        UpdateState::Downloading { release, downloaded } => {
                            title(ui, &format!("Downloading v{}", release.version));
                            let total = release.size().max(1);
                            ui.add(
                                egui::ProgressBar::new(*downloaded as f32 / total as f32)
                                    .text(format!("{:.1} / {:.1} MB", *downloaded as f64 / 1e6, total as f64 / 1e6)),
                            );
                        }
                        UpdateState::Installed(version) if cfg!(target_os = "android") => {
                            title(ui, &format!("Installing v{version}"));
                            ui.weak("Android asks you to install it; synththing restarts with it.");
                        }
                        UpdateState::Installed(version) => {
                            title(ui, &format!("v{version} is installed"));
                            ui.weak("It runs the next time synththing starts.");
                        }
                        UpdateState::Gone => {
                            title(ui, "This branch build is gone");
                            let name = branch.unwrap_or_default();
                            if cfg!(target_os = "android") {
                                ui.weak(format!(
                                    "The {name} branch was merged or deleted, so this app won't get updates any \
                                     more. Uninstall it when you're done with it."
                                ));
                            } else {
                                ui.weak(format!(
                                    "The {name} branch was merged or deleted, so this build won't get updates any \
                                     more. Go back to your usual synththing, and remove this one there, under Help > \
                                     Check for updates > Branch builds."
                                ));
                            }
                        }
                        UpdateState::Failed(error) => {
                            title(ui, "Something went wrong");
                            ui.colored_label(error_color, error);
                        }
                    }
                    if !follows.is_empty() && !matches!(state, UpdateState::Gone) {
                        ui.weak(&follows);
                    }
                    ui.add_space(4.0);
                    close = self.update_card_buttons_ui(ui, &state);
                });
            });
        });
        if let UpdateState::Available(release) = &state {
            ui.add_space(4.0);
            release_changes_ui(ui, release, current);
        }
        if branch.is_none() {
            self.branch_builds_ui(ui);
        } else if !cfg!(target_os = "android") {
            self.recopy_settings_ui(ui);
        }
        if let Some((message, error)) = &self.updates_message {
            if *error {
                ui.colored_label(error_color, message);
            } else {
                ui.label(message);
            }
        }
        close
    }

    /// The update card's buttons for `state`; whether a way out was
    /// clicked.
    fn update_card_buttons_ui(&mut self, ui: &mut egui::Ui, state: &UpdateState) -> bool {
        let mut close = false;
        ui.horizontal_wrapped(|ui| match state {
            UpdateState::Idle => {
                if ui.button("Check for updates").clicked() {
                    self.updater.check(self.update_track());
                }
            }
            UpdateState::UpToDate => {
                if ui.button("Check again").clicked() {
                    self.updater.check(self.update_track());
                }
            }
            UpdateState::Failed(_) => {
                if ui.button("Try again").clicked() {
                    self.updater.check(self.update_track());
                }
            }
            UpdateState::Available(release) => {
                if let Some(announce) = self.android_update_ui(ui) {
                    if announce {
                        // Shown this once: next time, only if there's a newer one.
                        self.skip_update(release);
                        close = true;
                    }
                } else {
                    if ui
                        .add_enabled(!cfg!(debug_assertions), egui::Button::new("Install"))
                        .on_disabled_hover_text("This is a development build (cargo run); updates install into release builds.")
                        .clicked()
                    {
                        self.updater.install(release.clone());
                    }
                    if ui.button("Skip this version").clicked() {
                        self.skip_update(release);
                        close = true;
                    }
                    if ui.button("Not now").clicked() {
                        close = true;
                    }
                }
                ui.hyperlink_to("Release page", &release.page_url);
            }
            UpdateState::Installed(_) if cfg!(target_os = "android") => {
                if ui.button("OK").clicked() {
                    close = true;
                }
            }
            UpdateState::Installed(_) => {
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
            }
            UpdateState::Gone => {
                if !cfg!(target_os = "android")
                    && let Some(stable) = crate::channels::installed_by()
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
            UpdateState::Checking | UpdateState::Downloading { .. } => {}
        });
        close
    }

    /// Don't offer `release` again: only a newer one.
    fn skip_update(&mut self, release: &updater::Release) {
        self.config.skipped_update = Some(release.version.to_string());
        if let Err(e) = self.config.save() {
            self.status = format!("Couldn't save preferences: {e}");
        }
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

    /// Updates' Branch builds group: trying a feature before it's merged.
    /// Each installs into a folder of its own with a copy of the settings,
    /// or on Android as an app of its own (`channels.rs`), so this install
    /// isn't touched. The list is fetched when Updates opens, and the group
    /// is shown only once there's a build (or an installed one), folded
    /// shut at first.
    pub(super) fn branch_builds_ui(&mut self, ui: &mut egui::Ui) {
        if matches!(self.branches.list(), crate::channels::ListState::Idle) {
            self.branches.refresh();
        }
        let installed = crate::channels::installed();
        let list = self.branches.list();
        let install = self.branches.install_state();
        let builds: &[updater::Release] = match &list {
            crate::channels::ListState::Loaded(builds) => builds,
            _ => &[],
        };
        // Every branch with a build, then installed ones whose build is gone.
        let mut rows: Vec<(String, Option<&updater::Release>)> = builds.iter().map(|b| (b.tag.clone(), Some(b))).collect();
        for channel in &installed {
            if !builds.iter().any(|b| &b.tag == channel) {
                rows.push((channel.clone(), None));
            }
        }
        if rows.is_empty() && matches!(install, crate::channels::InstallState::Idle) {
            return;
        }
        let installing = matches!(install, crate::channels::InstallState::Installing { .. });
        // Android has to let this app install others first.
        #[cfg(target_os = "android")]
        let can_install = crate::android::can_install_packages();
        #[cfg(not(target_os = "android"))]
        let can_install = true;
        let hint = format!("{} to try before they're merged", rows.len());
        let busy = self.branches.busy();
        let branches = self.branches.clone();
        let mut refresh = |ui: &mut egui::Ui| {
            if ui.add_enabled(!busy, super::row_icon_button(ui, "arrows-clockwise")).on_hover_text("Look again").clicked() {
                branches.refresh();
            }
        };
        settings_group("Branch builds").icon("git-branch").hint(&hint).closable("branch_builds").buttons(&mut refresh).show(ui, |ui| {
            setting_row(ui, |ui| {
                ui.weak(if cfg!(target_os = "android") {
                    "Each branch build installs as an app of its own, with settings of its own, and its online \
                     features are off. This synththing isn't touched."
                } else {
                    "Each branch build installs separately, starting with a copy of your settings, and its online \
                     features are off. This synththing isn't touched."
                })
            });
            #[cfg(target_os = "android")]
            if !can_install {
                setting_row(ui, |ui| {
                    ui.label("Android doesn't allow synththing to install apps yet.");
                    if ui.button("Allow it...").on_hover_text("Settings > Install unknown apps, for synththing").clicked() {
                        crate::android::open_install_settings();
                    }
                });
            }
            for (channel, build) in rows {
                setting_row(ui, |ui| {
                    ui.strong(crate::channels::branch_name(&channel));
                    match build {
                        Some(b) => {
                            let commit = b.commit.as_deref().map(|c| &c[..c.len().min(7)]).unwrap_or_default();
                            ui.weak(format!("{} {commit}", b.version)).on_hover_text(b.notes.trim());
                        }
                        None => {
                            ui.weak("its build is gone (merged?)");
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        self.branch_build_buttons_ui(ui, &channel, build, &installed, installing, can_install);
                    });
                });
            }
            match &install {
                crate::channels::InstallState::Idle => {}
                crate::channels::InstallState::Installing { channel, downloaded, size } => {
                    setting_row(ui, |ui| {
                        ui.label(format!("Installing the {} build", crate::channels::branch_name(channel)));
                        ui.add(
                            egui::ProgressBar::new(*downloaded as f32 / (*size).max(1) as f32)
                                .text(format!("{:.1} / {:.1} MB", *downloaded as f64 / 1e6, *size as f64 / 1e6)),
                        );
                    });
                }
                crate::channels::InstallState::Done(channel) if cfg!(target_os = "android") => {
                    setting_row(ui, |ui| {
                        ui.label(format!(
                            "Android asks you to install the {} build. Once it's in, open it here or from your apps.",
                            crate::channels::branch_name(channel)
                        ))
                    });
                }
                crate::channels::InstallState::Done(channel) => {
                    setting_row(ui, |ui| {
                        ui.label(format!("The {} build is installed and starting.", crate::channels::branch_name(channel)))
                    });
                }
                crate::channels::InstallState::Failed(error) => {
                    setting_row(ui, |ui| ui.colored_label(ui.visuals().error_fg_color, error));
                }
            }
        });
    }

    /// A branch build's buttons, in a right-to-left layout: Install, or
    /// Open and Remove.
    fn branch_build_buttons_ui(
        &mut self,
        ui: &mut egui::Ui,
        channel: &str,
        build: Option<&updater::Release>,
        installed: &[String],
        installing: bool,
        can_install: bool,
    ) {
        if installed.iter().any(|c| c == channel) {
            if ui
                .add_enabled_ui(!installing, |ui| {
                    confirm_button(ui, ("branch_build", channel), egui::Button::new("Remove"), "Delete this branch build and its settings", "Really remove?")
                })
                .inner
            {
                let name = crate::channels::branch_name(channel);
                self.updates_message = Some(match crate::channels::remove(channel) {
                    Ok(()) if cfg!(target_os = "android") => (format!("Android asks you to uninstall the {name} build."), false),
                    Ok(()) => (format!("Removed the {name} build."), false),
                    Err(e) => (format!("{e:#}"), true),
                });
            }
            if ui.add_enabled(!installing, egui::Button::new("Open")).clicked()
                && let Err(e) = crate::channels::launch(channel)
            {
                self.updates_message = Some((format!("{e:#}"), true));
            }
        } else if let Some(b) = build
            && ui.add_enabled(!installing && can_install, egui::Button::new("Install")).clicked()
        {
            self.branches.install(b.clone());
        }
    }
}

/// What an offered update brings: for a nightly the pull requests not in
/// a release yet, then its changelog's versions newer than this build's,
/// down to a marker for the version you have. Its notes, when its
/// changelog couldn't be read.
fn release_changes_ui(ui: &mut egui::Ui, release: &updater::Release, current: semver::Version) {
    let Some(changes) = &release.changes else {
        if !release.notes.trim().is_empty() {
            ui.label(release.notes.trim());
        }
        return;
    };
    if let Some(unreleased) = &changes.unreleased {
        let hint = format!("Merged since {}, newest first", unreleased.since);
        settings_group("Not in a release yet").icon("git-merge").hint(&hint).show(ui, |ui| match &unreleased.merged {
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
    you_had_ui(ui, &format!("You have v{current}"));
}

/// Where the version you had (or have) starts, in a changelog.
fn you_had_ui(ui: &mut egui::Ui, text: &str) {
    let color = ui.visuals().selection.stroke.color;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(icon("map-pin")).color(color));
        ui.label(egui::RichText::new(text).color(color));
    });
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
            you_had_ui(ui, &format!("You had v{} before this update", changelog::format_version(since)));
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
