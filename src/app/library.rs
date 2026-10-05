//! The Script Library tab (docs/SERVER.md): scripts shared on library
//! servers, searched across every server in use, installed into the
//! scripts folder (with a `.source.json` beside each, saying where it came
//! from), and the current script published (Publish...). Servers and the
//! user's identity are set up in Preferences > Library.
//!
//! Publishing is signed with the user's identity (made the first time
//! it's needed) unless they choose to post anonymously; a signed script
//! can be given new versions (by publishing it again with its slug) and
//! deleted, by its author only.
//!
//! The visualizer's script row has one button for the selected script:
//! Publish... (yours, or never published), or Update (installed from a
//! server, someone else's): it fetches the newest version and shows what
//! the author changed, and separately what the user changed in their copy
//! (which updating replaces). Both are measured from the untouched copy
//! kept when a script is installed, updated or published
//! (`library::save_original`), which Restore original also puts back.
//! When the user's changes and the author's don't touch the same lines,
//! Update can keep both (`library::merge`).
//!
//! Requests run on threads of their own and come back as `Event`s, picked
//! up each frame (`poll_library`). A server's key is pinned the first time
//! it's seen; one whose key changes isn't used until the user trusts it
//! again.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui;

use super::{info_icon, with_info, App};
use crate::config::LibraryServer;
use crate::library::identity::Identity;
use crate::library::{
    self, client, Change, Info, Installed, Listing, Receipt, ScriptDetails, ScriptSummary, Upload, UserInfo,
};
use crate::lua_visualizer;

/// Sort orders, as the API names them and as shown.
const SORTS: [(&str, &str); 2] = [("new", "Newest"), ("top", "Most encores")];

enum Event {
    Info(String, Result<Info, String>),
    /// A search's results from one server (the search's generation first).
    Listing(u64, String, Result<Listing, String>),
    Details(String, Result<ScriptDetails, String>),
    User(String, Result<UserInfo, String>),
    Downloaded(String, ScriptSummary, Result<client::Download, String>),
    /// An upload finished: the server, the server key used, the local
    /// script, the upload, and the result.
    Published(String, String, PathBuf, Box<Upload>, Result<Receipt, String>),
    Deleted(String, String, Result<(), String>),
    /// Whether the user has a script with a slug on a server.
    SlugChecked(String, String, Result<Option<ScriptSummary>, String>),
    /// An update check: the script, where it came from, the server's key,
    /// and the newer version if there is one.
    /// (With where the script came from, as worked out on the way: a
    /// bundled one learns its ID and version on the server.)
    UpdateChecked(PathBuf, Box<Installed>, Result<Option<Box<Newer>>, String>),
}

/// A newer version found by an update check, and the installed version's
/// untouched source (if it could be had) to compare against.
struct Newer {
    summary: ScriptSummary,
    download: client::Download,
    original: Option<String>,
}

/// A newer version of an installed script, waiting to be confirmed.
struct UpdateDraft {
    script: PathBuf,
    installed: Installed,
    summary: ScriptSummary,
    download: client::Download,
    /// The local copy was changed since it was installed.
    edited: bool,
    /// What the author changed (installed version to new), and what the
    /// user changed (installed version to their copy, if they did); each
    /// `None` if it can't be shown (too big, or the installed version
    /// couldn't be had: then `fallback` is the copy against the new one).
    whats_new: Option<Vec<(Change, String)>>,
    yours: Option<Vec<(Change, String)>>,
    fallback: Option<Vec<(Change, String)>>,
    /// The new version with the user's changes kept, when they don't
    /// overlap the author's.
    merged: Option<String>,
}

/// Official scripts' author, and what hovering it says.
const OFFICIAL_GOLD: egui::Color32 = egui::Color32::from_rgb(230, 180, 60);
const OFFICIAL_HOVER: &str = "Official: one of the scripts that come with synththing, published by its makers";

/// How long typing pauses before a slug is looked up.
const SLUG_CHECK_AFTER: Duration = Duration::from_millis(400);

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
    slug: String,
    /// Posted without a signature: can't be changed or deleted later.
    anonymous: bool,
    /// Published as a new script, even though it's one of yours already.
    as_new: bool,
    /// Where the script was published before, if it was.
    published: Option<Installed>,
    error: Option<String>,
    /// When the slug was last edited (looked up once typing pauses), and
    /// what the lookup found: (server, slug, the script using it).
    slug_edited: Option<Instant>,
    slug_asked: Option<(String, String)>,
    slug_found: Option<(String, String, Option<ScriptSummary>)>,
}

pub struct LibraryState {
    query: String,
    category: Option<&'static str>,
    sort: &'static str,
    /// Only one person's uploads: the server and their ID.
    author: Option<(String, String)>,
    author_info: Option<UserInfo>,
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
    /// A download, upload or delete is under way.
    busy: bool,
    /// Delete was clicked once (for this script); the next click confirms.
    confirm_delete: Option<(String, String)>,
    publish: Option<PublishDraft>,
    /// Installed scripts by (server, id), and by (server, author ID, slug)
    /// (bundled ones don't know their ID until they're first checked):
    /// their file and version.
    installed: HashMap<(String, String), (PathBuf, u32)>,
    installed_by_slug: HashMap<(String, String, String), (PathBuf, u32)>,
    /// The user's identity, once there is one.
    identity: Option<Identity>,
    /// Preferences > Library: the address typed in, the passphrase for
    /// exporting or importing, and whether replacing the identity was
    /// clicked once (import confirms on the second).
    new_server: String,
    passphrase: String,
    confirm_import: bool,
    identity_message: Option<String>,
    /// A newer version waiting to be confirmed (the Update window).
    update: Option<UpdateDraft>,
    /// The selected script's `.source.json` and its current SHA-256, read a
    /// couple of times a second rather than every frame.
    sidecar: Option<(PathBuf, Option<Installed>, String, Instant)>,
    /// Restore original was clicked once (for this script).
    confirm_restore: Option<PathBuf>,
    events: (Sender<Event>, Receiver<Event>),
}

impl LibraryState {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            category: None,
            sort: "new",
            author: None,
            author_info: None,
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
            confirm_delete: None,
            publish: None,
            installed: HashMap::new(),
            installed_by_slug: HashMap::new(),
            identity: Identity::load(),
            new_server: String::new(),
            passphrase: String::new(),
            confirm_import: false,
            identity_message: None,
            update: None,
            sidecar: None,
            confirm_restore: None,
            events: mpsc::channel(),
        }
    }

    pub fn publish_open(&self) -> bool {
        self.publish.is_some() || self.update.is_some()
    }

    fn my_id(&self) -> Option<String> {
        self.identity.as_ref().map(Identity::id)
    }
}

/// A diff, its unchanged stretches folded down to a little context.
fn diff_ui(ui: &mut egui::Ui, diff: &[(Change, String)]) {
    const CONTEXT: usize = 2;
    let near_change = |i: usize| {
        let lo = i.saturating_sub(CONTEXT);
        let hi = (i + CONTEXT + 1).min(diff.len());
        diff[lo..hi].iter().any(|(c, _)| *c != Change::Same)
    };
    if diff.iter().all(|(c, _)| *c == Change::Same) {
        ui.weak("(No changes in the text.)");
        return;
    }
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let mut skipped = 0;
    for (i, (change, line)) in diff.iter().enumerate() {
        if *change == Change::Same && !near_change(i) {
            skipped += 1;
            continue;
        }
        if skipped > 0 {
            ui.weak(format!("... {skipped} unchanged line{}", if skipped == 1 { "" } else { "s" }));
            skipped = 0;
        }
        let (mark, color) = match change {
            Change::Same => (" ", ui.visuals().weak_text_color()),
            Change::Removed => ("-", egui::Color32::from_rgb(220, 90, 90)),
            Change::Added => ("+", egui::Color32::from_rgb(90, 190, 110)),
        };
        ui.label(egui::RichText::new(format!("{mark} {line}")).font(font.clone()).color(color));
    }
    if skipped > 0 {
        ui.weak(format!("... {skipped} unchanged line{}", if skipped == 1 { "" } else { "s" }));
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

    /// The search as typed and picked, for one server (an author's uploads
    /// are only looked for on their server).
    fn library_query(&self, server: &str) -> Option<client::Search> {
        let lib = &self.library;
        let author = match &lib.author {
            Some((on, id)) if on == server => Some(id.clone()),
            Some(_) => return None,
            None => None,
        };
        Some(client::Search { query: lib.query.clone(), category: lib.category, sort: lib.sort, author, page: 0 })
    }

    /// Search every server in use (with what's typed and picked).
    fn library_search(&mut self) {
        let servers: Vec<(String, client::Search)> = self
            .config
            .library
            .enabled_servers()
            .into_iter()
            .filter_map(|s| self.library_query(&s).map(|q| (s, q)))
            .collect();
        let lib = &mut self.library;
        lib.generation += 1;
        lib.results.clear();
        lib.totals.clear();
        lib.errors.clear();
        lib.searched = true;
        lib.searching = servers.len();
        let generation = lib.generation;
        for (server, search) in servers {
            let known = self.library.infos.contains_key(&server);
            self.spawn_request(move || {
                // (The server's info first, the first time: its key gets pinned.)
                if !known {
                    return Event::Info(server.clone(), client::info(&server));
                }
                Event::Listing(generation, server.clone(), client::list(&server, &search))
            });
        }
        self.refresh_installed();
    }

    /// Show only `id`'s uploads on `server` (or everyone's again).
    fn library_show_author(&mut self, author: Option<(String, String)>) {
        self.library.author_info = None;
        if let Some((server, id)) = &author {
            let (server, id) = (server.clone(), id.clone());
            self.spawn_request(move || Event::User(server.clone(), client::user(&server, &id)));
        }
        self.library.author = author;
        self.library_search();
    }

    /// Which scripts in the scripts folder came from (or went to) a server.
    fn refresh_installed(&mut self) {
        let all: Vec<(PathBuf, Installed)> =
            self.available_scripts.iter().filter_map(|p| Installed::read(p).map(|i| (p.clone(), i))).collect();
        self.library.installed = all
            .iter()
            .filter(|(_, i)| !i.id.is_empty())
            .map(|(p, i)| ((library::resolve_server(&i.server), i.id.clone()), (p.clone(), i.version)))
            .collect();
        self.library.installed_by_slug = all
            .iter()
            .filter_map(|(p, i)| {
                Some(((library::resolve_server(&i.server), i.author_id.clone()?, i.slug.clone()?), (p.clone(), i.version)))
            })
            .collect();
    }

    /// The installed copy of a script found on `server`, if there is one.
    fn installed_copy(&self, server: &str, script: &ScriptSummary) -> Option<(PathBuf, u32)> {
        let lib = &self.library;
        lib.installed.get(&(server.to_string(), script.id.clone())).cloned().or_else(|| {
            let key = (server.to_string(), script.author_id.clone()?, script.slug.clone()?);
            lib.installed_by_slug.get(&key).cloned()
        })
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
                Event::User(server, result) => {
                    if self.library.author.as_ref().is_some_and(|(s, _)| *s == server) {
                        match result {
                            Ok(user) => self.library.author_info = Some(user),
                            Err(e) => self.status = format!("Couldn't look them up: {e}"),
                        }
                    }
                }
                Event::Downloaded(server, summary, result) => {
                    self.library.busy = false;
                    match result {
                        Ok(download) => self.install(&server, &summary, download),
                        Err(e) => self.status = format!("Couldn't install {}: {e}", summary.name),
                    }
                }
                Event::Published(server, key, script, upload, result) => self.published(server, key, script, *upload, result),
                Event::SlugChecked(server, slug, result) => {
                    if let Some(draft) = &mut self.library.publish
                        && let Ok(found) = result
                    {
                        draft.slug_found = Some((server, slug, found));
                    }
                }
                Event::UpdateChecked(script, installed, result) => {
                    self.library.busy = false;
                    let installed = *installed;
                    // What was worked out about it (a bundled script's ID).
                    if Installed::read(&script).is_some_and(|before| before != installed) {
                        let _ = installed.write(&script);
                        self.library.sidecar = None;
                        self.refresh_installed();
                    }
                    match result.map(|found| found.map(|newer| *newer)) {
                        Ok(None) if installed.version == 0 => {
                            self.status = format!("\"{}\" is up to date.", installed.name);
                        }
                        Ok(None) => {
                            self.status = format!("\"{}\" is up to date (version {}).", installed.name, installed.version);
                        }
                        Ok(Some(Newer { summary, download, original })) => {
                            let local = std::fs::read_to_string(&script).unwrap_or_default();
                            let edited = library::sha256_hex(local.as_bytes()) != installed.sha256;
                            // (Unedited, the copy is the installed version.)
                            let original = original.or_else(|| (!edited).then(|| local.clone()));
                            let merged = match (&original, edited) {
                                (Some(original), true) => library::merge(original, &local, &download.source),
                                _ => None,
                            };
                            let (whats_new, yours, fallback) = match &original {
                                Some(original) => (
                                    library::line_diff(original, &download.source),
                                    edited.then(|| library::line_diff(original, &local)).flatten(),
                                    None,
                                ),
                                None => (None, None, library::line_diff(&local, &download.source)),
                            };
                            self.library.update = Some(UpdateDraft {
                                script,
                                installed,
                                summary,
                                download,
                                edited,
                                whats_new,
                                yours,
                                fallback,
                                merged,
                            });
                        }
                        Err(e) => self.status = format!("Couldn't check for an update: {e}"),
                    }
                }
                Event::Deleted(server, id, result) => {
                    self.library.busy = false;
                    match result {
                        Ok(()) => {
                            log::info!("deleted {id} from {server}");
                            self.status = format!("Deleted it from {}.", host(&server));
                            self.library.selected = None;
                            self.library.details = None;
                            self.library_search();
                        }
                        Err(e) => self.status = format!("Couldn't delete it: {e}"),
                    }
                }
            }
        }
    }

    /// An upload finished: on success, note where it went beside the
    /// script (so publishing it again makes a new version).
    fn published(&mut self, server: String, key: String, script: PathBuf, upload: Upload, result: Result<Receipt, String>) {
        self.library.busy = false;
        if !self.config.library.pinned.contains_key(&server) {
            self.config.library.pinned.insert(server.clone(), key);
            let _ = self.config.save();
        }
        let receipt = match result {
            Ok(receipt) => receipt,
            Err(e) => {
                if let Some(draft) = &mut self.library.publish {
                    draft.error = Some(e);
                }
                return;
            }
        };
        let signed = upload.slug.is_some();
        let installed = Installed {
            server: server.clone(),
            id: receipt.id.clone(),
            version: receipt.version,
            sha256: receipt.sha256.clone(),
            name: upload.name.clone(),
            slug: upload.slug.clone(),
            author_id: signed.then(|| self.library.my_id()).flatten(),
            receipt: Some(receipt.clone()),
        };
        if let Err(e) = installed.write(&script) {
            log::warn!("couldn't note where {} was published: {e}", script.display());
        }
        library::save_original(&upload.source);
        log::info!("published {} to {server} as {} v{}", script.display(), receipt.id, receipt.version);
        self.status = if receipt.version > 1 {
            format!("Published version {} of \"{}\" to {}.", receipt.version, upload.name, host(&server))
        } else {
            format!("Published \"{}\" to {} ({}).", upload.name, host(&server), receipt.id)
        };
        self.library.publish = None;
        self.library_search();
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
        let generation = self.library.generation;
        let Some(search) = self.library_query(&server) else {
            self.library.searching = self.library.searching.saturating_sub(1);
            return;
        };
        self.spawn_request(move || Event::Listing(generation, server.clone(), client::list(&server, &search)));
    }

    /// Write a downloaded script into the scripts folder (a new file, or
    /// over the copy installed before), note where it came from, and list
    /// it in the script picker.
    fn install(&mut self, server: &str, summary: &ScriptSummary, download: client::Download) {
        let path = match self.installed_copy(server, summary) {
            Some((path, _)) => path,
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
            slug: summary.slug.clone(),
            author_id: summary.author_id.clone(),
            receipt: None,
        };
        let written = std::fs::write(&path, &download.source).and_then(|()| installed.write(&path));
        match written {
            Ok(()) => {
                library::save_original(&download.source);
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
        });
        if servers.is_empty() {
            ui.weak("No servers in use: turn one on in Preferences > Library.");
            return;
        }
        if !self.library.searched || search {
            self.library_search();
        }
        if let Some((server, id)) = self.library.author.clone() {
            let mut everyone = false;
            ui.horizontal_wrapped(|ui| {
                let info = self.library.author_info.as_ref();
                let names = info.map(|u| u.names.join(", ")).unwrap_or_default();
                ui.strong(format!("Uploads by #{id}"));
                if !names.is_empty() {
                    ui.label(format!("(posts as {names})"));
                }
                if let Some(info) = info {
                    ui.weak(format!(
                        "{} script{}, {} encore{} on {}",
                        info.scripts,
                        if info.scripts == 1 { "" } else { "s" },
                        info.encores,
                        if info.encores == 1 { "" } else { "s" },
                        host(&server)
                    ));
                }
                everyone = ui.small_button("Show everyone's").clicked();
            });
            if everyone {
                self.library_show_author(None);
            }
        }
        for (server, error) in &self.library.errors {
            ui.colored_label(ui.visuals().warn_fg_color, format!("{}: {error}", host(server)));
        }
        ui.separator();

        let mut pick = None;
        let mut install = None;
        let mut update = None;
        let mut use_script = None;
        let mut show_author = None;
        let mut delete = None;
        let my_id = self.library.my_id();
        let official_id = library::official_id();
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
                    let official = s.author_id.as_deref() == Some(official_id.as_str());
                    let weak = |text: String, job: &mut egui::text::LayoutJob| {
                        egui::RichText::new(text).weak().append_to(job, style, egui::FontSelection::Default, egui::Align::LEFT);
                    };
                    weak(format!("\n{} by ", library::category_title(&s.category)), &mut text);
                    // An official script's author in gold.
                    let author = egui::RichText::new(&s.author_name);
                    let author = if official { author.color(OFFICIAL_GOLD) } else { author.weak() };
                    author.append_to(&mut text, style, egui::FontSelection::Default, egui::Align::LEFT);
                    weak(format!("  ·  {} encore{}", s.encores, if s.encores == 1 { "" } else { "s" }), &mut text);
                    let response = ui.selectable_label(selected, text);
                    let response = if official { response.on_hover_text(OFFICIAL_HOVER) } else { response };
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
                ui.horizontal_wrapped(|ui| {
                    ui.label("by");
                    if s.author_id.as_deref() == Some(official_id.as_str()) {
                        ui.label(egui::RichText::new(&s.author_name).strong().color(OFFICIAL_GOLD)).on_hover_text(OFFICIAL_HOVER);
                    } else {
                        ui.label(&s.author_name);
                    }
                    match &s.author_id {
                        Some(author) => {
                            let label = if my_id.as_deref() == Some(author) { format!("#{author} (you)") } else { format!("#{author}") };
                            if ui.link(label).on_hover_text("Everything they've posted on this server").clicked() {
                                show_author = Some((server.clone(), author.clone()));
                            }
                        }
                        None => {
                            ui.weak("(anonymous)");
                        }
                    }
                });
                ui.weak(format!("{} on {}", library::category_title(&s.category), host(&server)));
                if !s.tags.is_empty() {
                    ui.weak(s.tags.join(", "));
                }
                ui.add_space(6.0);
                let installed = self.installed_copy(&server, s);
                ui.horizontal(|ui| {
                    match &installed {
                        Some((path, version)) if *version >= s.version => {
                            ui.label(format!("Installed (version {version})"));
                            if ui.button("Use it").clicked() {
                                use_script = Some(path.clone());
                            }
                        }
                        Some((path, version)) => {
                            ui.label(format!("Version {version} installed; {} is out", s.version));
                            if ui.add_enabled(!self.library.busy, egui::Button::new("Update")).clicked() {
                                update = Some(path.clone());
                            }
                        }
                        None => {
                            if ui.add_enabled(!self.library.busy, egui::Button::new("Install")).clicked() {
                                install = Some((server.clone(), s.clone()));
                            }
                        }
                    }
                    if s.author_id.is_some() && s.author_id == my_id {
                        let confirming = self.library.confirm_delete.as_ref() == Some(&(server.clone(), id.clone()));
                        let label = if confirming { "Click again to delete it" } else { "Delete" };
                        if ui
                            .add_enabled(!self.library.busy, egui::Button::new(label))
                            .on_hover_text("Take it off the server, every version (your copy stays)")
                            .clicked()
                        {
                            if confirming {
                                delete = Some((server.clone(), id.clone()));
                            } else {
                                self.library.confirm_delete = Some((server.clone(), id.clone()));
                            }
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
                    let app = env!("CARGO_PKG_VERSION");
                    if let Some(newest) = d.newest()
                        && !library::runs_on(&newest.min_app_version, app)
                    {
                        let note = match d.newest_for(app) {
                            Some(v) => format!(
                                "Version {} needs synththing {}; this app gets version {}.",
                                newest.version, newest.min_app_version, v.version
                            ),
                            None => format!("It needs synththing {} or newer.", newest.min_app_version),
                        };
                        ui.colored_label(ui.visuals().warn_fg_color, note);
                    }
                }
            });
        });

        if let Some(selected) = pick {
            let (server, id) = selected.clone();
            if self.library.selected.as_ref() != Some(&selected) {
                self.library.confirm_delete = None;
            }
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
                        // The newest version this app can run.
                        let result = client::details(&server, &summary.id).and_then(|details| {
                            let Some(version) = details.newest_for(env!("CARGO_PKG_VERSION")) else {
                                let needs = details.newest().map(|v| v.min_app_version.clone()).unwrap_or_default();
                                return Err(format!("it needs synththing {needs} or newer"));
                            };
                            client::download(&server, &summary.id, Some(version.version), &key)
                        });
                        Event::Downloaded(server, summary, result)
                    });
                }
                None => self.status = "That server's key isn't known yet: search again.".to_string(),
            }
        }
        if let Some((server, id)) = delete {
            self.library.confirm_delete = None;
            match (self.config.library.pinned.get(&server).cloned(), Identity::load()) {
                (Some(key), Some(identity)) => {
                    self.library.busy = true;
                    self.spawn_request(move || {
                        let result = client::delete(&server, &key, &id, &identity);
                        Event::Deleted(server, id, result)
                    });
                }
                _ => self.status = "Couldn't delete it: the server or your identity isn't known.".to_string(),
            }
        }
        if let Some(script) = update {
            self.start_update(script);
        }
        if let Some(author) = show_author {
            self.library_show_author(Some(author));
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
        let name = published.as_ref().map(|p| p.name.clone()).unwrap_or_else(|| lua_visualizer::display_name(&script));
        // (Back to the server it went to before, if that's still in use.)
        let server = published
            .as_ref()
            .map(|p| p.server.clone())
            .filter(|s| servers.contains(s))
            .or_else(|| servers.first().cloned())
            .unwrap_or_default();
        self.library.publish = Some(PublishDraft {
            slug: published.as_ref().and_then(|p| p.slug.clone()).unwrap_or_else(|| library::slugify(&name)),
            name,
            description: library::leading_comment(&source),
            category,
            tags: String::new(),
            author_name: self.config.library.author_name.clone(),
            server,
            anonymous: false,
            as_new: false,
            published,
            error: None,
            // (Looked up straight away.)
            slug_edited: Some(Instant::now() - SLUG_CHECK_AFTER),
            slug_asked: None,
            slug_found: None,
            script,
        });
    }

    /// The selected script's share button: Publish... for one of yours
    /// (or one never published), Update for one installed from a server.
    pub(super) fn script_share_button(&mut self, ui: &mut egui::Ui) {
        let Some(path) = self.visualizer.script().path().map(PathBuf::from) else { return };
        let fresh =
            self.library.sidecar.as_ref().is_some_and(|(p, _, _, at)| *p == path && at.elapsed() < Duration::from_secs(2));
        if !fresh {
            let installed = Installed::read(&path);
            let sha = installed
                .as_ref()
                .and_then(|_| std::fs::read(&path).ok())
                .map(|bytes| library::sha256_hex(&bytes))
                .unwrap_or_default();
            self.library.sidecar = Some((path.clone(), installed, sha, Instant::now()));
        }
        let installed = self.library.sidecar.as_ref().and_then(|(_, i, _, _)| i.clone());
        let current = self.library.sidecar.as_ref().map(|(_, _, sha, _)| sha.clone()).unwrap_or_default();
        // Changed since it was installed (or published), with the original
        // kept to go back to.
        if let Some(installed) = &installed
            && installed.sha256 != current
            && let Some(original) = library::read_original(&installed.sha256)
        {
            let confirming = self.library.confirm_restore.as_ref() == Some(&path);
            let label = if confirming { "Click again to restore" } else { "Restore original" };
            if ui
                .button(label)
                .on_hover_text(format!(
                    "Undo your changes: back to version {} as it came from {}",
                    installed.version,
                    host(&installed.server)
                ))
                .clicked()
            {
                if confirming {
                    self.library.confirm_restore = None;
                    self.restore_original(&path, &original);
                } else {
                    self.library.confirm_restore = Some(path.clone());
                }
            }
        }
        let my_id = self.library.my_id();
        // Yours: signed by you, or published from here (anonymously).
        let yours = installed.as_ref().is_some_and(|i| i.receipt.is_some() || (i.author_id.is_some() && i.author_id == my_id));
        match installed {
            Some(installed) if !yours => {
                if ui
                    .add_enabled(!self.library.busy, egui::Button::new("Update"))
                    .on_hover_text(format!(
                        "Look for a newer version of \"{}\" on {} (you have version {})",
                        installed.name,
                        host(&installed.server),
                        installed.version
                    ))
                    .clicked()
                {
                    self.start_update(path);
                }
            }
            _ => {
                let servers = !self.config.library.enabled_servers().is_empty();
                if ui
                    .add_enabled(servers, egui::Button::new("Publish..."))
                    .on_hover_text("Share this script in the Script Library")
                    .on_disabled_hover_text("Turn on a library server first (Preferences > Library)")
                    .clicked()
                {
                    self.open_publish(path);
                }
            }
        }
    }

    fn restore_original(&mut self, script: &std::path::Path, original: &str) {
        match std::fs::write(script, original) {
            Ok(()) => {
                log::info!("restored {} to its original", script.display());
                self.status = format!("Restored {} as it came.", lua_visualizer::display_name(script));
                self.library.sidecar = None;
                if self.visualizer.script().path() == Some(script)
                    && let Some(idx) = self.available_scripts.iter().position(|p| p == script)
                {
                    self.load_script(idx);
                }
            }
            Err(e) => self.status = format!("Couldn't restore it: {e}"),
        }
    }

    /// Look for a newer version of an installed script; if there is one,
    /// it's downloaded and shown for confirming (`update_ui`).
    fn start_update(&mut self, script: PathBuf) {
        let Some(installed) = Installed::read(&script) else {
            self.status = "That script didn't come from a library server.".to_string();
            return;
        };
        self.flush_editor();
        let pinned = self.config.library.pinned.get(&library::resolve_server(&installed.server)).cloned();
        self.library.busy = true;
        self.status = format!("Looking for a newer \"{}\"...", installed.name);
        self.spawn_request(move || {
            let mut installed = installed;
            let server = library::resolve_server(&installed.server);
            let result = (|| {
                let key = match pinned {
                    Some(key) => key,
                    None => client::info(&server)?.key,
                };
                // A bundled script: found by its slug, its version by its
                // contents (none matching: it's newer than what's
                // published, or older, by the app it came with).
                if installed.id.is_empty() {
                    let (Some(author), Some(slug)) = (installed.author_id.clone(), installed.slug.clone()) else {
                        return Err("it doesn't say what it is on the server".to_string());
                    };
                    match client::by_slug(&server, &author, &slug)? {
                        Some(found) => installed.id = found.id,
                        None => return Ok(None), // (not published yet)
                    }
                }
                let details = client::details(&server, &installed.id)?;
                let app = env!("CARGO_PKG_VERSION");
                if installed.version == 0 {
                    installed.version = details.versions.iter().find(|v| v.sha256 == installed.sha256).map_or(0, |v| v.version);
                }
                let offered: Vec<&library::VersionInfo> = details
                    .versions
                    .iter()
                    .filter(|v| library::runs_on(&v.min_app_version, app))
                    .filter(|v| installed.version > 0 || !library::runs_on(&v.app_version, app) || v.app_version == app)
                    .collect();
                // The newest version this app can run.
                let newest = offered.iter().map(|v| v.version).max().unwrap_or(0);
                if newest <= installed.version {
                    return Ok(None);
                }
                let download = client::download(&server, &installed.id, Some(newest), &key)?;
                // The installed version as it came: kept locally, or else
                // the server still has it.
                let original = library::read_original(&installed.sha256).or_else(|| {
                    let old = client::download(&server, &installed.id, Some(installed.version), &key).ok()?;
                    (old.sha256 == installed.sha256).then(|| {
                        library::save_original(&old.source);
                        old.source
                    })
                });
                Ok(Some(Box::new(Newer { summary: details.summary, download, original })))
            })();
            Event::UpdateChecked(script, Box::new(installed), result)
        });
    }

    /// The Update window: what a newer version changes, and a warning if
    /// the local copy was edited since it was installed.
    pub(super) fn update_ui(&mut self, ctx: &egui::Context) {
        let Some(draft) = &self.library.update else { return };
        let mut apply = false;
        let mut keep = false;
        let mut close = false;
        let response = egui::Modal::new(egui::Id::new("library_update")).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.heading(format!("Update \"{}\"", draft.summary.name));
            ui.label(format!(
                "Version {} is out on {}; you have version {}.",
                draft.summary.version,
                host(&draft.installed.server),
                draft.installed.version
            ));
            ui.add_space(6.0);
            egui::ScrollArea::vertical().max_height(380.0).auto_shrink([false, true]).show(ui, |ui| {
                if draft.whats_new.is_some() || draft.yours.is_some() || draft.edited {
                    ui.strong(format!("What's new in version {}", draft.summary.version));
                    match &draft.whats_new {
                        Some(diff) => diff_ui(ui, diff),
                        None if draft.fallback.is_none() => {
                            ui.weak("(Too big to show.)");
                        }
                        None => {}
                    }
                }
                if draft.edited {
                    ui.add_space(8.0);
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "You've changed your copy since installing it, and updating replaces it. Your changes:",
                    );
                    match (&draft.yours, &draft.fallback) {
                        (Some(diff), _) => {
                            diff_ui(ui, diff);
                            ui.add_space(4.0);
                            if draft.merged.is_some() {
                                ui.label("They don't touch the lines the author changed, so they can be kept.");
                            } else {
                                ui.weak(
                                    "They touch lines the author changed too, so they can't be kept automatically: \
                                     update and make them again, or keep your copy.",
                                );
                            }
                        }
                        (None, Some(diff)) => {
                            ui.weak(
                                "(The version you installed isn't available to compare with, so this is your copy \
                                 against the new version, yours in red.)",
                            );
                            diff_ui(ui, diff);
                        }
                        (None, None) => {
                            ui.weak("(Too big to show.)");
                        }
                    }
                }
            });
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("Cancel").clicked() {
                    close = true;
                }
                let label = if draft.edited { "Update (replace my changes)" } else { "Update" };
                if ui.button(label).clicked() {
                    apply = true;
                }
                if draft.merged.is_some() && ui.button("Update, keeping my changes").clicked() {
                    keep = true;
                }
            });
        });
        if (apply || keep) && let Some(draft) = self.library.update.take() {
            self.apply_update(draft, keep);
        } else if close || response.should_close() {
            self.library.update = None;
        }
    }

    /// Write the new version (with the user's changes merged in, if
    /// `keep`); either way, it's the new version that's installed.
    fn apply_update(&mut self, draft: UpdateDraft, keep: bool) {
        let UpdateDraft { script, installed, summary, download, merged, .. } = draft;
        let text = if keep { merged.unwrap_or_else(|| download.source.clone()) } else { download.source.clone() };
        library::save_original(&download.source);
        let updated = Installed {
            id: summary.id.clone(),
            version: download.version,
            sha256: download.sha256.clone(),
            name: summary.name.clone(),
            slug: summary.slug.clone(),
            author_id: summary.author_id.clone(),
            ..installed
        };
        match std::fs::write(&script, &text).and_then(|()| updated.write(&script)) {
            Ok(()) => {
                log::info!("updated {} to version {}", script.display(), download.version);
                let kept = if keep { ", your changes kept" } else { "" };
                self.status = format!("Updated \"{}\" to version {}{kept}.", summary.name, download.version);
                self.library.sidecar = None;
                self.refresh_installed();
                // The running script picks up the new version straight away.
                if self.visualizer.script().path() == Some(script.as_path())
                    && let Some(idx) = self.available_scripts.iter().position(|p| *p == script)
                {
                    self.load_script(idx);
                }
            }
            Err(e) => self.status = format!("Couldn't update {}: {e}", script.display()),
        }
    }

    /// Publish...: what to share the script as, and where.
    pub(super) fn publish_ui(&mut self, ctx: &egui::Context) {
        let Some(mut draft) = self.library.publish.take() else { return };
        let servers = self.config.library.enabled_servers();
        let my_id = self.library.my_id();
        let mut send = false;
        let mut close = false;
        let busy = self.library.busy;
        let signed_only = self.library.infos.get(&draft.server).is_some_and(|i| i.mode == "signed");
        if signed_only {
            draft.anonymous = false;
        }
        // One of yours already, on this server: publishing makes a new version.
        let updating = draft
            .published
            .as_ref()
            .filter(|p| p.server == draft.server && p.slug.is_some() && p.author_id.is_some() && p.author_id == my_id)
            .and_then(|p| p.slug.clone())
            .filter(|_| !draft.anonymous && !draft.as_new);
        let response = egui::Modal::new(egui::Id::new("library_publish")).show(ctx, |ui| {
            ui.set_width(480.0);
            ui.heading(if updating.is_some() { "Publish a new version" } else { "Publish a script" });
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
                        if ui.selectable_value(&mut draft.server, s.clone(), host(s)).clicked() {
                            draft.slug_edited = Some(Instant::now() - SLUG_CHECK_AFTER);
                        }
                    }
                });
                ui.end_row();
                if !draft.anonymous {
                    ui.label("Slug");
                    match &updating {
                        Some(slug) => {
                            ui.horizontal(|ui| {
                                ui.label(format!("{slug} (version after the last)"));
                                info_icon(ui).on_hover_text(
                                    "You published this script before, so it becomes its next version. Untick \
                                     below to publish it as a new script instead.",
                                );
                            });
                        }
                        None => {
                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    if ui.add(egui::TextEdit::singleline(&mut draft.slug).desired_width(200.0)).changed() {
                                        draft.slug_edited = Some(Instant::now());
                                    }
                                    info_icon(ui).on_hover_text(
                                        "Its name among your uploads: lowercase letters, digits and dashes. \
                                         Publishing again with the same slug adds a new version instead of a new \
                                         script.",
                                    );
                                });
                                let slug = draft.slug.trim();
                                let found = draft
                                    .slug_found
                                    .as_ref()
                                    .filter(|(server, s, _)| *server == draft.server && s == slug)
                                    .map(|(_, _, found)| found);
                                if !library::is_slug(slug) {
                                    ui.colored_label(
                                        ui.visuals().error_fg_color,
                                        "lowercase letters, digits and dashes only (not at the ends)",
                                    );
                                } else if let Some(Some(taken)) = found {
                                    ui.colored_label(
                                        ui.visuals().warn_fg_color,
                                        format!(
                                            "You already use this for \"{}\": publishing makes it that script's \
                                             next version",
                                            taken.name
                                        ),
                                    );
                                } else if found.is_some() {
                                    ui.weak("not used yet");
                                }
                            });
                        }
                    }
                    ui.end_row();
                }
            });
            ui.label("Description");
            ui.add(egui::TextEdit::multiline(&mut draft.description).desired_rows(5).desired_width(f32::INFINITY));
            if draft.published.as_ref().is_some_and(|p| p.server == draft.server && p.author_id.is_some() && p.author_id == my_id) {
                let mut as_update = !draft.as_new;
                if ui.checkbox(&mut as_update, "A new version of the one published before").changed() {
                    draft.as_new = !as_update;
                }
            }
            ui.add_enabled_ui(!signed_only, |ui| {
                ui.checkbox(&mut draft.anonymous, "Post anonymously")
                    .on_hover_text(
                        "Not signed with your identity: it can't be given new versions or deleted later, and it \
                         isn't listed with your uploads.",
                    )
                    .on_disabled_hover_text("This server only takes signed uploads.");
            });
            match (&my_id, draft.anonymous) {
                (_, true) => ui.weak("Anonymous: once it's up, it can't be changed or taken down from here."),
                (Some(id), false) => ui.weak(format!("Signed as #{id}: you can publish new versions or delete it later.")),
                (None, false) => ui.weak("Signed with your identity, made now (Preferences > Library to back it up)."),
            };
            if let Some(info) = self.library.infos.get(&draft.server) {
                ui.weak(format!("Shared under {}. {}", info.license.replace('-', " "), info.rules));
            }
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
            self.send_publish(&mut draft, updating);
        } else if updating.is_none() && !draft.anonymous {
            self.check_slug(&mut draft, ctx);
        }
        self.library.publish = Some(draft);
    }

    /// Once typing pauses, look up whether the slug is one of the user's
    /// already (nothing to look up before they have an identity).
    fn check_slug(&mut self, draft: &mut PublishDraft, ctx: &egui::Context) {
        let Some(edited) = draft.slug_edited else { return };
        let (Some(author), slug) = (self.library.my_id(), draft.slug.trim().to_string()) else {
            draft.slug_edited = None;
            return;
        };
        let wait = SLUG_CHECK_AFTER.saturating_sub(edited.elapsed());
        if !wait.is_zero() {
            ctx.request_repaint_after(wait);
            return;
        }
        draft.slug_edited = None;
        let asked = (draft.server.clone(), slug.clone());
        if !library::is_slug(&slug) || draft.slug_asked.as_ref() == Some(&asked) {
            return;
        }
        draft.slug_asked = Some(asked);
        let server = draft.server.clone();
        self.spawn_request(move || {
            let result = client::by_slug(&server, &author, &slug);
            Event::SlugChecked(server, slug, result)
        });
    }

    /// Check the draft and upload it (signed unless it's anonymous, the
    /// identity made first if there's none yet).
    fn send_publish(&mut self, draft: &mut PublishDraft, updating: Option<String>) {
        let source = std::fs::read_to_string(&draft.script).unwrap_or_default();
        let slug = if draft.anonymous { None } else { Some(updating.unwrap_or_else(|| draft.slug.trim().to_string())) };
        let upload = Upload {
            name: draft.name.trim().to_string(),
            description: draft.description.trim().to_string(),
            category: draft.category.to_string(),
            tags: library::parse_tags(&draft.tags),
            author_name: draft.author_name.trim().to_string(),
            source,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            slug,
        };
        if let Err(problem) = library::check_upload(&upload) {
            draft.error = Some(format!("Can't publish it: {problem}."));
            return;
        }
        let identity = if draft.anonymous {
            None
        } else {
            match self.ensure_identity() {
                Ok(identity) => Some(identity),
                Err(e) => {
                    draft.error = Some(format!("Couldn't make your identity: {e}"));
                    return;
                }
            }
        };
        draft.error = None;
        self.library.busy = true;
        if self.config.library.author_name != upload.author_name {
            self.config.library.author_name = upload.author_name.clone();
            let _ = self.config.save();
        }
        let (server, script) = (draft.server.clone(), draft.script.clone());
        let pinned = self.config.library.pinned.get(&server).cloned();
        self.spawn_request(move || {
            // (A server not searched yet: its key, first.)
            let key = match pinned {
                Some(key) => Ok(key),
                None => client::info(&server).map(|i| i.key),
            };
            let result = key.clone().and_then(|key| client::upload(&server, &key, &upload, identity.as_ref()));
            Event::Published(server, key.unwrap_or_default(), script, Box::new(upload), result)
        });
    }

    /// The identity (a fresh copy for a request's thread), made and saved
    /// first if there isn't one.
    fn ensure_identity(&mut self) -> Result<Identity, String> {
        if self.library.identity.is_none() {
            let identity = Identity::generate()?;
            identity.save()?;
            log::info!("made the library identity #{}", identity.id());
            self.library.identity = Some(identity);
        }
        Identity::load().ok_or_else(|| "it wasn't saved".to_string())
    }

    /// Preferences > Library: the servers in use, and the identity.
    pub(super) fn library_preferences_ui(&mut self, ui: &mut egui::Ui) {
        let before = (self.config.library.official, self.config.library.author_name.clone());
        let mut changed = false;
        with_info(
            ui,
            "The official script library. It can be turned off, but not removed.",
            |ui| ui.checkbox(&mut self.config.library.official, format!("Official server ({})", host(&library::official_url()))),
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
            let known = url == library::official_url() || self.config.library.servers.iter().any(|s| s.url == url);
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

        ui.add_space(6.0);
        ui.strong("Your identity");
        self.identity_ui(ui);
    }

    /// The identity: its ID, made on request; export and import.
    fn identity_ui(&mut self, ui: &mut egui::Ui) {
        let lib = &mut self.library;
        match &lib.identity {
            Some(identity) => {
                let id = identity.id();
                ui.horizontal(|ui| {
                    ui.label(format!("#{id}"));
                    if ui.small_button("Copy").clicked() {
                        ui.ctx().copy_text(format!("#{id}"));
                    }
                    info_icon(ui).on_hover_text(
                        "A key made by this app, kept in its config folder: it signs what you publish, so only \
                         you can change or delete it. No account, no email. Back it up with Export: lost, it \
                         can't be replaced (your scripts stay up, but you can't change them).",
                    );
                });
            }
            None => {
                ui.weak("None yet: one's made the first time you publish signed.");
            }
        }
        ui.horizontal(|ui| {
            ui.label("Passphrase");
            ui.add(egui::TextEdit::singleline(&mut lib.passphrase).password(true).desired_width(160.0))
                .on_hover_text("Seals an exported identity (optional, but wise), or opens a sealed one being imported.");
        });
        let mut export = false;
        let mut import = false;
        let mut make = false;
        ui.horizontal(|ui| {
            if lib.identity.is_none() {
                make = ui.button("Make one now").clicked();
            }
            export = ui.add_enabled(lib.identity.is_some(), egui::Button::new("Export...")).clicked();
            let label = if lib.confirm_import { "Import... (replaces yours: click again)" } else { "Import..." };
            if ui.button(label).clicked() {
                if lib.identity.is_some() && !lib.confirm_import {
                    lib.confirm_import = true;
                } else {
                    import = true;
                }
            }
        });
        if let Some(message) = &lib.identity_message {
            ui.weak(message.as_str());
        }
        if make {
            self.library.identity_message = match self.ensure_identity() {
                Ok(identity) => Some(format!("Made #{}: export it somewhere safe.", identity.id())),
                Err(e) => Some(format!("Couldn't make one: {e}")),
            };
        }
        if export && let Some(identity) = &self.library.identity {
            let file = rfd::FileDialog::new()
                .set_file_name(format!("synththing identity {}.json", identity.id()))
                .add_filter("Identity", &["json"])
                .save_file();
            if let Some(file) = file {
                self.library.identity_message = Some(
                    match identity.export(&self.library.passphrase).and_then(|text| library::identity::save_secret(&file, &text)) {
                        Ok(()) if self.library.passphrase.is_empty() => {
                            format!("Exported to {} (not sealed: keep it private).", file.display())
                        }
                        Ok(()) => format!("Exported to {}, sealed with the passphrase.", file.display()),
                        Err(e) => format!("Couldn't export it: {e}"),
                    },
                );
            }
        }
        if import {
            self.library.confirm_import = false;
            if let Some(file) = rfd::FileDialog::new().add_filter("Identity", &["json"]).pick_file() {
                let passphrase = self.library.passphrase.clone();
                let result = std::fs::read_to_string(&file)
                    .map_err(|e| e.to_string())
                    .and_then(|text| {
                        if passphrase.is_empty() && Identity::needs_passphrase(&text) {
                            return Err("it's sealed: type its passphrase above first".to_string());
                        }
                        Identity::import(&text, &passphrase)
                    })
                    .and_then(|identity| identity.save().map(|()| identity));
                self.library.identity_message = Some(match result {
                    Ok(identity) => {
                        let id = identity.id();
                        log::info!("imported the library identity #{id}");
                        self.library.identity = Some(identity);
                        format!("Now #{id}.")
                    }
                    Err(e) => format!("Couldn't import it: {e}"),
                });
            }
        }
    }
}
