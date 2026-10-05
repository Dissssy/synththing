//! Reporting a script (Report... in the Library tab), and the Moderation
//! window for a library server's admins (docs/SERVER.md).
//!
//! A report says why, and can point at the problem: lines of the reported
//! version's code (click a line, shift-click for a range) and sprites
//! written in it (shown as they look). Admins see each report with those
//! lines highlighted in the code and the sprites drawn, and can hide,
//! delete, ban (a key or an address, for a while or for good), and mark
//! reports dealt with; the same actions as `synththing admin` on the
//! server. Whether the user is an admin on a server is asked when the
//! Library first talks to it, and again with each search until they are.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};

use eframe::egui;

use super::library::host;
use super::App;
use crate::library::identity::Identity;
use crate::library::{
    self, ago, client, lines_text, report_reason_title, AdminAction, Ban, Report, ReportRequest, MAX_REPORT_DETAILS,
    MAX_REPORT_MARKS, REPORT_REASONS,
};
use crate::sprite_code::{self, SpriteCall};

enum Event {
    /// Whether the identity is an admin on a server.
    Whoami(String, bool),
    /// The source of the version being reported.
    ReportSource(String, String, u32, Result<String, String>),
    Reported(Result<i64, String>),
    Reports(String, Result<Vec<Report>, String>),
    Bans(String, Result<Vec<Ban>, String>),
    Admins(String, Result<Vec<(String, String)>, String>),
    /// A reported version's source, for an admin: (server, script, version).
    Source(String, String, u32, Result<String, String>),
    /// A script's author's key (or, anonymous, the address it came from),
    /// to ban.
    Author(String, Result<String, String>),
    /// An admin action finished: what it was, and how it went.
    Done(String, String, Result<(), String>),
}

/// Report... being filled in.
struct ReportDraft {
    server: String,
    id: String,
    name: String,
    version: u32,
    reason: &'static str,
    details: String,
    source: Option<Result<String, String>>,
    sprites_found: Vec<SpriteCall>,
    lines: BTreeSet<u32>,
    /// The line clicked last, for shift-click ranges.
    anchor: Option<u32>,
    sprites: BTreeSet<u32>,
    error: Option<String>,
    sending: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Reports,
    Bans,
    Admins,
}

#[derive(Default)]
struct BanForm {
    target: String,
    reason: String,
    hours: String,
    hide_scripts: bool,
    ban_addresses: bool,
}

/// The Moderation window, for one server.
struct Window {
    server: String,
    tab: Tab,
    all: bool,
    reports: Option<Result<Vec<Report>, String>>,
    bans: Option<Result<Vec<Ban>, String>>,
    admins: Option<Result<Vec<(String, String)>, String>>,
    /// Reports showing what they point at.
    shown: HashSet<i64>,
    sources: HashMap<(String, u32), Result<String, String>>,
    confirm_delete: Option<String>,
    ban: BanForm,
    new_admin: String,
    message: Option<String>,
}

pub struct ModerationState {
    events: (Sender<Event>, Receiver<Event>),
    report: Option<ReportDraft>,
    admin_on: Vec<String>,
    asked: HashSet<String>,
    window: Option<Window>,
    /// Sprites drawn, by (script, version, line).
    sprite_textures: HashMap<(String, u32, u32), Option<egui::TextureHandle>>,
}

impl Default for ModerationState {
    fn default() -> Self {
        Self {
            events: mpsc::channel(),
            report: None,
            admin_on: Vec::new(),
            asked: HashSet::new(),
            window: None,
            sprite_textures: HashMap::new(),
        }
    }
}

impl ModerationState {
    pub fn open(&self) -> bool {
        self.report.is_some() || self.window.is_some()
    }

    pub fn is_admin_anywhere(&self) -> bool {
        !self.admin_on.is_empty()
    }
}

/// Lines as ranges: {3, 4, 5, 9} to [(3, 5), (9, 9)].
fn ranges(lines: &BTreeSet<u32>) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for &line in lines {
        match out.last_mut() {
            Some((_, end)) if *end + 1 == line => *end = line,
            _ => out.push((line, line)),
        }
    }
    out
}

fn in_ranges(ranges: &[(u32, u32)], line: u32) -> bool {
    ranges.iter().any(|&(a, b)| (a..=b).contains(&line))
}

/// A sprite as an image (its whole sheet), if it's written out in the code.
fn sprite_image(call: &SpriteCall) -> Option<egui::ColorImage> {
    let (image, palette) = (call.image.as_ref()?, call.palette.as_ref()?);
    let rows = &image.value;
    let width = rows.iter().map(Vec::len).max()?;
    if width == 0 || rows.is_empty() || width * rows.len() > 512 * 512 {
        return None;
    }
    let mut rgba = Vec::with_capacity(width * rows.len() * 4);
    for row in rows {
        for x in 0..width {
            let index = row.get(x).copied().unwrap_or(0) as usize;
            match index.checked_sub(1).and_then(|i| palette.value.get(i)) {
                Some(color) => {
                    let alpha = color.alpha.map_or(255, |a| (a.clamp(0.0, 1.0) * 255.0) as u8);
                    rgba.extend_from_slice(&[color.rgb[0], color.rgb[1], color.rgb[2], alpha]);
                }
                None => rgba.extend_from_slice(&[0, 0, 0, 0]),
            }
        }
    }
    Some(egui::ColorImage::from_rgba_unmultiplied([width, rows.len()], &rgba))
}

/// A drawn sprite, scaled up by whole pixels to about `height`.
fn sprite_ui(ui: &mut egui::Ui, texture: &egui::TextureHandle, height: f32) {
    let size = texture.size_vec2();
    let scale = (height / size.y).floor().max(1.0).min(256.0 / size.x.max(1.0)).max(0.25);
    egui::Frame::new().fill(ui.visuals().extreme_bg_color).inner_margin(4.0).corner_radius(4.0).show(ui, |ui| {
        ui.add(egui::Image::new((texture.id(), size * scale)));
    });
}

impl App {
    fn spawn_moderation(&self, job: impl FnOnce() -> Event + Send + 'static) {
        let tx = self.moderation.events.0.clone();
        std::thread::spawn(move || {
            let _ = tx.send(job());
        });
    }

    /// The first time the Library talks to `server`: is the user an admin
    /// there?
    pub(super) fn check_admin(&mut self, server: &str) {
        let (Some(identity), Some(key)) = (Identity::load(), self.config.library.pinned.get(server).cloned()) else {
            return;
        };
        if !self.moderation.asked.insert(server.to_string()) {
            return;
        }
        let server = server.to_string();
        self.spawn_moderation(move || {
            let admin = client::admin::<serde_json::Value>(&server, &key, &identity, &AdminAction::Whoami).is_ok();
            Event::Whoami(server, admin)
        });
    }

    /// Ask again whether the user is an admin, where they weren't.
    pub(super) fn forget_admin_answers(&mut self) {
        let admin_on = &self.moderation.admin_on;
        self.moderation.asked.retain(|server| admin_on.contains(server));
    }

    /// Report... for `version` of a script.
    pub(super) fn open_report(&mut self, server: &str, id: &str, name: &str, version: u32) {
        let Some(key) = self.config.library.pinned.get(server).cloned() else { return };
        self.moderation.report = Some(ReportDraft {
            server: server.to_string(),
            id: id.to_string(),
            name: name.to_string(),
            version,
            reason: REPORT_REASONS[0].0,
            details: String::new(),
            source: None,
            sprites_found: Vec::new(),
            lines: BTreeSet::new(),
            anchor: None,
            sprites: BTreeSet::new(),
            error: None,
            sending: false,
        });
        let (server, id) = (server.to_string(), id.to_string());
        self.spawn_moderation(move || {
            let source = client::download(&server, &id, Some(version), &key).map(|d| d.source);
            Event::ReportSource(server, id, version, source)
        });
    }

    /// Open the Moderation window (on the first server the user is an
    /// admin on).
    pub(super) fn open_moderation(&mut self) {
        let Some(server) = self.moderation.admin_on.first().cloned() else { return };
        self.moderation.window = Some(Window {
            server,
            tab: Tab::Reports,
            all: false,
            reports: None,
            bans: None,
            admins: None,
            shown: HashSet::new(),
            sources: HashMap::new(),
            confirm_delete: None,
            ban: BanForm::default(),
            new_admin: String::new(),
            message: None,
        });
        self.refresh_moderation();
    }

    /// Fetch the Moderation window's current tab again.
    fn refresh_moderation(&mut self) {
        let Some(window) = &mut self.moderation.window else { return };
        let (Some(identity), Some(key)) = (Identity::load(), self.config.library.pinned.get(&window.server).cloned()) else {
            return;
        };
        let server = window.server.clone();
        let (tab, all) = (window.tab, window.all);
        match tab {
            Tab::Reports => window.reports = None,
            Tab::Bans => window.bans = None,
            Tab::Admins => window.admins = None,
        }
        self.spawn_moderation(move || match tab {
            Tab::Reports => Event::Reports(server.clone(), client::admin(&server, &key, &identity, &AdminAction::Reports { all })),
            Tab::Bans => Event::Bans(server.clone(), client::admin(&server, &key, &identity, &AdminAction::Bans)),
            Tab::Admins => Event::Admins(server.clone(), client::admin(&server, &key, &identity, &AdminAction::Admins)),
        });
    }

    /// Run admin actions in turn (stopping at one that fails), then say how
    /// it went as `what`.
    fn moderate(&mut self, what: &str, actions: Vec<AdminAction>) {
        let Some(window) = &self.moderation.window else { return };
        let (Some(identity), Some(key)) = (Identity::load(), self.config.library.pinned.get(&window.server).cloned()) else {
            return;
        };
        let (server, what) = (window.server.clone(), what.to_string());
        self.spawn_moderation(move || {
            let result = actions
                .iter()
                .try_for_each(|action| client::admin::<serde_json::Value>(&server, &key, &identity, action).map(|_| ()));
            Event::Done(server, what, result)
        });
    }

    pub(super) fn poll_moderation(&mut self) {
        while let Ok(event) = self.moderation.events.1.try_recv() {
            let window = self.moderation.window.as_mut();
            match event {
                Event::Whoami(server, admin) => {
                    if admin && !self.moderation.admin_on.contains(&server) {
                        self.moderation.admin_on.push(server);
                    }
                }
                Event::ReportSource(server, id, version, source) => {
                    if let Some(draft) = &mut self.moderation.report
                        && draft.server == server
                        && draft.id == id
                        && draft.version == version
                    {
                        if let Ok(source) = &source {
                            draft.sprites_found =
                                sprite_code::find_sprites(source).into_iter().filter(SpriteCall::editable).collect();
                        }
                        draft.source = Some(source);
                    }
                }
                Event::Reported(result) => match result {
                    Ok(number) => {
                        if let Some(draft) = self.moderation.report.take() {
                            log::info!("reported {} on {} (report #{number})", draft.id, draft.server);
                            self.status = format!("Reported \"{}\": the server's moderators will look at it.", draft.name);
                        }
                    }
                    Err(e) => {
                        if let Some(draft) = &mut self.moderation.report {
                            draft.sending = false;
                            draft.error = Some(e);
                        }
                    }
                },
                Event::Reports(server, result) => {
                    if let Some(w) = window.filter(|w| w.server == server) {
                        w.reports = Some(result);
                    }
                }
                Event::Bans(server, result) => {
                    if let Some(w) = window.filter(|w| w.server == server) {
                        w.bans = Some(result);
                    }
                }
                Event::Admins(server, result) => {
                    if let Some(w) = window.filter(|w| w.server == server) {
                        w.admins = Some(result);
                    }
                }
                Event::Source(server, script, version, result) => {
                    if let Some(w) = window.filter(|w| w.server == server) {
                        w.sources.insert((script, version), result);
                    }
                }
                Event::Author(server, result) => {
                    if let Some(w) = window.filter(|w| w.server == server) {
                        match result {
                            Ok(target) => {
                                w.ban.target = target;
                                w.tab = Tab::Bans;
                                w.message = Some("Fill in why, then Ban.".into());
                            }
                            Err(e) => w.message = Some(format!("Couldn't look up its author: {e}")),
                        }
                    }
                }
                Event::Done(server, what, result) => {
                    if let Some(w) = window.filter(|w| w.server == server) {
                        w.message = Some(match result {
                            Ok(()) => what,
                            Err(e) => format!("Didn't work: {e}"),
                        });
                        self.refresh_moderation();
                        // (Listings change too.)
                        self.library_search();
                    }
                }
            }
        }
    }

    /// The Report window.
    pub(super) fn report_ui(&mut self, ctx: &egui::Context) {
        let Some(mut draft) = self.moderation.report.take() else { return };
        let mut send = false;
        let mut close = false;
        let response = egui::Modal::new(egui::Id::new("library_report")).show(ctx, |ui| {
            ui.set_width(680.0);
            ui.heading(format!("Report \"{}\"", draft.name));
            ui.weak(format!("Version {} on {}. Reports go to the server's moderators, signed with your identity.", draft.version, host(&draft.server)));
            ui.add_space(6.0);
            ui.label("Why?");
            for (reason, title) in REPORT_REASONS {
                ui.radio_value(&mut draft.reason, reason, title);
            }
            ui.add_space(6.0);
            ui.label("Anything the moderators should know (optional)");
            ui.add(egui::TextEdit::multiline(&mut draft.details).desired_rows(3).desired_width(f32::INFINITY).char_limit(MAX_REPORT_DETAILS));
            ui.add_space(6.0);
            ui.label("Point at the problem (optional): click lines of its code, shift-click for a range.");
            match &draft.source {
                None => {
                    ui.spinner();
                }
                Some(Err(e)) => {
                    ui.colored_label(ui.visuals().warn_fg_color, format!("Couldn't get its code: {e}"));
                }
                Some(Ok(source)) => {
                    let lines: Vec<&str> = source.lines().collect();
                    let row = ui.text_style_height(&egui::TextStyle::Monospace);
                    egui::Frame::new().fill(ui.visuals().extreme_bg_color).corner_radius(4.0).show(ui, |ui| {
                        egui::ScrollArea::both().id_salt("report_code").max_height(240.0).auto_shrink([false, true]).show_rows(
                            ui,
                            row,
                            lines.len(),
                            |ui, range| {
                                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                                ui.spacing_mut().item_spacing.y = 0.0;
                                for index in range {
                                    let number = index as u32 + 1;
                                    let text = egui::RichText::new(format!("{number:>5}  {}", lines[index])).monospace();
                                    let picked = draft.lines.contains(&number);
                                    if ui.selectable_label(picked, text).clicked() {
                                        let shift = ui.input(|i| i.modifiers.shift);
                                        match draft.anchor {
                                            Some(anchor) if shift => {
                                                let (a, b) = (anchor.min(number), anchor.max(number));
                                                draft.lines.extend(a..=b);
                                            }
                                            _ if picked => {
                                                draft.lines.remove(&number);
                                            }
                                            _ => {
                                                draft.lines.insert(number);
                                            }
                                        }
                                        draft.anchor = Some(number);
                                    }
                                }
                            },
                        );
                    });
                    if !draft.lines.is_empty() {
                        ui.horizontal(|ui| {
                            ui.weak(format!("Lines {}", lines_text(&ranges(&draft.lines))));
                            if ui.small_button("Clear").clicked() {
                                draft.lines.clear();
                            }
                        });
                    }
                    if !draft.sprites_found.is_empty() {
                        ui.add_space(4.0);
                        ui.label("Sprites in it: tick any that are a problem.");
                        ui.horizontal_wrapped(|ui| {
                            for call in &draft.sprites_found {
                                let key = (draft.id.clone(), draft.version, call.line as u32);
                                let texture = self.moderation.sprite_textures.entry(key).or_insert_with(|| {
                                    sprite_image(call).map(|image| {
                                        ctx.load_texture(format!("report-sprite-{}", call.line), image, egui::TextureOptions::NEAREST)
                                    })
                                });
                                ui.vertical(|ui| {
                                    if let Some(texture) = texture {
                                        sprite_ui(ui, texture, 48.0);
                                    }
                                    let mut ticked = draft.sprites.contains(&(call.line as u32));
                                    if ui.checkbox(&mut ticked, call.label()).changed() {
                                        if ticked {
                                            draft.sprites.insert(call.line as u32);
                                        } else {
                                            draft.sprites.remove(&(call.line as u32));
                                        }
                                    }
                                });
                            }
                        });
                    }
                }
            }
            let marks = ranges(&draft.lines).len() + draft.sprites.len();
            if marks > MAX_REPORT_MARKS {
                ui.colored_label(ui.visuals().warn_fg_color, format!("That's a lot: a report can point at up to {MAX_REPORT_MARKS} stretches of lines and sprites."));
            }
            if let Some(error) = &draft.error {
                ui.colored_label(ui.visuals().warn_fg_color, error);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let can = !draft.sending && marks <= MAX_REPORT_MARKS;
                if ui.add_enabled(can, egui::Button::new("Send report")).clicked() {
                    send = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
                if draft.sending {
                    ui.spinner();
                }
            });
        });
        if send {
            match (self.ensure_identity(), self.config.library.pinned.get(&draft.server).cloned()) {
                (Ok(identity), Some(key)) => {
                    draft.sending = true;
                    draft.error = None;
                    let request = ReportRequest {
                        reason: draft.reason.to_string(),
                        details: draft.details.trim().to_string(),
                        version: draft.version,
                        lines: ranges(&draft.lines),
                        sprites: draft.sprites.iter().copied().collect(),
                    };
                    let (server, id) = (draft.server.clone(), draft.id.clone());
                    self.spawn_moderation(move || Event::Reported(client::report(&server, &key, &id, &identity, &request)));
                }
                (Err(e), _) => draft.error = Some(format!("Couldn't make your identity: {e}")),
                (_, None) => draft.error = Some("That server's key isn't known yet.".into()),
            }
        }
        if !(close || (response.should_close() && !draft.sending)) {
            self.moderation.report = Some(draft);
        }
    }

    /// The Moderation window.
    pub(super) fn moderation_ui(&mut self, ctx: &egui::Context) {
        let Some(mut window) = self.moderation.window.take() else { return };
        let mut close = false;
        let mut refresh = false;
        let mut actions: Vec<(String, Vec<AdminAction>)> = Vec::new();
        let mut fetch_source: Vec<(String, u32)> = Vec::new();
        let mut ban_author: Option<String> = None;
        let response = egui::Modal::new(egui::Id::new("library_moderation")).show(ctx, |ui| {
            ui.set_width(760.0);
            ui.horizontal(|ui| {
                ui.heading("Moderation");
                if self.moderation.admin_on.len() > 1 {
                    egui::ComboBox::from_id_salt("moderation_server").selected_text(host(&window.server)).show_ui(ui, |ui| {
                        for server in &self.moderation.admin_on {
                            if ui.selectable_label(*server == window.server, host(server)).clicked() && *server != window.server {
                                window.server = server.clone();
                                window.sources.clear();
                                refresh = true;
                            }
                        }
                    });
                } else {
                    ui.weak(host(&window.server));
                }
            });
            ui.horizontal(|ui| {
                for (tab, title) in [(Tab::Reports, "Reports"), (Tab::Bans, "Bans"), (Tab::Admins, "Admins")] {
                    if ui.selectable_label(window.tab == tab, title).clicked() && window.tab != tab {
                        window.tab = tab;
                        refresh = true;
                    }
                }
                ui.separator();
                if window.tab == Tab::Reports && ui.checkbox(&mut window.all, "Dealt with too").changed() {
                    refresh = true;
                }
                if ui.button("Refresh").clicked() {
                    refresh = true;
                }
            });
            if let Some(message) = &window.message {
                ui.weak(message);
            }
            ui.separator();
            egui::ScrollArea::vertical().id_salt("moderation_body").max_height(520.0).auto_shrink([false, true]).show(ui, |ui| {
                match window.tab {
                    Tab::Reports => self.reports_tab_ui(ui, ctx, &mut window, &mut actions, &mut fetch_source, &mut ban_author),
                    Tab::Bans => bans_tab_ui(ui, &mut window, &mut actions),
                    Tab::Admins => admins_tab_ui(ui, &mut window, &mut actions),
                }
            });
            ui.separator();
            if ui.button("Close").clicked() {
                close = true;
            }
        });
        let server = window.server.clone();
        let keep = !(close || response.should_close());
        if keep {
            self.moderation.window = Some(window);
        }
        if refresh {
            self.refresh_moderation();
        }
        for (what, list) in actions {
            self.moderate(&what, list);
        }
        if let (Some(identity), Some(key)) = (Identity::load(), self.config.library.pinned.get(&server).cloned()) {
            for (script, version) in fetch_source {
                let (server, key, identity) = (server.clone(), key.clone(), identity.clone());
                self.spawn_moderation(move || {
                    let result = client::admin::<serde_json::Value>(
                        &server,
                        &key,
                        &identity,
                        &AdminAction::Source { script: script.clone(), version: Some(version) },
                    )
                    .and_then(|v| v["source"].as_str().map(str::to_string).ok_or_else(|| "no source".to_string()));
                    Event::Source(server, script, version, result)
                });
            }
            if let Some(script) = ban_author {
                self.spawn_moderation(move || {
                    let info = client::admin::<library::AdminScriptInfo>(&server, &key, &identity, &AdminAction::Info { script });
                    let target = info.and_then(|info| {
                        info.author_key
                            .or_else(|| info.uploads.iter().rev().find_map(|(_, ip, _)| ip.clone()))
                            .ok_or_else(|| "it's anonymous and its address has been forgotten".to_string())
                    });
                    Event::Author(server, target)
                });
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn reports_tab_ui(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        window: &mut Window,
        actions: &mut Vec<(String, Vec<AdminAction>)>,
        fetch_source: &mut Vec<(String, u32)>,
        ban_author: &mut Option<String>,
    ) {
        let reports = match &window.reports {
            None => {
                ui.spinner();
                return;
            }
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().warn_fg_color, format!("Couldn't get the reports: {e}"));
                return;
            }
            Some(Ok(reports)) => reports.clone(),
        };
        if reports.is_empty() {
            ui.weak(if window.all { "No reports." } else { "No open reports." });
        }
        for report in &reports {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.strong(format!("\"{}\"", report.script_name));
                    ui.weak(format!("v{} ({})", report.request.version, report.script_id));
                    ui.label(report_reason_title(&report.request.reason));
                    ui.weak(format!("by #{}, {}", report.reporter_id, ago(report.created)));
                    let state = match (&report.resolved, report.exists, report.hidden) {
                        (Some(_), ..) => Some(format!("dealt with: {}", report.resolution.as_deref().unwrap_or_default())),
                        (None, false, _) => Some("deleted".to_string()),
                        (None, true, true) => Some("hidden".to_string()),
                        _ => None,
                    };
                    if let Some(state) = state {
                        ui.colored_label(ui.visuals().warn_fg_color, state);
                    }
                });
                if !report.request.details.is_empty() {
                    ui.label(&report.request.details);
                }
                let points = !report.request.lines.is_empty() || !report.request.sprites.is_empty();
                let shown = window.shown.contains(&report.id);
                ui.horizontal_wrapped(|ui| {
                    let label = match (shown, points) {
                        (true, _) => "Hide code",
                        (false, true) => "Show what it points at",
                        (false, false) => "Show code",
                    };
                    if report.exists && ui.button(label).clicked() {
                        if shown {
                            window.shown.remove(&report.id);
                        } else {
                            window.shown.insert(report.id);
                            let key = (report.script_id.clone(), report.request.version);
                            if !window.sources.contains_key(&key) {
                                fetch_source.push(key);
                            }
                        }
                    }
                    if report.exists {
                        let resolve = |note: &str| AdminAction::Resolve { report: report.id, note: note.to_string() };
                        let script = report.script_id.clone();
                        if report.hidden {
                            if ui.button("Unhide").clicked() {
                                actions.push(("Unhidden.".into(), vec![AdminAction::Unhide { script: script.clone() }]));
                            }
                        } else if ui.button("Hide").on_hover_text("Take it out of listings and downloads (it can be unhidden)").clicked() {
                            let reason = report_reason_title(&report.request.reason).to_string();
                            let mut list = vec![AdminAction::Hide { script: script.clone(), reason }];
                            if report.resolved.is_none() {
                                list.push(resolve("hidden"));
                            }
                            actions.push(("Hidden.".into(), list));
                        }
                        let confirming = window.confirm_delete.as_deref() == Some(script.as_str());
                        if ui
                            .button(if confirming { "Click again to delete" } else { "Delete" })
                            .on_hover_text("Delete it for good, every version (the report stays)")
                            .clicked()
                        {
                            if confirming {
                                window.confirm_delete = None;
                                let mut list = vec![AdminAction::Delete { script: script.clone() }];
                                if report.resolved.is_none() {
                                    list.push(resolve("deleted"));
                                }
                                actions.push(("Deleted.".into(), list));
                            } else {
                                window.confirm_delete = Some(script.clone());
                            }
                        }
                        if ui.button("Ban its author...").clicked() {
                            *ban_author = Some(script.clone());
                        }
                    }
                    if report.resolved.is_none()
                        && ui.button("Dismiss").on_hover_text("Mark it dealt with, nothing done").clicked()
                    {
                        actions.push((
                            "Dismissed.".into(),
                            vec![AdminAction::Resolve { report: report.id, note: "dismissed".into() }],
                        ));
                    }
                });
                if shown {
                    self.evidence_ui(ui, ctx, window, report);
                }
            });
        }
    }

    /// The reported version's code, its marked lines highlighted (with a
    /// little around them; all of it if none were marked), and the sprites
    /// it points at, drawn.
    fn evidence_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, window: &Window, report: &Report) {
        let key = (report.script_id.clone(), report.request.version);
        let source = match window.sources.get(&key) {
            None => {
                ui.spinner();
                return;
            }
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().warn_fg_color, format!("Couldn't get its code: {e}"));
                return;
            }
            Some(Ok(source)) => source,
        };
        let marked = &report.request.lines;
        let lines: Vec<&str> = source.lines().collect();
        const AROUND: u32 = 2;
        let near = |n: u32| marked.is_empty() || marked.iter().any(|&(a, b)| n + AROUND >= a && n <= b + AROUND);
        let highlight = ui.visuals().selection.bg_fill.gamma_multiply(0.6);
        egui::Frame::new().fill(ui.visuals().extreme_bg_color).corner_radius(4.0).inner_margin(4.0).show(ui, |ui| {
            egui::ScrollArea::both().id_salt(("evidence", report.id)).max_height(260.0).auto_shrink([false, true]).show(ui, |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                ui.spacing_mut().item_spacing.y = 0.0;
                let mut skipped = false;
                for (index, line) in lines.iter().enumerate() {
                    let number = index as u32 + 1;
                    if !near(number) {
                        skipped = true;
                        continue;
                    }
                    if skipped {
                        ui.weak("   ...");
                        skipped = false;
                    }
                    let mut text = egui::RichText::new(format!("{number:>5}  {line}")).monospace();
                    if in_ranges(marked, number) {
                        text = text.background_color(highlight).strong();
                    }
                    ui.label(text);
                }
            });
        });
        if !report.request.sprites.is_empty() {
            let calls = sprite_code::find_sprites(source);
            ui.horizontal_wrapped(|ui| {
                for &line in &report.request.sprites {
                    let Some(call) = calls.iter().find(|c| c.line as u32 == line) else {
                        ui.weak(format!("(no sprite at line {line})"));
                        continue;
                    };
                    let cache = (report.script_id.clone(), report.request.version, line);
                    let texture = self.moderation.sprite_textures.entry(cache).or_insert_with(|| {
                        sprite_image(call).map(|image| {
                            ctx.load_texture(format!("evidence-{}-{line}", report.id), image, egui::TextureOptions::NEAREST)
                        })
                    });
                    ui.vertical(|ui| {
                        match texture {
                            Some(texture) => sprite_ui(ui, texture, 64.0),
                            None => {
                                ui.weak("(not drawable)");
                            }
                        }
                        ui.weak(call.label());
                    });
                }
            });
        }
    }
}

fn bans_tab_ui(ui: &mut egui::Ui, window: &mut Window, actions: &mut Vec<(String, Vec<AdminAction>)>) {
    ui.label("Ban a key (an ID like #k3f9q2xa, or the whole key) or an address:");
    egui::Grid::new("ban_form").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Who");
        ui.add(egui::TextEdit::singleline(&mut window.ban.target).desired_width(360.0));
        ui.end_row();
        ui.label("Why");
        ui.add(egui::TextEdit::singleline(&mut window.ban.reason).desired_width(360.0));
        ui.end_row();
        ui.label("Hours");
        ui.add(egui::TextEdit::singleline(&mut window.ban.hours).hint_text("empty: for good").desired_width(120.0));
        ui.end_row();
        ui.label("");
        ui.checkbox(&mut window.ban.hide_scripts, "Also hide everything they uploaded");
        ui.end_row();
        ui.label("");
        ui.checkbox(&mut window.ban.ban_addresses, "Also ban the addresses they've used")
            .on_hover_text(
                "Every address the key was seen on in the last 30 days is banned for 30 days, and so is any new one it \
                 comes back from while it's banned, so a new key on the same connection doesn't get around it. \
                 Addresses can be shared (a household, a phone network), so use it for the ones who keep at it.",
            );
        ui.end_row();
    });
    let hours = window.ban.hours.trim();
    let hours_ok = hours.is_empty() || hours.parse::<u64>().is_ok_and(|h| h > 0);
    let ready = !window.ban.target.trim().is_empty() && !window.ban.reason.trim().is_empty() && hours_ok;
    if ui.add_enabled(ready, egui::Button::new("Ban")).clicked() {
        actions.push((
            format!("Banned {}.", window.ban.target.trim()),
            vec![AdminAction::Ban {
                target: window.ban.target.trim().to_string(),
                hours: hours.parse().ok(),
                reason: window.ban.reason.trim().to_string(),
                hide_scripts: window.ban.hide_scripts,
                ban_addresses: window.ban.ban_addresses,
            }],
        ));
        window.ban = BanForm::default();
    }
    ui.separator();
    match &window.bans {
        None => {
            ui.spinner();
        }
        Some(Err(e)) => {
            ui.colored_label(ui.visuals().warn_fg_color, format!("Couldn't get the bans: {e}"));
        }
        Some(Ok(bans)) if bans.is_empty() => {
            ui.weak("Nobody's banned.");
        }
        Some(Ok(bans)) => {
            for ban in bans {
                ui.horizontal_wrapped(|ui| {
                    let who = if ban.target.len() == 64 { format!("#{}", library::key_id(&ban.target)) } else { ban.target.clone() };
                    ui.strong(who);
                    let until = match ban.until {
                        Some(until) => format!(
                            "for another {}",
                            crate::song_info::format_length((until - now()).max(0) as f64)
                        ),
                        None => "for good".to_string(),
                    };
                    ui.weak(format!("{until}, since {}: {}", ago(ban.created), ban.reason));
                    if ui.small_button("Unban").clicked() {
                        actions.push(("Unbanned.".into(), vec![AdminAction::Unban { target: ban.target.clone() }]));
                    }
                });
            }
        }
    }
}

fn admins_tab_ui(ui: &mut egui::Ui, window: &mut Window, actions: &mut Vec<(String, Vec<AdminAction>)>) {
    ui.weak("Admins moderate from here, signed with their identity. The server's own key (synththing admin, on the server) always can.");
    match &window.admins {
        None => {
            ui.spinner();
        }
        Some(Err(e)) => {
            ui.colored_label(ui.visuals().warn_fg_color, format!("Couldn't get the admins: {e}"));
        }
        Some(Ok(admins)) => {
            for (key, id) in admins {
                ui.horizontal(|ui| {
                    ui.strong(format!("#{id}"));
                    if ui.small_button("Remove").clicked() {
                        actions.push((format!("#{id} isn't an admin now."), vec![AdminAction::RemoveAdmin { key: key.clone() }]));
                    }
                });
            }
        }
    }
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut window.new_admin).hint_text("#ID or key").desired_width(240.0));
        if ui.add_enabled(!window.new_admin.trim().is_empty(), egui::Button::new("Add admin")).clicked() {
            actions.push((
                format!("{} is an admin.", window.new_admin.trim()),
                vec![AdminAction::AddAdmin { key: window.new_admin.trim().to_string() }],
            ));
            window.new_admin.clear();
        }
    });
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}
