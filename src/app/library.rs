//! The Script Library tab (docs/SERVER.md): scripts shared on library
//! servers, searched across every server in use, installed into the
//! scripts folder (with a `.source.json` beside each, saying where it came
//! from), and the current script published (Publish...). Servers are set
//! up in Preferences > Library.
//!
//! Requests run on threads of their own and come back as `Event`s, picked
//! up each frame (`poll_library`). A server's key is pinned the first time
//! it's seen; one whose key changes isn't used until the user trusts it
//! again.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};

use eframe::egui;

use super::{info_icon, with_info, App};
use crate::config::LibraryServer;
use crate::library::{self, client, Info, Installed, Listing, Receipt, ScriptDetails, ScriptSummary, Upload};
use crate::lua_visualizer;

/// Sort orders, as the API names them and as shown.
const SORTS: [(&str, &str); 2] = [("new", "Newest"), ("top", "Most encores")];

enum Event {
    Info(String, Result<Info, String>),
    /// A search's results from one server (the search's generation first).
    Listing(u64, String, Result<Listing, String>),
    Details(String, Result<ScriptDetails, String>),
    Downloaded(String, ScriptSummary, Result<client::Download, String>),
    Published(String, PathBuf, String, Result<Receipt, String>),
}

/// A script found on a server.
struct Found {
    server: String,
    summary: ScriptSummary,
}

/// What's typed into Publish... so far.
struct PublishDraft {
    script: PathBuf,
    name: String,
    description: String,
    category: &'static str,
    tags: String,
    author_name: String,
    server: String,
    error: Option<String>,
}

pub struct LibraryState {
    query: String,
    category: Option<&'static str>,
    sort: &'static str,
    /// Bumped by each search, so late results from an older one are dropped.
    generation: u64,
    searching: usize,
    searched: bool,
    results: Vec<Found>,
    /// How many each server has in all, for "first 30 of 120".
    totals: HashMap<String, u64>,
    /// Servers that couldn't be searched, and why.
    errors: Vec<(String, String)>,
    infos: HashMap<String, Info>,
    /// Servers whose key isn't the one pinned: the new key.
    key_changed: HashMap<String, String>,
    selected: Option<(String, String)>,
    details: Option<(String, ScriptDetails)>,
    /// A download or upload is under way.
    busy: bool,
    publish: Option<PublishDraft>,
    /// Installed scripts by (server, id): their file and version.
    installed: HashMap<(String, String), (PathBuf, u32)>,
    /// The address typed into Preferences > Library.
    new_server: String,
    events: (Sender<Event>, Receiver<Event>),
}

impl LibraryState {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            category: None,
            sort: "new",
            generation: 0,
            searching: 0,
            searched: false,
            results: Vec::new(),
            totals: HashMap::new(),
            errors: Vec::new(),
            infos: HashMap::new(),
            key_changed: HashMap::new(),
            selected: None,
            details: None,
            busy: false,
            publish: None,
            installed: HashMap::new(),
            new_server: String::new(),
            events: mpsc::channel(),
        }
    }

    pub fn publish_open(&self) -> bool {
        self.publish.is_some()
    }
}

/// A server's address as shown: its host.
fn host(url: &str) -> &str {
    url.split("://").nth(1).unwrap_or(url)
}

impl App {
    fn spawn_request(&self, job: impl FnOnce() -> Event + Send + 'static) {
        let tx = self.library.events.0.clone();
        std::thread::spawn(move || {
            let _ = tx.send(job());
        });
    }

    /// Search every server in use (with what's typed and picked).
    fn library_search(&mut self) {
        let lib = &mut self.library;
        lib.generation += 1;
        lib.results.clear();
        lib.totals.clear();
        lib.errors.clear();
        lib.searched = true;
        let servers = self.config.library.enabled_servers();
        lib.searching = servers.len();
        let (generation, query, category, sort) = (lib.generation, lib.query.clone(), lib.category, lib.sort);
        for server in servers {
            let known = self.library.infos.contains_key(&server);
            let query = query.clone();
            self.spawn_request(move || {
                // (The server's info first, the first time: its key gets pinned.)
                if !known {
                    return Event::Info(server.clone(), client::info(&server));
                }
                Event::Listing(generation, server.clone(), client::list(&server, &query, category, sort, 0))
            });
        }
        self.refresh_installed();
    }

    /// Which scripts in the scripts folder came from (or went to) a server.
    fn refresh_installed(&mut self) {
        self.library.installed = self
            .available_scripts
            .iter()
            .filter_map(|path| Installed::read(path).map(|i| ((i.server, i.id), (path.clone(), i.version))))
            .collect();
    }

    /// Per frame: requests that finished.
    pub(super) fn poll_library(&mut self) {
        while let Ok(event) = self.library.events.1.try_recv() {
            match event {
                Event::Info(server, result) => self.library_info(server, result),
                Event::Listing(generation, server, result) => {
                    if generation != self.library.generation {
                        continue;
                    }
                    self.library.searching = self.library.searching.saturating_sub(1);
                    match result {
                        Ok(listing) => {
                            self.library.totals.insert(server.clone(), listing.total);
                            self.library
                                .results
                                .extend(listing.scripts.into_iter().map(|summary| Found { server: server.clone(), summary }));
                            let sort = self.library.sort;
                            self.library.results.sort_by(|a, b| match sort {
                                "top" => b.summary.encores.cmp(&a.summary.encores).then(b.summary.updated.cmp(&a.summary.updated)),
                                _ => b.summary.updated.cmp(&a.summary.updated),
                            });
                        }
                        Err(e) => self.library.errors.push((server, e)),
                    }
                }
                Event::Details(server, result) => match result {
                    Ok(details) => {
                        if self.library.selected.as_ref().is_some_and(|(s, id)| *s == server && *id == details.summary.id) {
                            self.library.details = Some((server, details));
                        }
                    }
                    Err(e) => self.status = format!("Couldn't get that script's details: {e}"),
                },
                Event::Downloaded(server, summary, result) => {
                    self.library.busy = false;
                    match result {
                        Ok(download) => self.install(&server, &summary, download),
                        Err(e) => self.status = format!("Couldn't install {}: {e}", summary.name),
                    }
                }
                Event::Published(server, script, name, result) => {
                    self.library.busy = false;
                    match result {
                        Ok(receipt) => {
                            let source = std::fs::read_to_string(&script).unwrap_or_default();
                            let installed = Installed {
                                server: server.clone(),
                                id: receipt.id.clone(),
                                version: receipt.version,
                                sha256: library::sha256_hex(source.as_bytes()),
                                name: name.clone(),
                            };
                            if let Err(e) = installed.write(&script) {
                                log::warn!("couldn't note where {} was published: {e}", script.display());
                            }
                            log::info!("published {} to {server} as {}", script.display(), receipt.id);
                            self.status = format!("Published \"{name}\" to {} ({}).", host(&server), receipt.id);
                            self.library.publish = None;
                            self.library_search();
                        }
                        Err(e) => {
                            if let Some(draft) = &mut self.library.publish {
                                draft.error = Some(e);
                            }
                        }
                    }
                }
            }
        }
    }

    /// A server's info came back: pin its key (or notice it changed), then
    /// run the search waiting on it.
    fn library_info(&mut self, server: String, result: Result<Info, String>) {
        let info = match result {
            Ok(info) => info,
            Err(e) => {
                self.library.searching = self.library.searching.saturating_sub(1);
                self.library.errors.push((server, e));
                return;
            }
        };
        match self.config.library.pinned.get(&server) {
            Some(pinned) if *pinned != info.key => {
                self.library.searching = self.library.searching.saturating_sub(1);
                self.library.key_changed.insert(server.clone(), info.key.clone());
                self.library.errors.push((
                    server,
                    "its key has changed since it was first used, so it isn't trusted (Preferences > Library)".into(),
                ));
                return;
            }
            Some(_) => {}
            None => {
                self.config.library.pinned.insert(server.clone(), info.key.clone());
                if let Err(e) = self.config.save() {
                    self.status = format!("Couldn't save preferences: {e}");
                }
            }
        }
        self.library.infos.insert(server.clone(), info);
        let (generation, query, category, sort) =
            (self.library.generation, self.library.query.clone(), self.library.category, self.library.sort);
        self.spawn_request(move || Event::Listing(generation, server.clone(), client::list(&server, &query, category, sort, 0)));
    }

    /// Write a downloaded script into the scripts folder (a new file, or
    /// over the copy installed before), note where it came from, and list
    /// it in the script picker.
    fn install(&mut self, server: &str, summary: &ScriptSummary, download: client::Download) {
        let key = (server.to_string(), summary.id.clone());
        let path = match self.library.installed.get(&key) {
            Some((path, _)) => path.clone(),
            None => {
                let stem = library::file_stem_for(&summary.name);
                let mut path = self.scripts_dir.join(format!("{stem}.lua"));
                let mut n = 2;
                while path.exists() {
                    path = self.scripts_dir.join(format!("{stem} ({n}).lua"));
                    n += 1;
                }
                path
            }
        };
        let installed = Installed {
            server: server.to_string(),
            id: summary.id.clone(),
            version: download.version,
            sha256: download.sha256.clone(),
            name: summary.name.clone(),
        };
        let written = std::fs::write(&path, &download.source).and_then(|()| installed.write(&path));
        match written {
            Ok(()) => {
                log::info!("installed {} from {server} as {}", summary.id, path.display());
                self.available_scripts = lua_visualizer::list_scripts(&self.scripts_dir);
                self.refresh_installed();
                self.status = format!("Installed \"{}\": pick it in the visualizer's script list.", summary.name);
            }
            Err(e) => self.status = format!("Couldn't save {}: {e}", path.display()),
        }
    }

    /// The Script Library tab.
    pub(super) fn library_ui(&mut self, ui: &mut egui::Ui) {
        let servers = self.config.library.enabled_servers();
        let mut search = false;
        ui.horizontal_wrapped(|ui| {
            let edit = ui.add(egui::TextEdit::singleline(&mut self.library.query).hint_text("Search scripts").desired_width(220.0));
            if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                search = true;
            }
            let category = self.library.category.map(library::category_title).unwrap_or("All kinds");
            egui::ComboBox::from_id_salt("library_category").selected_text(category).show_ui(ui, |ui| {
                search |= ui.selectable_value(&mut self.library.category, None, "All kinds").clicked();
                for c in library::CATEGORIES {
                    search |= ui.selectable_value(&mut self.library.category, Some(c), library::category_title(c)).clicked();
                }
            });
            let sort = SORTS.iter().find(|(s, _)| *s == self.library.sort).map(|(_, t)| *t).unwrap_or("Newest");
            egui::ComboBox::from_id_salt("library_sort").selected_text(sort).show_ui(ui, |ui| {
                for (s, title) in SORTS {
                    search |= ui.selectable_value(&mut self.library.sort, s, title).clicked();
                }
            });
            search |= ui.button("Search").clicked();
            if self.library.searching > 0 {
                ui.spinner();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let script = self.visualizer.script().path().map(PathBuf::from);
                if ui
                    .add_enabled(script.is_some() && !servers.is_empty(), egui::Button::new("Publish..."))
                    .on_hover_text("Share the script that's running in the visualizer")
                    .clicked()
                    && let Some(script) = script
                {
                    self.open_publish(script);
                }
            });
        });
        if servers.is_empty() {
            ui.weak("No servers in use: turn one on in Preferences > Library.");
            return;
        }
        if !self.library.searched || search {
            self.library_search();
        }
        for (server, error) in &self.library.errors {
            ui.colored_label(ui.visuals().warn_fg_color, format!("{}: {error}", host(server)));
        }
        ui.separator();

        let mut pick = None;
        let mut install = None;
        let mut use_script = None;
        ui.columns(2, |columns| {
            let ui = &mut columns[0];
            egui::ScrollArea::vertical().id_salt("library_results").auto_shrink([false, false]).show(ui, |ui| {
                if self.library.results.is_empty() && self.library.searching == 0 {
                    ui.weak(if self.library.query.trim().is_empty() { "Nothing here yet." } else { "Nothing found." });
                }
                for found in &self.library.results {
                    let selected = self
                        .library
                        .selected
                        .as_ref()
                        .is_some_and(|(s, id)| *s == found.server && *id == found.summary.id);
                    let s = &found.summary;
                    let mut text = egui::text::LayoutJob::default();
                    let style = ui.style();
                    egui::RichText::new(&s.name).strong().append_to(&mut text, style, egui::FontSelection::Default, egui::Align::LEFT);
                    egui::RichText::new(format!(
                        "\n{} by {}  ·  {} encore{}",
                        library::category_title(&s.category),
                        s.author_name,
                        s.encores,
                        if s.encores == 1 { "" } else { "s" }
                    ))
                    .weak()
                    .append_to(&mut text, style, egui::FontSelection::Default, egui::Align::LEFT);
                    let response = ui.selectable_label(selected, text);
                    let response = if servers.len() > 1 { response.on_hover_text(host(&found.server)) } else { response };
                    if response.clicked() {
                        pick = Some((found.server.clone(), s.id.clone()));
                    }
                }
                for (server, total) in &self.library.totals {
                    let shown = self.library.results.iter().filter(|f| f.server == *server).count() as u64;
                    if *total > shown {
                        ui.weak(format!("{}: the first {shown} of {total}; search to narrow it down", host(server)));
                    }
                }
            });

            let ui = &mut columns[1];
            egui::ScrollArea::vertical().id_salt("library_details").auto_shrink([false, false]).show(ui, |ui| {
                let Some((server, id)) = self.library.selected.clone() else {
                    ui.weak("Pick a script to see more.");
                    return;
                };
                let Some(found) = self.library.results.iter().find(|f| f.server == server && f.summary.id == id) else {
                    return;
                };
                let s = &found.summary;
                ui.heading(&s.name);
                let author = match &s.author_id {
                    Some(id) => format!("by {} (#{id})", s.author_name),
                    None => format!("by {} (anonymous)", s.author_name),
                };
                ui.label(author);
                ui.weak(format!("{} on {}", library::category_title(&s.category), host(&server)));
                if !s.tags.is_empty() {
                    ui.weak(s.tags.join(", "));
                }
                ui.add_space(6.0);
                let installed = self.library.installed.get(&(server.clone(), id.clone())).cloned();
                ui.horizontal(|ui| match &installed {
                    Some((path, version)) if *version >= s.version => {
                        ui.label(format!("Installed (version {version})"));
                        if ui.button("Use it").clicked() {
                            use_script = Some(path.clone());
                        }
                    }
                    Some((_, version)) => {
                        ui.label(format!("Version {version} installed; {} is out", s.version));
                    }
                    None => {
                        if ui.add_enabled(!self.library.busy, egui::Button::new("Install")).clicked() {
                            install = Some((server.clone(), s.clone()));
                        }
                    }
                });
                ui.add_space(6.0);
                let description = match &self.library.details {
                    Some((ds, d)) if *ds == server && d.summary.id == id => d.summary.description.clone(),
                    _ => s.description.clone(),
                };
                ui.label(description);
                if let Some((ds, d)) = &self.library.details
                    && *ds == server
                    && d.summary.id == id
                {
                    ui.add_space(6.0);
                    let versions = d.versions.len();
                    let made = d.versions.first().map(|v| v.app_version.clone()).unwrap_or_default();
                    ui.weak(format!(
                        "{versions} version{}; first published with synththing {made}",
                        if versions == 1 { "" } else { "s" }
                    ));
                }
            });
        });

        if let Some(selected) = pick {
            let (server, id) = selected.clone();
            self.library.selected = Some(selected);
            self.library.details = None;
            self.spawn_request(move || Event::Details(server.clone(), client::details(&server, &id)));
        }
        if let Some((server, summary)) = install {
            match self.config.library.pinned.get(&server).cloned() {
                Some(key) => {
                    self.library.busy = true;
                    self.status = format!("Installing \"{}\"...", summary.name);
                    self.spawn_request(move || {
                        let result = client::download(&server, &summary.id, None, &key);
                        Event::Downloaded(server, summary, result)
                    });
                }
                None => self.status = "That server's key isn't known yet: search again.".to_string(),
            }
        }
        if let Some(path) = use_script
            && let Some(idx) = self.available_scripts.iter().position(|p| *p == path)
        {
            self.load_script(idx);
            self.set_open(crate::layout::Section::Visualizer, true);
        }
    }

    fn open_publish(&mut self, script: PathBuf) {
        self.flush_editor();
        let source = std::fs::read_to_string(&script).unwrap_or_default();
        let servers = self.config.library.enabled_servers();
        let published = Installed::read(&script);
        let category = script
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(lua_visualizer::bundled_category)
            .and_then(|c| match c {
                "games" => Some("game"),
                "examples" => Some("example"),
                _ => None,
            })
            .unwrap_or("visualizer");
        self.library.publish = Some(PublishDraft {
            name: published.map(|p| p.name).unwrap_or_else(|| lua_visualizer::display_name(&script)),
            description: library::leading_comment(&source),
            category,
            tags: String::new(),
            author_name: self.config.library.author_name.clone(),
            server: servers.first().cloned().unwrap_or_default(),
            error: None,
            script,
        });
    }

    /// Publish...: what to share the script as, and where.
    pub(super) fn publish_ui(&mut self, ctx: &egui::Context) {
        let Some(mut draft) = self.library.publish.take() else { return };
        let servers = self.config.library.enabled_servers();
        let mut send = false;
        let mut close = false;
        let busy = self.library.busy;
        let response = egui::Modal::new(egui::Id::new("library_publish")).show(ctx, |ui| {
            ui.set_width(480.0);
            ui.heading("Publish a script");
            ui.weak(draft.script.display().to_string());
            ui.add_space(6.0);
            egui::Grid::new("publish_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Name");
                ui.add(egui::TextEdit::singleline(&mut draft.name).desired_width(f32::INFINITY));
                ui.end_row();
                ui.label("Kind");
                egui::ComboBox::from_id_salt("publish_category")
                    .selected_text(library::category_title(draft.category))
                    .show_ui(ui, |ui| {
                        for c in library::CATEGORIES {
                            ui.selectable_value(&mut draft.category, c, library::category_title(c));
                        }
                    });
                ui.end_row();
                ui.label("Tags");
                ui.add(egui::TextEdit::singleline(&mut draft.tags).hint_text("chill, retro").desired_width(f32::INFINITY));
                ui.end_row();
                ui.label("Post as");
                ui.add(egui::TextEdit::singleline(&mut draft.author_name).hint_text("a name").desired_width(f32::INFINITY));
                ui.end_row();
                ui.label("Server");
                egui::ComboBox::from_id_salt("publish_server").selected_text(host(&draft.server)).show_ui(ui, |ui| {
                    for s in &servers {
                        ui.selectable_value(&mut draft.server, s.clone(), host(s));
                    }
                });
                ui.end_row();
            });
            ui.label("Description");
            ui.add(egui::TextEdit::multiline(&mut draft.description).desired_rows(5).desired_width(f32::INFINITY));
            if let Some(info) = self.library.infos.get(&draft.server) {
                ui.weak(format!("Shared under {}. {}", info.license.replace('-', " "), info.rules));
            }
            ui.weak(
                "Anonymous for now: once it's up, it can't be changed or taken down from here (that comes with \
                 identities, next).",
            );
            if let Some(error) = &draft.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("Cancel").clicked() {
                    close = true;
                }
                if ui.add_enabled(!busy, egui::Button::new(if busy { "Publishing..." } else { "Publish" })).clicked() {
                    send = true;
                }
            });
        });
        if close || (response.should_close() && !busy) {
            return;
        }
        if send {
            let source = std::fs::read_to_string(&draft.script).unwrap_or_default();
            let upload = Upload {
                name: draft.name.trim().to_string(),
                description: draft.description.trim().to_string(),
                category: draft.category.to_string(),
                tags: library::parse_tags(&draft.tags),
                author_name: draft.author_name.trim().to_string(),
                source,
                app_version: env!("CARGO_PKG_VERSION").to_string(),
            };
            match library::check_upload(&upload) {
                Err(problem) => draft.error = Some(format!("Can't publish it: {problem}.")),
                Ok(()) => {
                    draft.error = None;
                    self.library.busy = true;
                    if self.config.library.author_name != upload.author_name {
                        self.config.library.author_name = upload.author_name.clone();
                        let _ = self.config.save();
                    }
                    let (server, script, name) = (draft.server.clone(), draft.script.clone(), upload.name.clone());
                    self.spawn_request(move || {
                        let result = client::upload(&server, &upload);
                        Event::Published(server, script, name, result)
                    });
                }
            }
        }
        self.library.publish = Some(draft);
    }

    /// Preferences > Library: the servers in use.
    pub(super) fn library_preferences_ui(&mut self, ui: &mut egui::Ui) {
        let before = (self.config.library.official, self.config.library.author_name.clone());
        let mut changed = false;
        with_info(
            ui,
            "The official script library. It can be turned off, but not removed.",
            |ui| ui.checkbox(&mut self.config.library.official, format!("Official server ({})", host(library::OFFICIAL_URL))),
        );
        let mut remove = None;
        for (i, server) in self.config.library.servers.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                changed |= ui.checkbox(&mut server.enabled, host(&server.url)).changed();
                if ui.small_button("Remove").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            let server = self.config.library.servers.remove(i);
            self.config.library.pinned.remove(&server.url);
            changed = true;
        }
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.library.new_server)
                    .hint_text("another server's address")
                    .desired_width(220.0),
            );
            let url = client::normalize_url(&self.library.new_server);
            let known = url == library::OFFICIAL_URL || self.config.library.servers.iter().any(|s| s.url == url);
            if ui.add_enabled(!self.library.new_server.trim().is_empty() && !known, egui::Button::new("Add")).clicked() {
                self.config.library.servers.push(LibraryServer { url, enabled: true });
                self.library.new_server.clear();
                changed = true;
            }
            info_icon(ui).on_hover_text(
                "Anyone can run a library server (synththing serve). Its address, e.g. scripts.example.org \
                 (https is assumed; write http:// for one on your own network).",
            );
        });

        let mut trust = None;
        for (server, key) in &self.library.key_changed {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("{}'s key isn't the one it had when first used.", host(server)),
            );
            ui.horizontal(|ui| {
                ui.weak("If its owner says they changed it, trust the new one:");
                if ui.small_button("Trust the new key").clicked() {
                    trust = Some((server.clone(), key.clone()));
                }
            });
        }
        if let Some((server, key)) = trust {
            self.library.key_changed.remove(&server);
            self.config.library.pinned.insert(server, key);
            changed = true;
        }
        ui.add_space(6.0);
        with_info(ui, "The name your uploads show. Each upload can use a different one.", |ui| {
            ui.horizontal(|ui| {
                ui.label("Post as");
                ui.text_edit_singleline(&mut self.config.library.author_name);
            })
            .response
        });
        changed |= before != (self.config.library.official, self.config.library.author_name.clone());
        if changed {
            self.library.searched = false;
            self.library.infos.retain(|server, _| self.config.library.enabled_servers().contains(server));
            if let Err(e) = self.config.save() {
                self.status = format!("Couldn't save preferences: {e}");
            }
        }
    }
}
