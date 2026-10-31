//! Choose art: picking what an album's shown with (`song_store::Art`,
//! synced with its songs): what its files have, a picture of its release
//! at the Cover Art Archive, one of the iTunes Store's covers, asked
//! for on demand, or a picture of one's own (`pictures.rs`).

use std::path::PathBuf;
use std::sync::mpsc;

use eframe::egui;

use super::App;
use crate::album_art::Source;
use crate::build_info;
use crate::lookup::art::{self, Choice};
use crate::song_store::Art;

/// How big each choice is drawn.
const TILE: f32 = 120.0;

/// Pictures being asked for, or what came of it.
enum Asked {
    Waiting(mpsc::Receiver<Result<Vec<Choice>, String>>),
    Done(Result<Vec<Choice>, String>),
}

impl Asked {
    fn start(ask: impl FnOnce() -> anyhow::Result<Vec<Choice>> + Send + 'static) -> Self {
        let (send, receive) = mpsc::channel();
        std::thread::spawn(move || send.send(ask().map_err(|e| format!("{e:#}"))));
        Self::Waiting(receive)
    }

    fn poll(&mut self) {
        if let Self::Waiting(receive) = self
            && let Ok(done) = receive.try_recv()
        {
            *self = Self::Done(done);
        }
    }
}

pub(super) struct ChooseArt {
    title: String,
    artist: Option<String>,
    /// Its songs, by key.
    keys: Vec<String>,
    /// Where its art comes from now, but for a choice.
    file: Option<PathBuf>,
    release: Option<(String, Option<String>)>,
    archive: Option<Asked>,
    itunes: Option<Asked>,
}

impl App {
    /// Open Choose art for the album `title` (by `artist`) of the songs
    /// `keys`, whose art comes from `from`.
    pub(super) fn open_choose_art(&mut self, title: &str, artist: Option<String>, keys: Vec<String>, from: Source) {
        self.choose_art = Some(ChooseArt { title: title.to_string(), artist, keys, file: from.file, release: from.release, archive: None, itunes: None });
    }

    /// The art chosen for the open album (`None`: as it'd be by itself).
    fn chosen_art(&self, keys: &[String]) -> Option<Art> {
        keys.iter().find_map(|key| self.songs.get_by_key(key)?.online.as_ref()?.art.clone())
    }

    /// Show the album with `art` (`None`: as it'd be by itself).
    pub(super) fn choose_art(&mut self, keys: &[String], art: Option<Art>) {
        self.songs.set_art(keys, art);
        self.songs_changed();
    }

    /// The Choose art window, while it's open.
    pub(super) fn choose_art_ui(&mut self, ctx: &egui::Context) {
        let Some(mut choose) = self.choose_art.take() else { return };
        let online = build_info::online(build_info::Online::Lookup);
        // (Its release's pictures are asked for once it's open, and online.)
        if online
            && choose.archive.is_none()
            && let Some((release, group)) = choose.release.clone()
        {
            choose.archive = Some(Asked::start(move || art::cover_art_archive(&release, group.as_deref())));
        }
        for asked in [&mut choose.archive, &mut choose.itunes].into_iter().flatten() {
            asked.poll();
        }
        let chosen = self.chosen_art(&choose.keys);
        let mut pick: Option<Option<Art>> = None;
        let mut open = true;
        let mut search_itunes = false;
        let mut turn_on = false;
        let mut pick_own = false;
        let art = &mut self.groupings.art;
        egui::Window::new(format!("Art for {}", choose.title))
            .id(egui::Id::new("choose_art"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .constrain_to(super::window_room(ctx))
            .default_width(4.0 * (TILE + 12.0))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().max_height(520.0).show(ui, |ui| {
                    let automatic = Source { file: choose.file.clone(), release: choose.release.clone(), chosen: None, image: None };
                    let mut tiles: Vec<(Source, String, Option<Art>)> = vec![(automatic, "Automatic".into(), None)];
                    if choose.file.is_some() {
                        let files = Source { file: choose.file.clone(), chosen: Some(Art::Files), ..Source::default() };
                        tiles.push((files, "From its files".into(), Some(Art::Files)));
                    }
                    if let Some(Art::Image(key)) = &chosen {
                        tiles.push((Source { chosen: chosen.clone(), ..Source::default() }, "Your picture".into(), Some(Art::Image(key.clone()))));
                    }
                    ui.weak("Automatic: its files' art, else its release's front cover once it's matched at MusicBrainz.");
                    choices_ui(ui, art, &tiles, &chosen, &mut pick, &choose.title);
                    pick_own = ui.button("Pick a picture...").on_hover_text("Show it with a picture of your own").clicked();

                    if !online {
                        ui.separator();
                        ui.horizontal_wrapped(|ui| {
                            ui.weak(build_info::Online::Lookup.off_note());
                            turn_on = ui.button("Turn on for this run").clicked();
                        });
                        return;
                    }
                    ui.separator();
                    ui.strong("Cover Art Archive");
                    match &choose.archive {
                        None => {
                            ui.weak("It isn't matched at MusicBrainz yet.");
                        }
                        Some(asked) => asked_ui(ui, art, asked, &chosen, &mut pick, &choose.title, "It has no pictures there."),
                    }

                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.strong("iTunes Store");
                        if choose.itunes.is_none() {
                            search_itunes = ui.button("Search").on_hover_text("Ask the iTunes Store for covers of albums named like it").clicked();
                        }
                    });
                    if let Some(asked) = &choose.itunes {
                        asked_ui(ui, art, asked, &chosen, &mut pick, &choose.title, "It found nothing named like it.");
                    }
                });
            });
        if turn_on {
            self.set_online(build_info::Online::Lookup, true);
        }
        if pick_own {
            self.pick_picture(super::pictures::PictureFor::Album(choose.keys.clone()));
        }
        if search_itunes {
            let term = match &choose.artist {
                Some(artist) => format!("{} {artist}", crate::lookup::matching::search_name(&choose.title)),
                None => crate::lookup::matching::search_name(&choose.title),
            };
            choose.itunes = Some(Asked::start(move || art::itunes(&term).map(|found| found.iter().map(art::Listed::choice).collect())));
        }
        if let Some(art) = pick {
            self.choose_art(&choose.keys, art);
        }
        if open {
            self.choose_art = Some(choose);
        }
    }
}

/// Online pictures being asked for: a spinner, the error, or the choices.
fn asked_ui(
    ui: &mut egui::Ui,
    art: &mut crate::album_art::AlbumArt,
    asked: &Asked,
    chosen: &Option<Art>,
    pick: &mut Option<Option<Art>>,
    title: &str,
    none: &str,
) {
    match asked {
        Asked::Waiting(_) => {
            ui.spinner();
        }
        Asked::Done(Err(e)) => {
            ui.colored_label(ui.visuals().warn_fg_color, format!("Couldn't ask: {e}"));
        }
        Asked::Done(Ok(found)) if found.is_empty() => {
            ui.weak(none);
        }
        Asked::Done(Ok(found)) => {
            let tiles: Vec<(Source, String, Option<Art>)> =
                found.iter().map(|c| (Source::url(&c.url), c.label.clone(), Some(Art::Url(c.url.clone())))).collect();
            choices_ui(ui, art, &tiles, chosen, pick, title);
        }
    }
}

/// A row (wrapping) of pictures to choose from, the chosen one outlined.
fn choices_ui(
    ui: &mut egui::Ui,
    art: &mut crate::album_art::AlbumArt,
    tiles: &[(Source, String, Option<Art>)],
    chosen: &Option<Art>,
    pick: &mut Option<Option<Art>>,
    title: &str,
) {
    ui.horizontal_wrapped(|ui| {
        for (source, label, this) in tiles {
            ui.vertical(|ui| {
                ui.set_width(TILE);
                let (rect, response) = ui.allocate_exact_size(egui::vec2(TILE, TILE), egui::Sense::click());
                art.tile(ui, rect, source, title);
                if this == chosen {
                    ui.painter().rect_stroke(rect.expand(2.0), 4.0, ui.visuals().selection.stroke, egui::StrokeKind::Outside);
                } else if response.hovered() {
                    ui.painter().rect_stroke(rect.expand(2.0), 4.0, ui.visuals().widgets.hovered.fg_stroke, egui::StrokeKind::Outside);
                }
                if response.on_hover_text("Show the album with this").clicked() {
                    *pick = Some(this.clone());
                }
                ui.add(egui::Label::new(egui::RichText::new(label).small()).truncate());
            });
        }
    });
}
