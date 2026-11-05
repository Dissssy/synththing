//! The controls bar along the bottom: transport, seek bar, volume and
//! speed, the soundfont and status line, laid out to fold when narrow.

use eframe::egui;

use crate::applog;
use crate::commands::{self, Command};
use crate::audio::AudioCommand;
use crate::config::{self, BarItem};
use crate::engine::{EngineView, MAX_SPEED, MIN_SPEED};
use crate::song_store;
use crate::select_list;
use crate::lua_visualizer;
use crate::playlist::LoopMode;

use super::{App, editor, fmt_time, icon, icon_button, on_color, palette_color, playback_state_text, tags, toast};

impl App {
    /// The seek bar's color: the tempo's (`tempo_color`), easing toward a
    /// new one when the tempo changes, flashing brighter on each beat (more
    /// on a bar's first) while it plays, unless that's turned off. None for
    /// a song without a tempo map (an audio file): the theme's accent.
    pub(super) fn transport_accent(&mut self, ctx: &egui::Context, view: &EngineView) -> Option<egui::Color32> {
        let timing = self.song_notes.as_ref().filter(|_| view.has_midi).and_then(|n| n.timing().cloned());
        let Some(timing) = timing else {
            self.accent = None;
            return None;
        };
        let target = tempo_color(timing.tempo_at(view.position));
        let ease = 1.0 - (-ctx.input(|i| i.stable_dt).min(0.1) * 4.0).exp();
        let shown = match self.accent {
            Some(shown) => [0, 1, 2].map(|i| shown[i] + (target[i] - shown[i]) * ease),
            None => target,
        };
        self.accent = Some(shown);
        if (0..3).any(|i| (shown[i] - target[i]).abs() > 0.5) {
            ctx.request_repaint();
        }
        let playing = !view.paused && !view.finished;
        let strength = self.config.beat_pulse.unwrap_or(config::DEFAULT_BEAT_PULSE).clamp(0.0, 1.0);
        let length = self.config.beat_pulse_length.unwrap_or(config::DEFAULT_BEAT_PULSE_LENGTH).clamp(0.1, 1.0);
        let pulse = if playing && strength > 0.0 {
            ctx.request_repaint();
            let since_beat = timing.beat_at(view.position).fract() as f32;
            let downbeat = timing.bar_at(view.position).1 < 1.0;
            // (A flash `strength` of the way to white, so relative to how dark
            // the color is: 100% is white. It fades over `length` of the beat,
            // half as long again on a bar's first, which marks the bar.)
            let length = if downbeat { (length * 1.5).min(1.0) } else { length };
            let left = (1.0 - since_beat / length).max(0.0);
            left * left * strength
        } else {
            0.0
        };
        let [r, g, b] = shown.map(|c| (c + (255.0 - c) * pulse).round().clamp(0.0, 255.0) as u8);
        Some(egui::Color32::from_rgb(r, g, b))
    }

    /// The bottom transport bar, across the window's width: the song (title
    /// over a weak detail line) with the status line at the right, the seek
    /// bar between the times, then the devices button and quick actions,
    /// the transport in the middle, and speed and volume. When the sides
    /// don't fit beside the transport, it goes on a line of its own over
    /// them. Shown identically whether fullscreen or not, so it's the one
    /// thing that's always reachable.
    pub(super) fn controls_bar_ui(&mut self, ui: &mut egui::Ui, view: &EngineView) {
        if self.following() {
            return self.following_bar_ui(ui, view);
        }
        ui.add_space(6.0);

        // The song at the left, the status line at the right taking at
        // most `STATUS_SHARE` of the line (cut short past that).
        let width = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 34.0), egui::Sense::hover());
        let right = egui::Layout::right_to_left(egui::Align::Center);
        let status = ui
            .scope_builder(egui::UiBuilder::new().max_rect(rect).layout(right), |ui| {
                ui.set_max_width(width * STATUS_SHARE);
                if status_line_ui(ui, &self.status, &mut self.unseen_failure, applog::elapsed, applog::entries) {
                    self.log_search = STATUS_TARGET.to_string();
                    self.open_help(super::help::HelpTab::Log);
                }
            })
            .response
            .rect;
        let title = match self.current_song.clone() {
            Some(path) => self.song_title(&path),
            None => view.track_name.clone().unwrap_or_else(|| "Nothing playing".to_string()),
        };
        let detail = Some(self.song_detail(view)).filter(|d| !d.is_empty());
        let song = rect.with_max_x(status.left() - 12.0);
        ui.scope_builder(egui::UiBuilder::new().max_rect(song).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
            ui.set_clip_rect(song.intersect(ui.clip_rect()));
            select_list::two_lines(ui, std::iter::empty(), egui::RichText::new(title).strong(), detail, true);
        });

        self.seek_line_ui(ui, view);
        ui.add_space(2.0);

        self.controls_line_ui(ui, view, width);
        ui.add_space(6.0);
    }

    /// The seek bar, between the time it's at and the song's length.
    pub(super) fn seek_line_ui(&mut self, ui: &mut egui::Ui, view: &EngineView) {
        let accent = self.transport_accent(ui.ctx(), view);
        ui.horizontal(|ui| {
            ui.weak(fmt_time(view.position));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(fmt_time(view.length));
                ui.spacing_mut().slider_width = ui.available_width();
                let mut position = view.position;
                if seek_bar_ui(ui, &mut position, view.length, accent).changed() {
                    self.send(AudioCommand::Seek(position));
                }
            });
        });
    }

    /// The song playing's art, as big a square as fits in `rect`: its own
    /// (its file's, else its album's), else its artist's picture, else a
    /// tile in its album's (or its name's) color.
    pub(super) fn now_playing_art_ui(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let online = self.config.online_lookup && crate::build_info::online(crate::build_info::Online::Lookup);
        let title = self.current_title();
        let art = &mut self.now_playing_art;
        art.frame(ui.ctx(), online);
        let square = egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(rect.width().min(rect.height()).max(0.0)));
        let Some(path) = self.current_song.clone() else { return };
        let song = self.songs.get(&path);
        if art.picture(ui, square, &crate::album_art::Source::of_song(Some(path.clone()), song)) {
            return;
        }
        let facts = song.map(crate::fields::Facts::of);
        let text = |field| facts.as_ref().and_then(|f| f.text(field)).map(|t| t.into_owned());
        let artist = text(crate::fields::Field::Artist).and_then(|a| a.split(';').next().map(|a| crate::fields::group_key(a.trim(), &self.config.group_ignore)));
        if let Some(key) = artist.and_then(|a| self.songs.artist_images().get(&a))
            && art.picture(ui, square, &crate::album_art::Source::image(key))
        {
            return;
        }
        let name = text(crate::fields::Field::Album).or(title).unwrap_or_default();
        art.tile(ui, square, &crate::album_art::Source::default(), &name);
    }

    /// The bar's detail line: where the song is on its playlist, its
    /// soundfont, or why nothing plays.
    pub(super) fn song_detail(&self, view: &EngineView) -> String {
        let mut parts = Vec::new();
        if let Some(np) = self.playing_entry()
            && let Some(list) = self.playlists.lists.get(np.list)
        {
            parts.push(format!("{} {}/{}", list.playlist.name, np.entry + 1, list.playlist.entries.len()));
        }
        if view.has_midi && view.has_soundfont {
            parts.push(view.soundfont_name.clone().unwrap_or_default());
        }
        match playback_state_text(view) {
            "no song" | "no soundfont" => parts.push(playback_state_text(view).to_string()),
            _ => {}
        }
        parts.retain(|p| !p.is_empty());
        parts.join("  ·  ")
    }

    /// The controls bar's line of buttons and sliders (`config::ControlsBar`):
    /// its left section at the left end, the middle centred (moving over
    /// for the sides when they don't fit beside it), the right at the right
    /// end. The sliders share the room that's left. With too little, speed
    /// and then volume become buttons that open theirs (`Shape`), and then
    /// the star and tag buttons fold, last first, into a ⋯ button listing
    /// them.
    pub(super) fn controls_line_ui(&mut self, ui: &mut egui::Ui, view: &EngineView, width: f32) {
        let gap = ui.spacing().item_spacing.x;
        let mut sections = self.config.controls_bar.sections().map(|items| {
            items
                .into_iter()
                .filter(|item| self.bar_item_shown(item))
                .map(|item| Slot::Item(item.clone()))
                .collect::<Vec<_>>()
        });
        let count = |sections: &[Vec<Slot>; 3], item: BarItem| sections.iter().flatten().filter(|s| **s == Slot::Item(item.clone())).count() as f32;
        let (volumes, speeds) = (count(&sections, BarItem::Volume), count(&sections, BarItem::Speed));
        let fixed = |sections: &[Vec<Slot>; 3]| -> f32 {
            sections.iter().flatten().filter(|s| !matches!(s, Slot::Item(BarItem::Volume | BarItem::Speed))).map(|s| s.width() + gap).sum::<f32>() + 2.0 * gap
        };
        let need = |fixed: f32, volume: Shape, speed: Shape| {
            fixed + volumes * (shape_width(volume, volume_width(gap, MIN_SLIDER)) + gap) + speeds * (shape_width(speed, speed_width(gap, MIN_SLIDER)) + gap)
        };
        // Speed's sliders give way first, then volume's.
        let all_fixed = fixed(&sections);
        let (volume, speed) = [(Shape::Slider, Shape::Slider), (Shape::Slider, Shape::Button), (Shape::Button, Shape::Button)]
            .into_iter()
            .find(|&(v, s)| need(all_fixed, v, s) <= width)
            .unwrap_or((Shape::Button, Shape::Button));
        // Then the star and tag buttons fold, last first, the ⋯ button
        // taking the first folded one's place.
        let over = need(all_fixed, volume, speed) - width;
        if over > 0.0 {
            let toggles: Vec<(usize, usize)> = (0..3)
                .flat_map(|s| (0..sections[s].len()).map(move |i| (s, i)))
                .filter(|&(s, i)| matches!(sections[s][i], Slot::Item(BarItem::Star | BarItem::Tag(_))))
                .collect();
            let fold = (((over / (ICON_BUTTON_WIDTH + gap)).ceil() as usize) + 1).min(toggles.len());
            if fold > 1 {
                let folded = &toggles[toggles.len() - fold..];
                let items: Vec<BarItem> = folded.iter().filter_map(|&(s, i)| if let Slot::Item(item) = &sections[s][i] { Some(item.clone()) } else { None }).collect();
                let (first_s, first_i) = folded[0];
                for &(s, i) in folded.iter().rev() {
                    sections[s].remove(i);
                }
                sections[first_s].insert(first_i, Slot::More(items));
            }
        }
        let (volume, speed) = {
            let fixed = fixed(&sections);
            [(Shape::Slider, Shape::Slider), (Shape::Slider, Shape::Button), (Shape::Button, Shape::Button)]
                .into_iter()
                .find(|&(v, s)| need(fixed, v, s) <= width)
                .unwrap_or((volume, speed))
        };
        let sliders = volumes * f32::from(u8::from(volume == Shape::Slider)) + speeds * f32::from(u8::from(speed == Shape::Slider));
        let spare = width - need(fixed(&sections), volume, speed);
        let slider = if sliders > 0.0 { (MIN_SLIDER + spare / sliders).clamp(MIN_SLIDER, MAX_SLIDER) } else { MIN_SLIDER };
        let shapes = Shapes { volume, speed };
        let slot_width = |slot: &Slot| match slot {
            Slot::Item(BarItem::Volume) if volume == Shape::Slider => volume_width(gap, slider),
            Slot::Item(BarItem::Speed) if speed == Shape::Slider => speed_width(gap, slider),
            Slot::Item(BarItem::Volume | BarItem::Speed) => ICON_BUTTON_WIDTH,
            slot => slot.width(),
        };
        let section_width = |slots: &[Slot]| (slots.iter().map(|s| slot_width(s) + gap).sum::<f32>() - gap).max(0.0);
        let [left_w, middle_w, right_w] = [&sections[0], &sections[1], &sections[2]].map(|s| section_width(s));
        let centred = (width - middle_w) / 2.0;
        let x = if centred >= left_w.max(right_w) + SIDE_GAP {
            centred
        } else {
            centred.min(width - right_w - gap - middle_w).max(left_w + gap).min(width - middle_w).max(0.0)
        };
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, TRANSPORT_HEIGHT), egui::Sense::hover());
        let zone = |from: f32, to: f32| {
            egui::UiBuilder::new()
                .max_rect(egui::Rect::from_x_y_ranges(rect.left() + from..=rect.left() + to.max(from), rect.y_range()))
                .layout(egui::Layout::left_to_right(egui::Align::Center))
        };
        let [left, middle, right] = sections;
        ui.spacing_mut().slider_width = slider;
        ui.scope_builder(zone(0.0, x - gap), |ui| self.controls_slots_ui(ui, view, &left, shapes));
        ui.scope_builder(zone(x, x + middle_w), |ui| self.controls_slots_ui(ui, view, &middle, shapes));
        // The right section ends at the right edge, as wide as it was drawn
        // last frame when that's wider than worked out (buttons run a point
        // or two past their icons' own widths).
        let drawn_id = ui.scope_id().with("controls_right_drawn");
        let drawn = ui.data(|d| d.get_temp::<f32>(drawn_id)).unwrap_or(0.0);
        let right_x = (width - right_w.max(drawn)).max(x + middle_w + gap);
        let drawn = ui.scope_builder(zone(right_x, width), |ui| self.controls_slots_ui(ui, view, &right, shapes)).response.rect.width();
        ui.data_mut(|d| d.insert_temp(drawn_id, drawn));
    }

    /// Whether a controls bar item has something to show (a tag still
    /// there).
    pub(super) fn bar_item_shown(&self, item: &BarItem) -> bool {
        match item {
            BarItem::Tag(id) => self.songs.tags().contains_key(id),
            _ => true,
        }
    }

    /// Some of the controls bar's slots, left to right, as many as fit.
    /// (The line's layout already made room for them all: this only drops
    /// what a section has no room for at all, with half a button to spare,
    /// as widgets snapped to whole pixels can run a point or two past what
    /// was worked out.)
    pub(super) fn controls_slots_ui(&mut self, ui: &mut egui::Ui, view: &EngineView, slots: &[Slot], shapes: Shapes) {
        for slot in slots {
            if ui.available_width() < ICON_BUTTON_WIDTH / 2.0 {
                break;
            }
            match slot {
                Slot::Item(item) => self.controls_item_ui(ui, view, item, shapes),
                Slot::More(items) => self.controls_more_ui(ui, items),
            }
        }
    }

    /// The ⋯ button the star and tag buttons fold into when the bar's too
    /// narrow: a list of them above it, each toggling its tag on the song
    /// playing.
    pub(super) fn controls_more_ui(&mut self, ui: &mut egui::Ui, items: &[BarItem]) {
        let song = self.current_song.clone().filter(|p| self.songs.get(p).is_some());
        let button = ui.add(icon_button("dots-three")).on_hover_text("More buttons");
        let mut clicked = None;
        bar_popup(&button, |ui| {
            ui.set_min_width(160.0);
            ui.with_layout(egui::Layout::top_down_justified(egui::Align::Min), |ui| {
                for item in items {
                    let id = match item {
                        BarItem::Star => song_store::STARRED,
                        BarItem::Tag(id) => id.as_str(),
                        _ => continue,
                    };
                    let Some(toggle) = self.tag_toggle(id) else { continue };
                    let name = if id == song_store::STARRED { "Starred".to_string() } else { toggle.label.clone() };
                    let text = format!("{}  {name}", icon(&toggle.icon));
                    if ui.add_enabled(song.is_some(), egui::Button::selectable(toggle.on, text)).on_disabled_hover_text("Play a song from your library to star or tag it").clicked() {
                        clicked = Some(toggle);
                    }
                }
            });
        });
        if let Some(toggle) = clicked
            && let Some(song) = song
        {
            self.tag_songs(tags::TagAction::Set(toggle.tag, !toggle.on), &[song]);
        }
    }

    /// One of the controls bar's items.
    pub(super) fn controls_item_ui(&mut self, ui: &mut egui::Ui, view: &EngineView, item: &BarItem, shapes: Shapes) {
        // (Stop, previous and play have nothing to act on without a song.)
        let has_song = view.has_midi || view.has_audio_file;
        let no_song = "Nothing to play yet: pick a song";
        match item {
            BarItem::Middle | BarItem::Right => {}
            BarItem::Devices => self.devices_button_ui(ui, view),
            BarItem::Shuffle => {
                let hover = shuffle_text(self.shuffle);
                if ui.add(icon_button("shuffle").selected(self.shuffle)).on_hover_text(hover).clicked() {
                    self.set_shuffle(!self.shuffle);
                    toast::show(ui.ctx(), shuffle_text(self.shuffle));
                }
            }
            BarItem::Stop => {
                if ui.add_enabled(has_song, icon_button("stop")).on_hover_text("Stop").on_disabled_hover_text(no_song).clicked() {
                    self.send(AudioCommand::Seek(0.0));
                    if !view.paused {
                        self.send(AudioCommand::TogglePause);
                    }
                }
            }
            BarItem::Previous => {
                if ui
                    .add_enabled(has_song, icon_button("skip-back"))
                    .on_hover_text(format!(
                        "Previous: the previous playlist track, or this one from the start if it's been playing a few seconds{}",
                        commands::hint(ui.ctx(), Command::PreviousTrack)
                    ))
                    .on_disabled_hover_text(no_song)
                    .clicked()
                {
                    self.previous_track(view.position);
                }
            }
            BarItem::PlayPause => {
                let hover = if view.paused { "Play" } else { "Pause" };
                let hover = format!("{hover}{}", commands::hint(ui.ctx(), Command::TogglePause));
                let play = play_button(ui.ctx(), view.paused, 18.0, egui::vec2(PLAY_WIDTH, TRANSPORT_HEIGHT));
                if ui.add_enabled(has_song, play).on_hover_text(hover).on_disabled_hover_text(no_song).clicked() {
                    self.send(AudioCommand::TogglePause);
                }
            }
            BarItem::Next => {
                if ui
                    .add_enabled(self.has_next(), icon_button("skip-forward"))
                    .on_hover_text(format!(
                        "Next: the next queued song, else the next playlist track, or the next song autoplay plays{}",
                        commands::hint(ui.ctx(), Command::NextTrack)
                    ))
                    .on_disabled_hover_text("Queue songs (right-click them), or play something from a playlist or from Songs with Autoplay on, to have a next track")
                    .clicked()
                {
                    self.next_track();
                }
            }
            BarItem::Loop => {
                let (glyph, hover) = match self.loop_mode {
                    LoopMode::Off => ("repeat", "Loop: off (click: loop the playlist, or what autoplay plays)"),
                    LoopMode::All => ("repeat", "Loop: the playlist, or what autoplay plays (click: this track)"),
                    LoopMode::One => ("repeat-once", "Loop: this track (click: off)"),
                };
                if ui.add(icon_button(glyph).selected(self.loop_mode != LoopMode::Off)).on_hover_text(hover).clicked() {
                    self.set_loop_mode(self.loop_mode.cycle());
                    toast::show(ui.ctx(), loop_text(self.loop_mode));
                }
            }
            BarItem::Volume => match shapes.volume {
                Shape::Button => self.volume_button_ui(ui),
                Shape::Slider => self.volume_ui(ui),
            },
            BarItem::Speed => match shapes.speed {
                Shape::Button => self.speed_button_ui(ui, view),
                Shape::Slider => self.speed_ui(ui, view),
            },
            BarItem::Star => self.bar_toggle_ui(ui, song_store::STARRED),
            BarItem::Tag(id) => self.bar_toggle_ui(ui, id),
        }
    }

    /// A star or tag button on the controls bar: it toggles the tag `id`
    /// on the song playing; greyed out while no song from the library is.
    pub(super) fn bar_toggle_ui(&mut self, ui: &mut egui::Ui, id: &str) {
        let Some(toggle) = self.tag_toggle(id) else { return };
        let song = self.current_song.clone().filter(|p| self.songs.get(p).is_some());
        let size = egui::vec2(ICON_BUTTON_WIDTH, TRANSPORT_HEIGHT);
        let clicked = ui
            .add_enabled_ui(song.is_some(), |ui| tags::quick_toggle_button(ui, &self.songs, &toggle, 16.0, size))
            .inner
            .on_disabled_hover_text("Play a song from your library to star or tag it")
            .clicked();
        if clicked && let Some(song) = song {
            self.tag_songs(tags::TagAction::Set(toggle.tag, !toggle.on), &[song]);
        }
    }

    /// Volume: an icon, a slider (`slider_width` wide) and its value.
    pub(super) fn volume_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_sized([SPEAKER_WIDTH, TRANSPORT_HEIGHT], egui::Label::new(egui::RichText::new(icon(speaker_icon(self.volume))).size(16.0))).on_hover_text("Volume");
        if ui.add(egui::Slider::new(&mut self.volume, 0.0..=1.0).show_value(false)).changed() {
            self.send_volume();
        }
        ui.add_sized([VOLUME_TEXT_WIDTH, TRANSPORT_HEIGHT], egui::Label::new(egui::RichText::new(format!("{:.0}%", self.volume * 100.0)).weak()));
    }

    /// Volume as a button, when the bar has no room for its slider: it
    /// opens the slider upright above it, its value on top.
    pub(super) fn volume_button_ui(&mut self, ui: &mut egui::Ui) {
        let button = ui.add(icon_button(speaker_icon(self.volume))).on_hover_text(format!("Volume: {:.0}%", self.volume * 100.0));
        bar_popup(&button, |ui| {
            ui.weak(format!("{:.0}%", self.volume * 100.0));
            if ui.add(egui::Slider::new(&mut self.volume, 0.0..=1.0).vertical().show_value(false)).changed() {
                self.send_volume();
            }
        });
    }

    /// Speed as a button, when the bar has no room for its slider: it
    /// opens the slider upright above it, its value on top and a button
    /// back to 1x under it.
    pub(super) fn speed_button_ui(&mut self, ui: &mut egui::Ui, view: &EngineView) {
        let button = ui.add(icon_button("gauge").selected(view.speed != 1.0)).on_hover_text(format!("Speed: {:.2}x", view.speed));
        bar_popup(&button, |ui| {
            ui.weak(format!("{:.2}x", view.speed));
            let mut speed = view.speed;
            if ui.add(egui::Slider::new(&mut speed, MIN_SPEED..=MAX_SPEED).vertical().show_value(false)).changed() {
                self.send(AudioCommand::SetSpeed(speed));
            }
            if ui.add_enabled(view.speed != 1.0, egui::Button::new("1x")).on_hover_text("Back to normal speed").clicked() {
                self.send(AudioCommand::SetSpeed(1.0));
            }
        });
    }

    /// Speed: its icon, which puts it back to 1x, a slider and its value.
    pub(super) fn speed_ui(&mut self, ui: &mut egui::Ui, view: &EngineView) {
        if ui.add(icon_button("gauge").selected(view.speed != 1.0)).on_hover_text("Speed (click: back to 1x)").clicked() {
            self.send(AudioCommand::SetSpeed(1.0));
        }
        let mut speed = view.speed;
        if ui.add(egui::Slider::new(&mut speed, MIN_SPEED..=MAX_SPEED).show_value(false)).changed() {
            self.send(AudioCommand::SetSpeed(speed));
        }
        ui.add_sized([SPEED_TEXT_WIDTH, TRANSPORT_HEIGHT], egui::Label::new(egui::RichText::new(format!("{:.2}x", view.speed)).weak()));
    }
}

/// How wide volume and speed's sliders get in the controls bar.
pub(super) const MIN_SLIDER: f32 = 60.0;

pub(super) const MAX_SLIDER: f32 = 160.0;

/// How tall a slider is in a controls bar popup (`bar_popup`).
pub(super) const POPUP_SLIDER: f32 = 120.0;

pub(super) const VOLUME_TEXT_WIDTH: f32 = 36.0;

pub(super) const SPEED_TEXT_WIDTH: f32 = 40.0;

pub(super) const SPEAKER_WIDTH: f32 = 20.0;

/// The least room between the transport and the bar's sides.
pub(super) const SIDE_GAP: f32 = 24.0;

/// How much of the bar's first line the status line may take.
pub(super) const STATUS_SHARE: f32 = 0.4;

/// How the controls bar shows volume or speed, by the room it has.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Shape {
    /// Its slider, with its value and icon.
    Slider,
    /// A button opening the slider in a popup.
    Button,
}

/// How wide a `Shape`'s slider form is `slider`; its button form an
/// icon button.
pub(super) fn shape_width(shape: Shape, slider: f32) -> f32 {
    match shape {
        Shape::Slider => slider,
        Shape::Button => ICON_BUTTON_WIDTH,
    }
}

/// A place on the controls bar's line: one of its items, or the ⋯
/// button the star and tag buttons fold into (`App::controls_more_ui`).
#[derive(Clone, PartialEq)]
pub(super) enum Slot {
    Item(BarItem),
    More(Vec<BarItem>),
}

impl Slot {
    /// How wide it is, as a button (sliders aside).
    fn width(&self) -> f32 {
        match self {
            Slot::Item(BarItem::Middle | BarItem::Right) => 0.0,
            Slot::Item(BarItem::PlayPause) => PLAY_WIDTH,
            _ => ICON_BUTTON_WIDTH,
        }
    }
}

/// How the controls bar shows volume and speed this frame.
#[derive(Clone, Copy)]
pub(super) struct Shapes {
    volume: Shape,
    speed: Shape,
}

/// A popup right above a controls bar button (as a dropdown, but up),
/// holding one of its sliders upright, `POPUP_SLIDER` tall, down its
/// middle.
pub(super) fn bar_popup(button: &egui::Response, add: impl FnOnce(&mut egui::Ui)) {
    egui::Popup::from_toggle_button_response(button)
        .align(egui::RectAlign::TOP)
        .align_alternatives(&[])
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .width(button.rect.width())
        .show(|ui| {
            ui.spacing_mut().slider_width = POPUP_SLIDER;
            ui.with_layout(egui::Layout::top_down(egui::Align::Center), add);
        });
}

/// The speaker icon for `volume`.
pub(super) fn speaker_icon(volume: f32) -> &'static str {
    match volume {
        v if v <= 0.0 => "speaker-x",
        v if v < 0.5 => "speaker-low",
        _ => "speaker-high",
    }
}

/// How wide volume is (`App::volume_ui`) with a `slider` wide slider,
/// `gap` between each of its widgets.
pub(super) fn volume_width(gap: f32, slider: f32) -> f32 {
    VOLUME_TEXT_WIDTH + slider + SPEAKER_WIDTH + 2.0 * gap
}

/// How wide speed is (`App::speed_ui`), as `volume_width`.
pub(super) fn speed_width(gap: f32, slider: f32) -> f32 {
    SPEED_TEXT_WIDTH + slider + ICON_BUTTON_WIDTH + 2.0 * gap
}

/// How wide an `icon_button` is.
pub(super) const ICON_BUTTON_WIDTH: f32 = 28.0;

pub(super) const PLAY_WIDTH: f32 = 44.0;

pub(super) const TRANSPORT_HEIGHT: f32 = 28.0;

/// The handle on the top edge of a section without a tab bar (in the
/// margin over `ui`, its body), where it's dragged from: a short line,
/// brighter under the pointer.
pub(super) fn drag_handle(ui: &egui::Ui) {
    let content = ui.max_rect();
    let margin = crate::theme::DOCK_HANDLE;
    let strip = egui::Rect::from_min_max(egui::pos2(content.left(), content.top() - margin), egui::pos2(content.right(), content.top()));
    let weak = ui.visuals().weak_text_color();
    let color = if ui.rect_contains_pointer(strip) { weak } else { weak.gamma_multiply(0.4) };
    let line = egui::Rect::from_center_size(strip.center(), egui::vec2(32.0, 3.0));
    ui.ctx().layer_painter(ui.layer_id()).rect_filled(line, 1.5, color);
}

/// Square off the top left corner of the body around `ui` (its top margin
/// `top` tall), under its tab.
pub(super) fn square_corner(ui: &egui::Ui, top: f32) {
    let content = ui.max_rect();
    let corner = egui::pos2(content.left() - f32::from(crate::theme::DOCK_MARGIN), content.top() - top);
    let patch = egui::Rect::from_min_size(corner, egui::Vec2::splat(8.0));
    ui.ctx().layer_painter(ui.layer_id()).rect_filled(patch, 0.0, ui.visuals().window_fill);
}

/// The play/pause button: Play while `paused`, else Pause in the look of
/// the playing row (`select_list::paint_row`): tinted and outlined in the
/// theme's playing color.
pub(crate) fn play_button(ctx: &egui::Context, paused: bool, icon_size: f32, size: egui::Vec2) -> egui::Button<'static> {
    if paused {
        return egui::Button::new(egui::RichText::new(icon("play")).size(icon_size)).min_size(size);
    }
    let color = crate::theme::playing_color(ctx);
    egui::Button::new(egui::RichText::new(icon("pause")).size(icon_size).color(color))
        .min_size(size)
        .fill(color.gamma_multiply(0.15))
        .stroke(egui::Stroke::new(1.0, color))
}

/// What Loop is set to, for the toast its button shows.
pub(super) fn loop_text(mode: LoopMode) -> &'static str {
    match mode {
        LoopMode::Off => "Loop: off",
        LoopMode::All => "Loop: all",
        LoopMode::One => "Loop: this track",
    }
}

pub(super) fn shuffle_text(on: bool) -> &'static str {
    if on { "Shuffle: on" } else { "Shuffle: off" }
}

/// The seek bar over a song `length` seconds long, in `accent` (the
/// tempo's color) when there is one.
pub(super) fn seek_bar_ui(ui: &mut egui::Ui, position: &mut f64, length: f64, accent: Option<egui::Color32>) -> egui::Response {
    ui.scope(|ui| {
        let rail_radius = (ui.spacing().slider_rail_height / 2.0) as u8;
        let visuals = ui.visuals_mut();
        // egui runs the filled part past the handle by the rail's corner
        // radius, which a very round theme makes far longer than the rail
        // can be round: no rounder than the rail.
        let corners = &mut visuals.widgets.inactive.corner_radius;
        *corners = egui::CornerRadius::same(corners.nw.min(rail_radius));
        if let Some(accent) = accent {
            visuals.selection.bg_fill = accent;
            // (The handle ringed in black or white, whichever stands out on it.)
            for state in [&mut visuals.widgets.inactive, &mut visuals.widgets.hovered, &mut visuals.widgets.active] {
                state.bg_fill = accent;
                state.fg_stroke = egui::Stroke::new(1.5, on_color(accent));
            }
        }
        ui.add(egui::Slider::new(position, 0.0..=length.max(0.001)).show_value(false).trailing_fill(true))
    })
    .inner
}

/// The accent color for a tempo: slow songs blue, then cyan, green, yellow
/// and coral, fast ones pink, blended between (palette colors).
pub(super) fn tempo_color(bpm: f64) -> [f32; 3] {
    const STOPS: [(f64, u8); 6] = [(60.0, 0), (90.0, 5), (115.0, 2), (135.0, 3), (160.0, 1), (190.0, 6)];
    let rgb = |i: u8| lua_visualizer::default_channel_color(i).map(f32::from);
    let Some(after) = STOPS.iter().position(|&(at, _)| bpm < at) else { return rgb(STOPS[STOPS.len() - 1].1) };
    if after == 0 {
        return rgb(STOPS[0].1);
    }
    let ((a, from), (b, to)) = (STOPS[after - 1], STOPS[after]);
    let t = ((bpm - a) / (b - a)) as f32;
    let (from, to) = (rgb(from), rgb(to));
    [0, 1, 2].map(|i| from[i] + (to[i] - from[i]) * t)
}

/// The status line's color, by what it says: red for something that
/// failed ("Couldn't ...", "Failed to ..."), green for something done
/// ("Saved ...", "Installed ..."), none (grey) for anything else.
pub(super) fn status_color(visuals: &egui::Visuals, status: &str) -> Option<egui::Color32> {
    match status_outcome(status)? {
        false => Some(visuals.error_fg_color),
        true => {
            // (The palette's green, darker on a light theme so it reads.)
            let green = palette_color(2);
            Some(if visuals.dark_mode { green } else { green.gamma_multiply(0.55).to_opaque() })
        }
    }
}

/// Whether a status says something failed (`Some(false)`), was done
/// (`Some(true)`), or neither, by its wording.
pub(super) fn status_outcome(status: &str) -> Option<bool> {
    const FAILED: [&str; 6] = ["Couldn't", "Failed", "Can't", "Didn't", "Error", "Not "];
    const DONE: [&str; 22] = [
        "Saved", "Sent", "Installed", "Downloaded", "Published", "Deleted", "Updated", "Restored", "Renamed", "Rendered",
        "Reported", "Replaced", "Reloaded", "Formatted", "Imported", "Exported", "Disputed", "Added", "Removed",
        "Recorded", "Soundfont loaded", "Script applied",
    ];
    if FAILED.iter().any(|w| status.starts_with(w)) || status.contains(" not loaded") || status.contains("failed") {
        return Some(false);
    }
    DONE.iter().any(|w| status.starts_with(w)).then_some(true)
}

/// The status line: a history button (with a red dot while a failure that
/// came up hasn't been looked at) and the latest `status`, red when
/// something failed, green when something got done. The button opens the
/// recent messages from the log (`entries`, timed on `now`'s clock) in a
/// popup above it, which clears the dot. True when "Open the full log..."
/// was clicked.
pub(super) fn status_line_ui(
    ui: &mut egui::Ui,
    status: &str,
    unseen_failure: &mut bool,
    now: impl FnOnce() -> f64,
    entries: impl FnOnce() -> Vec<applog::Entry>,
) -> bool {
    let button = ui
        .add(icon_button("clock-counter-clockwise"))
        .on_hover_text(if *unseen_failure {
            "Something failed: the recent messages"
        } else {
            "The recent messages"
        });
    if *unseen_failure {
        let corner = button.rect.right_top() + egui::vec2(-3.0, 3.0);
        ui.painter().circle_filled(corner, 3.5, ui.visuals().error_fg_color);
    }
    let mut open_log = false;
    let shown = egui::Popup::from_toggle_button_response(&button)
        .show(|ui| {
            ui.set_max_width(480.0);
            let now = now();
            let recent: Vec<applog::Entry> = entries()
                .into_iter()
                .rev()
                .filter(|e| e.target == STATUS_TARGET && !e.message.is_empty())
                .take(STATUS_HISTORY)
                .collect();
            ui.strong("Recent messages");
            if recent.is_empty() {
                ui.weak("Nothing yet.");
            }
            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                for entry in &recent {
                    ui.horizontal_wrapped(|ui| {
                        match status_color(ui.visuals(), &entry.message) {
                            Some(color) => ui.colored_label(color, &entry.message),
                            None => ui.label(&entry.message),
                        };
                        ui.weak(editor::ago_text((now - entry.secs).max(0.0) as u64));
                    });
                }
            });
            ui.separator();
            if ui.button("Open the full log...").on_hover_text("Help > Log, showing just these").clicked() {
                open_log = true;
                ui.close();
            }
        })
        .is_some();
    if shown {
        *unseen_failure = false;
    }
    let text = match status_color(ui.visuals(), status) {
        Some(color) => egui::RichText::new(status).color(color),
        None => egui::RichText::new(status).weak(),
    };
    ui.add(egui::Label::new(text).truncate());
    open_log
}

/// The log target status messages go under (the status line's history).
pub(super) const STATUS_TARGET: &str = "synththing::status";

/// How many of the status line's past messages its popup shows.
pub(super) const STATUS_HISTORY: usize = 50;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::applog;

    /// The status history opens from its button, clears the red dot, and
    /// stays open on the frames after (the dot going away mustn't lose it).
    #[test]
    fn status_history_opens_and_stays_open() {
        let ctx = egui::Context::default();
        let entries = || {
            vec![applog::Entry {
                secs: 1.0,
                level: log::Level::Info,
                target: STATUS_TARGET.into(),
                message: "Couldn't save: denied".into(),
            }]
        };
        let button = std::cell::Cell::new(egui::Rect::NOTHING);
        let frame = |events: Vec<egui::Event>, unseen: &mut bool| {
            let input = egui::RawInput {
                events,
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                ui.horizontal(|ui| {
                    let start = ui.cursor().min;
                    status_line_ui(ui, "Couldn't save: denied", unseen, || 125.0, entries);
                    button.set(egui::Rect::from_min_size(start, egui::vec2(12.0, 12.0)));
                });
            });
            output.textures_delta.clear(); // (no renderer here to hand them to)
            egui::Popup::is_any_open(&ctx)
        };
        let mut unseen = true;
        assert!(!frame(vec![], &mut unseen));
        let at = button.get().center();
        let press = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(vec![egui::Event::PointerMoved(at), press(true)], &mut unseen);
        frame(vec![press(false)], &mut unseen);
        let mut open = 0;
        for _ in 0..5 {
            open += usize::from(frame(vec![], &mut unseen));
        }
        assert_eq!(open, 5, "open, and still open");
        assert!(!unseen, "seen");
    }

    /// Slow songs blue, fast ones pink, blended between; out of range,
    /// the nearest end.
    #[test]
    fn tempos_have_colors() {
        let rgb = |i: u8| lua_visualizer::default_channel_color(i).map(f32::from);
        assert_eq!(tempo_color(40.0), rgb(0));
        assert_eq!(tempo_color(60.0), rgb(0));
        assert_eq!(tempo_color(115.0), rgb(2));
        assert_eq!(tempo_color(250.0), rgb(6));
        let between = tempo_color(102.5);
        let (cyan, green) = (rgb(5), rgb(2));
        for i in 0..3 {
            assert!((between[i] - (cyan[i] + green[i]) / 2.0).abs() < 0.01);
        }
    }

    #[test]
    fn statuses_are_sorted_by_wording() {
        assert_eq!(status_outcome("Couldn't save preferences: denied"), Some(false));
        assert_eq!(status_outcome("Failed to create script: exists"), Some(false));
        assert_eq!(status_outcome("Soundfont not loaded"), Some(false));
        assert_eq!(status_outcome("Published \"Disco\" to synththing.p51.nl."), Some(true));
        assert_eq!(status_outcome("Soundfont loaded: GeneralUser-GS"), Some(true));
        assert_eq!(status_outcome("Click a song to play it, a soundfont to load it."), None);
        assert_eq!(status_outcome("Loading Canon in D..."), None);
    }
}
