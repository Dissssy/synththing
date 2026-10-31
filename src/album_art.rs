//! Album art: the picture embedded in a song's file, else its release's
//! front cover from the Cover Art Archive (once looked up, `lookup`; kept
//! in the config folder's `art`), read and scaled on threads of their own
//! and kept as a texture for each size it's drawn at (the most recently
//! drawn ones, up to `KEPT`); an album without one (or of MIDI files)
//! gets a tile of its own color with its initials, from its name, the way
//! branch builds get their color.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime};

use eframe::egui;

use crate::song_store::Art;

/// The pixel sizes art is scaled to: a tile takes the next one up.
const SIZES: [u32; 4] = [64, 128, 256, 512];
/// How many textures are kept, the ones drawn longest ago dropped first.
const KEPT: usize = 300;
/// How many requests wait at most: scrolling past art drops the oldest.
const WAITING: usize = 64;

/// How many threads read art: one can wait on a download while another
/// reads files.
const READERS: usize = 2;
/// How long a release found without a cover waits to be asked again.
const NO_COVER_KEPT: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Where an album's art comes from: a song's file, then its release.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Source {
    pub file: Option<PathBuf>,
    /// The release (and its group) at MusicBrainz.
    pub release: Option<(String, Option<String>)>,
    /// The art chosen for it, over all that (Choose art).
    pub chosen: Option<Art>,
    /// A picture the user picked (`images.rs`), over everything.
    pub image: Option<PathBuf>,
}

impl Source {
    fn is_empty(&self) -> bool {
        self.file.is_none() && self.release.is_none() && self.chosen.is_none() && self.image.is_none()
    }

    /// A song's art: its file (when it's here), then its release and the
    /// art chosen for it, if it's in the library.
    pub fn of_song(file: Option<PathBuf>, song: Option<&crate::song_store::Song>) -> Self {
        let Some(song) = song else { return Self { file, ..Self::default() } };
        Self {
            file,
            release: crate::lookup::release_of(song).map(|(release, group)| (release.to_string(), group.map(str::to_string))),
            chosen: song.online.as_ref().and_then(|o| o.art.clone()),
            image: None,
        }
    }

    /// Only the picture the user picked, kept as `key` (`images.rs`).
    pub fn image(key: &str) -> Self {
        Self { image: crate::images::path(key).ok(), ..Self::default() }
    }

    /// Only the picture at `url` (Choose art's choices).
    pub fn url(url: &str) -> Self {
        Self { chosen: Some(Art::Url(url.to_string())), ..Self::default() }
    }
}

/// The playing song's art for visualizer scripts (`album_art()`): decoded,
/// no bigger than `SCRIPT_ART` across, and the color it's mostly made of.
pub struct ScriptArt {
    pub image: image::RgbaImage,
    /// `0xFFRRGGBB`: the picture's most colorful common color, made bright
    /// enough to draw with ("album colors").
    pub color: u32,
}

/// The largest side of art handed to scripts.
const SCRIPT_ART: u32 = 512;

/// The art `source` gives, for scripts (see `ScriptArt`).
pub fn for_script(source: &Source, online: bool) -> Option<ScriptArt> {
    let bytes = picture(source, online)?;
    let mut image = image::load_from_memory(&bytes).ok()?.into_rgba8();
    if image.width().max(image.height()) > SCRIPT_ART {
        image = image::DynamicImage::ImageRgba8(image).thumbnail(SCRIPT_ART, SCRIPT_ART).into_rgba8();
    }
    let color = accent(&image);
    Some(ScriptArt { image, color })
}

/// The color a picture's mostly made of, favoring colorful pixels over
/// greys: the average of the hue (in twelve slices) that has the most
/// color, brightened so it shows on a dark background. (An average of the
/// whole picture would mix its colors to grey.)
fn accent(image: &image::RgbaImage) -> u32 {
    const SLICES: usize = 12;
    let mut slices = [([0.0f64; 3], 0.0f64); SLICES];
    for pixel in image.pixels().step_by(3) {
        let [r, g, b, _] = pixel.0.map(f64::from);
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        let chroma = max - min;
        let weight = (chroma / 255.0).powi(2) * (max / 255.0) + 1e-6;
        let hue = if chroma == 0.0 {
            0.0
        } else if max == r {
            ((g - b) / chroma).rem_euclid(6.0)
        } else if max == g {
            (b - r) / chroma + 2.0
        } else {
            (r - g) / chroma + 4.0
        };
        let slice = &mut slices[((hue / 6.0 * SLICES as f64) as usize).min(SLICES - 1)];
        for (total, value) in slice.0.iter_mut().zip([r, g, b]) {
            *total += value * weight;
        }
        slice.1 += weight;
    }
    let (sum, weight) = slices.into_iter().fold(([0.0; 3], 0.0), |best, slice| if slice.1 > best.1 { slice } else { best });
    let mut color = sum.map(|total| total / weight.max(1e-12));
    let brightest = color.iter().copied().fold(1.0, f64::max);
    if brightest < 200.0 {
        color = color.map(|c| c * 200.0 / brightest);
    }
    let [r, g, b] = color.map(|c| c.round().clamp(0.0, 255.0) as u32);
    0xFF00_0000 | (r << 16) | (g << 8) | b
}

type Request = (Source, u32);

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
    /// Covers not kept yet may be downloaded (`AlbumArt::online`).
    online: AtomicBool,
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
    /// hasn't been drawn in a while. Once a frame, before any `tile`;
    /// `online`: covers may be downloaded.
    pub fn frame(&mut self, ctx: &egui::Context, online: bool) {
        self.frame += 1;
        if self.queue.online.swap(online, Ordering::Relaxed) != online {
            // (Those found missing may be there now.)
            self.kept.retain(|_, k| !matches!(k.slot, Slot::Missing));
        }
        if let Some(done) = &self.done {
            for (request, image) in done.try_iter() {
                let slot = match image {
                    Some(image) => {
                        let name = format!("album art {:?}@{}", request.0, request.1);
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

    /// Look again for the pictures picked (`Source::image`,
    /// `Art::Image`) found missing: one came from the other devices.
    pub fn images_arrived(&mut self) {
        self.kept.retain(|(source, _), k| (source.image.is_none() && !matches!(source.chosen, Some(Art::Image(_)))) || !matches!(k.slot, Slot::Missing));
    }

    /// Draw the art of the album `name`, taken `from` a song of it and
    /// its release (neither: the album's all MIDI), filling `rect`.
    pub fn tile(&mut self, ui: &egui::Ui, rect: egui::Rect, from: &Source, name: &str) {
        let (radius, letters) = (4.0, self.letters);
        match self.slot(ui.ctx(), rect, from) {
            Some(Slot::Ready(texture)) => paint(ui, texture, rect, radius),
            _ => generated_tile(ui.painter(), rect, name, radius, letters),
        }
    }

    /// Draw the art `from` gives filling `rect`, if it gives any: whether
    /// it does (or might: while it's being read, nothing's drawn).
    pub fn picture(&mut self, ui: &egui::Ui, rect: egui::Rect, from: &Source) -> bool {
        match self.slot(ui.ctx(), rect, from) {
            Some(Slot::Ready(texture)) => {
                paint(ui, texture, rect, 4.0);
                true
            }
            Some(Slot::Loading) => true,
            Some(Slot::Missing) | None => false,
        }
    }

    /// Where the art `from` gives, at `rect`'s size, stands (asking for it
    /// if it hasn't been yet); `None` if `from` gives nothing to look in.
    fn slot(&mut self, ctx: &egui::Context, rect: egui::Rect, from: &Source) -> Option<&Slot> {
        if from.is_empty() {
            return None;
        }
        let pixels = rect.width().max(rect.height()) * ctx.pixels_per_point();
        let size = SIZES.into_iter().find(|&s| s as f32 >= pixels).unwrap_or(SIZES[SIZES.len() - 1]);
        let request = (from.clone(), size);
        let frame = self.frame;
        if !self.kept.contains_key(&request) {
            self.ask(ctx, request.clone());
        }
        let kept = self.kept.entry(request).or_insert(Kept { slot: Slot::Loading, used: frame });
        kept.used = frame;
        Some(&kept.slot)
    }

    fn ask(&mut self, ctx: &egui::Context, request: Request) {
        if self.done.is_none() {
            let (send, done) = mpsc::channel();
            self.done = Some(done);
            for _ in 0..READERS {
                let (queue, send, ctx) = (self.queue.clone(), send.clone(), ctx.clone());
                let started = std::thread::Builder::new().name("album art".into()).spawn(move || read_art_forever(&queue, &send, &ctx));
                if let Err(e) = started {
                    log::warn!("couldn't start reading album art: {e}");
                }
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

/// Paint `texture` filling `rect`, its corners rounded by `radius`.
fn paint(ui: &egui::Ui, texture: &egui::TextureHandle, rect: egui::Rect, radius: f32) {
    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    egui::Image::new((texture.id(), rect.size())).uv(uv).corner_radius(radius).paint_at(ui, rect);
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
        let (source, size) = &request;
        let image = picture(source, queue.online.load(Ordering::Relaxed)).and_then(|bytes| scaled(&bytes, *size));
        if send.send((request, image)).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

/// The picture (a JPEG or PNG, as kept) `source` gives, if any: covers
/// not kept yet are downloaded only if `online`.
pub fn picture(source: &Source, online: bool) -> Option<Box<[u8]>> {
    if let Some(image) = &source.image {
        return std::fs::read(image).ok().map(Vec::into_boxed_slice);
    }
    match &source.chosen {
        // (Asked for by the user: downloaded whatever `online` says, if
        // online lookups are on in this build.)
        Some(Art::Url(url)) => picture_at(url),
        Some(Art::Files) => source.file.as_deref().and_then(embedded_picture),
        Some(Art::Image(key)) => crate::images::path(key).ok().and_then(|p| std::fs::read(p).ok()).map(Vec::into_boxed_slice),
        None => source
            .file
            .as_deref()
            .and_then(embedded_picture)
            .or_else(|| source.release.as_ref().and_then(|(release, group)| cover(release, group.as_deref(), online))),
    }
}

/// The picture in the file at `path`'s tags: its front cover, else the
/// first one.
pub(crate) fn embedded_picture(path: &Path) -> Option<Box<[u8]>> {
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

/// Where covers are kept, by release.
fn covers_dir() -> Option<PathBuf> {
    Some(crate::config::config_dir().ok()?.join("art"))
}

/// The front cover of `release` (else of its `group`) from the Cover Art
/// Archive: kept, or downloaded if `online` (and kept, or noted missing).
fn cover(release: &str, group: Option<&str>, online: bool) -> Option<Box<[u8]>> {
    let dir = covers_dir()?;
    let kept = dir.join(format!("{release}.jpg"));
    if let Ok(bytes) = std::fs::read(&kept) {
        return Some(bytes.into());
    }
    let none = dir.join(format!("{release}.none"));
    let noted = std::fs::metadata(&none).and_then(|m| m.modified()).ok();
    if !online || noted.is_some_and(|at| SystemTime::now().duration_since(at).unwrap_or_default() < NO_COVER_KEPT) {
        return None;
    }
    let urls = std::iter::once(format!("https://coverartarchive.org/release/{release}/front-500"))
        .chain(group.map(|g| format!("https://coverartarchive.org/release-group/{g}/front-500")));
    let mut failed = false;
    for url in urls {
        match download(&url) {
            Ok(Some(bytes)) => {
                if let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&kept, &bytes)) {
                    log::warn!("couldn't keep the cover of {release}: {e}");
                }
                return Some(bytes.into());
            }
            Ok(None) => {}
            Err(e) => {
                log::warn!("couldn't download {url}: {e}");
                failed = true;
            }
        }
    }
    // (Not found anywhere: noted, so it isn't asked again for a while; a
    // failed download is tried again next time it's drawn.)
    if !failed && let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&none, b"")) {
        log::warn!("couldn't note {release} has no cover: {e}");
    }
    None
}

/// The picture at `url`: kept (by a hash of it), or downloaded and kept.
fn picture_at(url: &str) -> Option<Box<[u8]>> {
    use sha2::Digest;
    let dir = covers_dir()?;
    let kept = dir.join(format!("{}.img", crate::library::hex(&sha2::Sha256::digest(url.as_bytes())[..16])));
    if let Ok(bytes) = std::fs::read(&kept) {
        return Some(bytes.into());
    }
    if !crate::build_info::online(crate::build_info::Online::Lookup) {
        return None;
    }
    match download(url) {
        Ok(Some(bytes)) => {
            if let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&kept, &bytes)) {
                log::warn!("couldn't keep {url}: {e}");
            }
            Some(bytes.into())
        }
        Ok(None) => None,
        Err(e) => {
            log::warn!("couldn't download {url}: {e}");
            None
        }
    }
}

/// The image at `url` (`None`: there's none, a 404).
pub(crate) fn download(url: &str) -> Result<Option<Vec<u8>>, ureq::Error> {
    let agent = format!("synththing/{} ( https://github.com/Dissssy/synththing )", crate::build_info::VERSION);
    match ureq::get(url).header("User-Agent", &agent).call() {
        Ok(mut response) => Ok(Some(response.body_mut().read_to_vec()?)),
        Err(ureq::Error::StatusCode(404)) => Ok(None),
        Err(e) => Err(e),
    }
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

    /// The color most of a picture is, not the grey all its colors make.
    #[test]
    fn accent_is_the_main_color() {
        let mut image = image::RgbaImage::from_pixel(10, 10, image::Rgba([240, 30, 30, 255]));
        for x in 0..10 {
            image.put_pixel(x, 0, image::Rgba([30, 30, 240, 255]));
            image.put_pixel(x, 1, image::Rgba([30, 240, 30, 255]));
        }
        assert_eq!(accent(&image), 0xFFF0_1E1E);
        let dark = image::RgbaImage::from_pixel(4, 4, image::Rgba([60, 10, 10, 255]));
        assert_eq!(accent(&dark), 0xFFC8_2121, "brightened");
    }

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
