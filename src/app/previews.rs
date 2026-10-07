//! Previews in the app (`preview.rs`): the server's, in the Online Library,
//! and one for every script in the script picker's sidebar. Those are made
//! here in the background the first time a script is looked at (by
//! `synththing preview`, as a process of its own), or kept from the server
//! when the script was installed, and cached by the source's SHA-256 in
//! `library/previews` in the config folder: an edited script gets a new one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use eframe::egui;

use crate::library::{self, Installed};
use crate::lua_visualizer;
use crate::preview::{self, COLUMNS, FPS, FRAMES};

/// How long making one preview here may take.
const TIME_LIMIT: Duration = Duration::from_secs(60);

/// A preview, ready to draw.
pub struct PreviewImage {
    still: egui::TextureHandle,
    sheet: Option<egui::TextureHandle>,
}

impl PreviewImage {
    /// From the still's and the sheet's PNGs.
    pub fn from_png(ctx: &egui::Context, name: &str, still: &[u8], sheet: Option<&[u8]>) -> Result<Self, String> {
        let texture = |name: String, bytes: &[u8]| {
            let (w, h, rgba) = preview::decode_png(bytes)?;
            let image = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
            Ok::<_, String>(ctx.load_texture(name, image, egui::TextureOptions::LINEAR))
        };
        Ok(Self {
            still: texture(format!("{name}-still"), still)?,
            sheet: sheet.map(|bytes| texture(format!("{name}-sheet"), bytes)).transpose()?,
        })
    }

    /// Draw it `width` wide: the animation, playing, or the still.
    pub fn ui(&self, ui: &mut egui::Ui, width: f32) -> egui::Response {
        let still = self.still.size_vec2();
        let size = egui::vec2(width, width * still.y / still.x.max(1.0));
        match &self.sheet {
            Some(sheet) => {
                let rows = FRAMES / COLUMNS;
                let frame = (ui.input(|i| i.time) * FPS) as usize % FRAMES;
                let (column, row) = ((frame % COLUMNS) as f32, (frame / COLUMNS) as f32);
                let uv = egui::Rect::from_min_size(
                    egui::pos2(column / COLUMNS as f32, row / rows as f32),
                    egui::vec2(1.0 / COLUMNS as f32, 1.0 / rows as f32),
                );
                ui.ctx().request_repaint_after(Duration::from_secs_f64(1.0 / FPS));
                ui.add(egui::Image::new((sheet.id(), size)).uv(uv).corner_radius(4.0))
            }
            None => ui.add(egui::Image::new((self.still.id(), size)).corner_radius(4.0)),
        }
    }
}

/// A space the size of a preview `width` wide, with `text` in it.
pub fn placeholder_ui(ui: &mut egui::Ui, width: f32, text: &str, spinner: bool) {
    let size = egui::vec2(width, width * preview::HEIGHT as f32 / preview::WIDTH as f32);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
    // (In a child of its own: `put` would move the cursor back up to the
    // text's box, inside the space, and what follows would overlap it.)
    let mut inside = ui.new_child(
        egui::UiBuilder::new().max_rect(rect.shrink(12.0)).layout(egui::Layout::top_down(egui::Align::Center)),
    );
    if spinner {
        inside.put(egui::Rect::from_center_size(rect.center() - egui::vec2(0.0, 16.0), egui::vec2(20.0, 20.0)), egui::Spinner::new());
    }
    let below = if spinner { egui::vec2(0.0, 14.0) } else { egui::Vec2::ZERO };
    inside.put(
        egui::Rect::from_center_size(rect.center() + below, rect.shrink(12.0).size()),
        egui::Label::new(egui::RichText::new(text).weak()).wrap(),
    );
}

/// What the sidebar says about a script, read from its file (again when
/// the file changes).
pub struct Card {
    modified: Option<SystemTime>,
    pub sha256: String,
    /// Its leading comment.
    pub description: String,
    pub installed: Option<Installed>,
}

/// Where a script's preview has got to.
pub enum Shown<'a> {
    Ready(&'a PreviewImage),
    Making,
    Missing(String),
}

/// Previews being made, one at a time.
struct Making {
    sha256: String,
    done: Arc<AtomicBool>,
}

/// The script picker's previews and cards.
#[derive(Default)]
pub struct LocalPreviews {
    cards: HashMap<PathBuf, Card>,
    /// Loaded, by SHA-256 (an error: there isn't one, and why).
    images: HashMap<String, Result<PreviewImage, String>>,
    making: Option<Making>,
    /// The next to make, once the one under way is done (only the latest
    /// asked for: the one being looked at).
    waiting: Option<(String, PathBuf, PathBuf)>,
    /// The sidebar's script: the last one hovered, while the list is open.
    pub shown: Option<PathBuf>,
    pub drawn_at: u64,
}

pub fn cache_dir() -> Option<PathBuf> {
    crate::config::config_dir().ok().map(|d| d.join("library").join("previews"))
}

fn cached(dir: &Path, sha256: &str, sheet: bool) -> PathBuf {
    dir.join(format!("{sha256}{}.png", if sheet { "-sheet" } else { "" }))
}

/// Keep a server's preview of a script installed from it (`sha256`, its
/// source's), so it isn't made again here.
pub fn keep(sha256: &str, still: &[u8], sheet: Option<&[u8]>) {
    let Some(dir) = cache_dir() else { return };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let _ = std::fs::write(cached(&dir, sha256, false), still);
    if let Some(sheet) = sheet {
        let _ = std::fs::write(cached(&dir, sha256, true), sheet);
    }
}

impl LocalPreviews {
    /// `script`'s card (read again if the file changed).
    pub fn card(&mut self, script: &Path) -> Option<&Card> {
        let modified = std::fs::metadata(script).and_then(|m| m.modified()).ok();
        if self.cards.get(script).is_none_or(|c| c.modified != modified || modified.is_none()) {
            let source = std::fs::read_to_string(script).ok()?;
            let card = Card {
                modified,
                sha256: library::sha256_hex(source.as_bytes()),
                description: library::leading_comment(&source),
                installed: Installed::read(script),
            };
            self.cards.insert(script.to_path_buf(), card);
        }
        self.cards.get(script)
    }

    /// `script`'s preview (its source's SHA-256 `sha256`): cached, or made
    /// now (with `soundfont`) if it isn't.
    pub fn preview(&mut self, ctx: &egui::Context, script: &Path, sha256: &str, soundfont: Option<PathBuf>) -> Shown<'_> {
        self.poll(ctx);
        if !self.images.contains_key(sha256)
            && let Some(dir) = cache_dir()
        {
            let still = std::fs::read(cached(&dir, sha256, false));
            let missing = std::fs::read_to_string(dir.join(format!("{sha256}.none")));
            if let Ok(still) = still {
                let sheet = std::fs::read(cached(&dir, sha256, true)).ok();
                let image = PreviewImage::from_png(ctx, sha256, &still, sheet.as_deref());
                self.images.insert(sha256.to_string(), image);
            } else if let Ok(why) = missing {
                self.images.insert(sha256.to_string(), Err(why));
            }
        }
        if self.images.contains_key(sha256) {
            return match &self.images[sha256] {
                Ok(image) => Shown::Ready(image),
                Err(why) => Shown::Missing(why.clone()),
            };
        }
        if self.making.as_ref().is_some_and(|m| m.sha256 == sha256) {
            return Shown::Making;
        }
        let Some(soundfont) = soundfont else {
            return Shown::Missing("Add a soundfont to see previews (they play the starter songs).".into());
        };
        if self.making.is_some() {
            self.waiting = Some((sha256.to_string(), script.to_path_buf(), soundfont));
        } else {
            self.start(ctx, sha256.to_string(), script.to_path_buf(), soundfont);
        }
        Shown::Making
    }

    fn start(&mut self, ctx: &egui::Context, sha256: String, script: PathBuf, soundfont: PathBuf) {
        let Some(dir) = cache_dir() else { return };
        let done = Arc::new(AtomicBool::new(false));
        self.making = Some(Making { sha256: sha256.clone(), done: Arc::clone(&done) });
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let work = dir.join(format!("work-{sha256}"));
            let result = preview::run_process(&script, &soundfont, &work, TIME_LIMIT, false);
            let still = work.join(preview::STILL);
            if still.exists() {
                let _ = std::fs::rename(&still, cached(&dir, &sha256, false));
                let _ = std::fs::rename(work.join(preview::SHEET), cached(&dir, &sha256, true));
            } else {
                let why = match &result {
                    Err(e) => format!("No preview: {}", e.trim_start_matches(preview::SCRIPT_ERROR)),
                    Ok(()) => "No preview: it doesn't draw anything.".to_string(),
                };
                let _ = std::fs::write(dir.join(format!("{sha256}.none")), why);
            }
            if let Err(e) = &result {
                log::info!("preview of {}: {e}", script.display());
            }
            let _ = std::fs::remove_dir_all(&work);
            done.store(true, Ordering::Relaxed);
            ctx.request_repaint();
        });
    }

    /// The one being made is done: on to the next.
    fn poll(&mut self, ctx: &egui::Context) {
        if !self.making.as_ref().is_some_and(|m| m.done.load(Ordering::Relaxed)) {
            return;
        }
        self.making = None;
        if let Some((sha256, script, soundfont)) = self.waiting.take()
            && !self.images.contains_key(&sha256)
        {
            self.start(ctx, sha256, script, soundfont);
        }
    }
}

/// The picker's sidebar width.
const SIDEBAR: f32 = 300.0;

impl super::App {
    /// What previews are made with here: the starter pack's TimGM6mb (what
    /// library servers use), else the first soundfont in the list.
    fn preview_soundfont(&self) -> Option<PathBuf> {
        crate::starter::soundfonts_dir()
            .ok()
            .map(|d| d.join(crate::starter::SOUNDFONTS[1].file))
            .filter(|p| p.exists())
            .or_else(|| self.config.soundfonts.iter().find(|p| p.exists()).cloned())
    }

    /// The scripts to pick from, for a combo box: grouped, the bundled
    /// visualizers and games, then the user's own (and copies of templates
    /// and examples), beside a sidebar about the one under the mouse (its
    /// preview, where it's from, its description). Returns the one clicked.
    pub(super) fn script_list_ui(&self, ui: &mut egui::Ui) -> Option<usize> {
        let mut picked = None;
        let pass = ui.ctx().cumulative_pass_nr();
        {
            let mut previews = self.script_previews.borrow_mut();
            // Opened again: the sidebar starts on the selected script.
            if previews.drawn_at + 1 < pass {
                previews.shown = None;
            }
            previews.drawn_at = pass;
        }
        let category_of = |path: &PathBuf| {
            path.file_name()
                .and_then(|n| n.to_str())
                .and_then(lua_visualizer::bundled_category)
                .filter(|c| matches!(*c, "visualizers" | "games"))
        };
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                egui::ScrollArea::vertical().id_salt("script_list").max_height(420.0).show(ui, |ui| {
                    for group in [Some("visualizers"), Some("games"), None] {
                        let members: Vec<(usize, &PathBuf)> = self
                            .available_scripts
                            .iter()
                            .enumerate()
                            .filter(|(_, path)| category_of(path) == group)
                            .collect();
                        if members.is_empty() {
                            continue;
                        }
                        ui.label(egui::RichText::new(lua_visualizer::category_title(group.unwrap_or(""))).weak().small());
                        for (i, path) in members {
                            let name = lua_visualizer::display_name(path);
                            let response = ui.selectable_label(Some(i) == self.active_script, name);
                            if response.hovered() {
                                self.script_previews.borrow_mut().shown = Some(path.clone());
                            }
                            if response.clicked() {
                                picked = Some(i);
                            }
                        }
                    }
                });
            });
            ui.separator();
            ui.vertical(|ui| {
                ui.set_width(SIDEBAR);
                let shown = self.script_previews.borrow().shown.clone();
                let shown = shown.or_else(|| self.active_script.and_then(|i| self.available_scripts.get(i).cloned()));
                match shown {
                    Some(path) => self.script_card_ui(ui, &path),
                    None => {
                        ui.weak("Point at a script to see it here.");
                    }
                }
            });
        });
        picked
    }

    /// The sidebar for one script.
    fn script_card_ui(&self, ui: &mut egui::Ui, script: &Path) {
        let soundfont = self.preview_soundfont();
        let mut previews = self.script_previews.borrow_mut();
        let Some(card) = previews.card(script) else {
            ui.weak("Couldn't read it.");
            return;
        };
        let (sha256, description, installed) = (card.sha256.clone(), card.description.clone(), card.installed.clone());
        match previews.preview(ui.ctx(), script, &sha256, soundfont) {
            Shown::Ready(image) => {
                image.ui(ui, SIDEBAR);
            }
            Shown::Making => placeholder_ui(ui, SIDEBAR, "Making its preview...", true),
            Shown::Missing(why) => placeholder_ui(ui, SIDEBAR, &why, false),
        }
        drop(previews);

        ui.add_space(4.0);
        ui.strong(lua_visualizer::display_name(script));
        let file = script.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let bundled = lua_visualizer::bundled_category(file).map(|c| match c {
            "games" => "game",
            "examples" => "example",
            "templates" => "template",
            _ => "visualizer",
        });
        let category = installed.as_ref().and_then(|i| i.category.clone()).or(bundled.map(str::to_string));
        let official = |id: &Option<String>| id.as_deref().is_some_and(library::is_official_id);
        let mine = self.library.my_id();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if let Some(category) = &category {
                ui.weak(format!("{} ·", library::category_title(category)));
            }
            match &installed {
                Some(i) if official(&i.author_id) || (i.id.is_empty() && bundled.is_some()) => {
                    ui.weak("by");
                    ui.label(egui::RichText::new(i.author_name.as_deref().unwrap_or("synththing")).color(super::library::OFFICIAL_GOLD))
                        .on_hover_text(super::library::OFFICIAL_HOVER);
                }
                Some(i) if i.receipt.is_some() || (i.author_id.is_some() && i.author_id == mine) => {
                    ui.weak(format!("yours, published on {}", super::library::host(&i.server)));
                }
                Some(i) => {
                    let author = i
                        .author_name
                        .clone()
                        .or_else(|| i.author_id.as_ref().map(|id| format!("#{id}")))
                        .unwrap_or_else(|| "someone anonymous".to_string());
                    ui.weak(format!("by {author}, from {}", super::library::host(&i.server)));
                }
                None if bundled.is_some() => {
                    ui.weak("by");
                    ui.label(egui::RichText::new("synththing").color(super::library::OFFICIAL_GOLD))
                        .on_hover_text(super::library::OFFICIAL_HOVER);
                }
                None => {
                    ui.weak("your script");
                }
            }
        });
        if !description.is_empty() {
            const MOST: usize = 600;
            let text = if description.chars().count() > MOST {
                description.chars().take(MOST).collect::<String>() + "..."
            } else {
                description
            };
            ui.add_space(4.0);
            ui.label(egui::RichText::new(text).small());
        }
    }
}
