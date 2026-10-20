//! Album art: the picture embedded in a song's file, read and scaled on a
//! thread of its own and kept as a texture for each size it's drawn at
//! (the most recently drawn ones, up to `KEPT`); an album without one (or
//! of MIDI files) gets a tile of its own color with its initials, from its
//! name, the way branch builds get their color.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};

use eframe::egui;

/// The pixel sizes art is scaled to: a tile takes the next one up.
const SIZES: [u32; 4] = [64, 128, 256, 512];
/// How many textures are kept, the ones drawn longest ago dropped first.
const KEPT: usize = 300;
/// How many requests wait at most: scrolling past art drops the oldest.
const WAITING: usize = 64;

type Request = (PathBuf, u32);

enum Slot {
    Loading,
    /// The file has no picture (or it can't be read).
    Missing,
    Ready(egui::TextureHandle),
}

struct Kept {
    slot: Slot,
    /// The frame it was last drawn in.
    used: u64,
}

/// What the reading thread works through: the newest request first.
#[derive(Default)]
struct Queue {
    waiting: Mutex<Vec<Request>>,
    ready: Condvar,
}

/// How many letters a tile without art shows, unless set otherwise.
pub const DEFAULT_LETTERS: u8 = 2;

#[derive(Default)]
pub struct AlbumArt {
    /// How many letters (1 to 4) tiles without art show (`Config::tile_letters`).
    pub letters: u8,
    kept: HashMap<Request, Kept>,
    queue: Arc<Queue>,
    done: Option<mpsc::Receiver<(Request, Option<egui::ColorImage>)>>,
    frame: u64,
}

impl AlbumArt {
    /// Take in the art read since the last frame, and let go of what
    /// hasn't been drawn in a while. Once a frame, before any `tile`.
    pub fn frame(&mut self, ctx: &egui::Context) {
        self.frame += 1;
        if let Some(done) = &self.done {
            for (request, image) in done.try_iter() {
                let slot = match image {
                    Some(image) => {
                        let name = format!("album art {}@{}", request.0.display(), request.1);
                        Slot::Ready(ctx.load_texture(name, image, egui::TextureOptions::LINEAR))
                    }
                    None => Slot::Missing,
                };
                if let Some(kept) = self.kept.get_mut(&request) {
                    kept.slot = slot;
                }
            }
        }
        if self.kept.len() > KEPT {
            let mut by_age: Vec<(u64, Request)> = self.kept.iter().map(|(r, k)| (k.used, r.clone())).collect();
            by_age.sort_unstable_by_key(|(used, _)| *used);
            for (_, request) in by_age.into_iter().take(self.kept.len() - KEPT) {
                self.kept.remove(&request);
            }
        }
    }

    /// Draw the art of the album `name`, taken from the song at `from`
    /// (none: the album's all MIDI), filling `rect`.
    pub fn tile(&mut self, ui: &egui::Ui, rect: egui::Rect, from: Option<&Path>, name: &str) {
        let radius = 4.0;
        if let Some(from) = from {
            let pixels = rect.width().max(rect.height()) * ui.ctx().pixels_per_point();
            let size = SIZES.into_iter().find(|&s| s as f32 >= pixels).unwrap_or(SIZES[SIZES.len() - 1]);
            let request = (from.to_path_buf(), size);
            let frame = self.frame;
            let kept = match self.kept.get_mut(&request) {
                Some(kept) => kept,
                None => {
                    self.ask(ui.ctx(), request.clone());
                    self.kept.entry(request).or_insert(Kept { slot: Slot::Loading, used: frame })
                }
            };
            kept.used = frame;
            if let Slot::Ready(texture) = &kept.slot {
                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                egui::Image::new((texture.id(), rect.size())).uv(uv).corner_radius(radius).paint_at(ui, rect);
                return;
            }
        }
        generated_tile(ui.painter(), rect, name, radius, self.letters);
    }

    fn ask(&mut self, ctx: &egui::Context, request: Request) {
        if self.done.is_none() {
            let (send, done) = mpsc::channel();
            self.done = Some(done);
            let queue = self.queue.clone();
            let ctx = ctx.clone();
            let started = std::thread::Builder::new().name("album art".into()).spawn(move || read_art_forever(&queue, &send, &ctx));
            if let Err(e) = started {
                log::warn!("couldn't start reading album art: {e}");
            }
        }
        let mut waiting = self.queue.waiting.lock().unwrap_or_else(|e| e.into_inner());
        waiting.push(request);
        if waiting.len() > WAITING {
            // (They're asked again if they're scrolled back to.)
            let over = waiting.len() - WAITING;
            let dropped: Vec<Request> = waiting.drain(..over).collect();
            for request in dropped {
                self.kept.remove(&request);
            }
        }
        self.queue.ready.notify_one();
    }
}

fn read_art_forever(queue: &Queue, send: &mpsc::Sender<(Request, Option<egui::ColorImage>)>, ctx: &egui::Context) {
    loop {
        let request = {
            let mut waiting = queue.waiting.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(request) = waiting.pop() {
                    break request;
                }
                waiting = queue.ready.wait(waiting).unwrap_or_else(|e| e.into_inner());
            }
        };
        let image = embedded_picture(&request.0).and_then(|bytes| scaled(&bytes, request.1));
        if send.send((request, image)).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

/// The picture in the file at `path`'s tags: its front cover, else the
/// first one.
fn embedded_picture(path: &Path) -> Option<Box<[u8]>> {
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::{MetadataOptions, StandardVisualKey};
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path).ok()?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut probed = symphonia::default::get_probe().format(&hint, stream, &FormatOptions::default(), &MetadataOptions::default()).ok()?;
    let mut visuals = Vec::new();
    if let Some(metadata) = probed.metadata.get()
        && let Some(revision) = metadata.current()
    {
        visuals.extend(revision.visuals().iter().cloned());
    }
    if let Some(revision) = probed.format.metadata().current() {
        visuals.extend(revision.visuals().iter().cloned());
    }
    let cover = visuals.iter().position(|v| v.usage == Some(StandardVisualKey::FrontCover)).unwrap_or(0);
    (!visuals.is_empty()).then(|| visuals.swap_remove(cover).data)
}

/// `bytes` (a JPEG or PNG) as a `size`-pixel square, cropped to fill it.
fn scaled(bytes: &[u8], size: u32) -> Option<egui::ColorImage> {
    let image = image::load_from_memory(bytes).ok()?;
    let image = image.resize_to_fill(size, size, image::imageops::FilterType::Triangle).to_rgba8();
    let dimensions = [image.width() as usize, image.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(dimensions, image.as_raw()))
}

/// An album's color, from its name: any hue, soft enough for white text.
fn color_of(name: &str) -> egui::Color32 {
    // FNV-1a, as branch builds' colors are made.
    let hash = name.to_lowercase().bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    let hue = (hash % 360) as f32 / 360.0;
    egui::ecolor::Hsva::new(hue, 0.45, 0.55, 1.0).into()
}

/// Up to `n` letters for an album: its first words' first letters.
fn initials(name: &str, n: usize) -> String {
    name.split_whitespace().filter_map(|w| w.chars().find(|c| c.is_alphanumeric())).take(n).flat_map(char::to_uppercase).collect()
}

/// The tile an album without art gets: its color, darker to the bottom,
/// with up to `letters` of its initials, in a row (one to three) or a
/// square of two over two (four).
fn generated_tile(painter: &egui::Painter, rect: egui::Rect, name: &str, radius: f32, letters: u8) {
    let color = color_of(name);
    painter.rect_filled(rect, radius, color);
    let lower = egui::Rect::from_min_max(egui::pos2(rect.left(), rect.center().y), rect.max);
    let shade = egui::CornerRadius { nw: 0, ne: 0, sw: radius as u8, se: radius as u8 };
    painter.rect_filled(lower, shade, egui::Color32::from_black_alpha(40));
    let text = egui::Color32::from_white_alpha(220);
    let letters = letters.clamp(1, 4) as usize;
    let initials = initials(name, letters);
    if letters == 4 && initials.chars().count() > 2 {
        let chars: Vec<char> = initials.chars().collect();
        let font = egui::FontId::proportional(rect.height() * 0.26);
        let lines = [chars[..2].iter().collect::<String>(), chars[2..].iter().collect::<String>()];
        let gap = rect.height() * 0.14;
        for (i, line) in lines.iter().enumerate() {
            let y = rect.center().y + if i == 0 { -gap } else { gap };
            painter.text(egui::pos2(rect.center().x, y), egui::Align2::CENTER_CENTER, line, font.clone(), text);
        }
        return;
    }
    let size = match letters {
        1 => 0.5,
        2 => 0.32,
        _ => 0.24,
    };
    painter.text(rect.center(), egui::Align2::CENTER_CENTER, initials, egui::FontId::proportional(rect.height() * size), text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_skip_punctuation() {
        assert_eq!(initials("Tribute To Nujabes", 2), "TT");
        assert_eq!(initials("Tribute To Nujabes", 4), "TTN", "as many as there are words");
        assert_eq!(initials("\"NuJams\"", 2), "N");
        assert_eq!(initials("", 1), "");
    }

    #[test]
    fn an_albums_color_ignores_case() {
        assert_eq!(color_of("Modal Soul"), color_of("modal soul"));
    }
}
