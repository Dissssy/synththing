//! Experimental YouTube search and audio downloads via an installed yt-dlp.
//! Work runs off the UI thread. No shell interpretation or network calls on the UI thread.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use eframe::egui;
use serde::Deserialize;

use super::App;

#[derive(Clone, Debug, Deserialize)]
struct Video {
    id: String,
    title: String,
    #[serde(default)]
    uploader: Option<String>,
    #[serde(default)]
    duration: Option<f64>,
}

enum Event {
    Results(Result<Vec<Video>, String>),
    Downloaded(Result<Vec<PathBuf>, String>),
}

pub(super) struct YoutubeState {
    pub open: bool,
    query: String,
    items: Vec<Video>,
    error: Option<String>,
    busy: bool,
    events: (Sender<Event>, Receiver<Event>),
    yt_dlp: String,
}

impl YoutubeState {
    pub fn new() -> Self {
        Self {
            open: false, query: String::new(), items: Vec::new(), error: None,
            busy: false, events: mpsc::channel(),
            yt_dlp: if cfg!(windows) { "yt-dlp.exe".into() } else { "yt-dlp".into() },
        }
    }
}

fn command(exe: &str) -> Command {
    let mut cmd = Command::new(exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

fn search(exe: &str, query: &str) -> Result<Vec<Video>, String> {
    let output = command(exe)
        .args(["--ignore-config", "--flat-playlist", "--dump-json", "--no-warnings", "--"])
        .arg(format!("ytsearch25:{query}"))
        .output().map_err(|e| format!("Couldn't run yt-dlp: {e}. Install yt-dlp and add it to PATH."))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    let mut items: Vec<Video> = String::from_utf8(output.stdout).map_err(|e| e.to_string())?
        .lines().filter(|s| !s.trim().is_empty())
        .map(|line| serde_json::from_str::<Video>(line).map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    items.sort_by(|a, b| {
        let score = |v: &Video| {
            let label = format!("{} {}", v.title, v.uploader.as_deref().unwrap_or(""));
            crate::filebrowser::fuzzy_score(&label, query).unwrap_or(i32::MIN)
        };
        score(b).cmp(&score(a))
    });
    Ok(items)
}

fn download(exe: &str, url: &str, dir: &Path, ffmpeg: Option<&Path>, playlist: bool) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    // The video ID makes names stable and avoids accidental overwrites.
    let mut cmd = command(exe);
    cmd.args(["--ignore-config", "--no-overwrites", "--restrict-filenames",
        "--extract-audio", "--format", "bestaudio/best", "--write-info-json", "--print", "after_move:filepath",
        "--paths"]).arg(dir)
        .arg(if playlist { "--yes-playlist" } else { "--no-playlist" })
        .arg("--download-archive").arg(dir.join(".youtube-downloads.txt"))
        .args(["--output", "%(title).160B [%(id)s].%(ext)s"]);
    if let Some(path) = ffmpeg { cmd.arg("--ffmpeg-location").arg(path); }
    let output = cmd.arg("--").arg(url)
        .stdout(Stdio::piped()).stderr(Stdio::piped()).output()
        .map_err(|e| format!("Couldn't start yt-dlp: {e}"))?;
    if !output.status.success() {
        return Err(format!("Download failed: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    let paths: Vec<PathBuf> = String::from_utf8_lossy(&output.stdout)
        .lines().map(str::trim).filter(|line| !line.is_empty())
        .map(PathBuf::from).filter(|path| path.is_file()).collect();
    // An empty result with a successful exit typically means all entries were
    // already present in the persistent download archive.
    Ok(paths)
}

impl App {
    fn youtube_search(&mut self) {
        let query = self.youtube.query.trim().to_string();
        if query.is_empty() || self.youtube.busy { return; }
        self.youtube.busy = true;
        self.youtube.items.clear();
        self.youtube.error = None;
        let tx = self.youtube.events.0.clone();
        let exe = self.youtube.yt_dlp.clone();
        thread::spawn(move || { let _ = tx.send(Event::Results(search(&exe, &query))); });
    }

    fn youtube_download(&mut self, item: Video) {
        if self.youtube.busy { return; }
        self.youtube.busy = true;
        self.youtube.error = None;
        let exe = self.youtube.yt_dlp.clone();
        let tx = self.youtube.events.0.clone();
        let folder = self.config.youtube_download_dir.clone().unwrap_or_else(|| self.config.browse_start_dir().join("YouTube"));
        let ffmpeg = crate::ffmpeg::find();
        thread::spawn(move || {
            let url = format!("https://www.youtube.com/watch?v={}", item.id);
            let result = download(&exe, &url, &folder, ffmpeg.as_deref(), false);
            let _ = tx.send(Event::Downloaded(result));
        });
    }

    fn youtube_download_link(&mut self, url: String) {
        if self.youtube.busy { return; }
        if !is_youtube_url(&url) {
            self.youtube.error = Some("Enter a YouTube or YouTube Music URL.".into());
            return;
        }
        self.youtube.busy = true;
        self.youtube.error = None;
        let exe = self.youtube.yt_dlp.clone();
        let tx = self.youtube.events.0.clone();
        let folder = self.config.youtube_download_dir.clone().unwrap_or_else(|| self.config.browse_start_dir().join("YouTube"));
        let ffmpeg = crate::ffmpeg::find();
        thread::spawn(move || {
            let result = download(&exe, &url, &folder, ffmpeg.as_deref(), true);
            let _ = tx.send(Event::Downloaded(result));
        });
    }

    pub(super) fn youtube_ui(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.youtube.events.1.try_recv() {
            self.youtube.busy = false;
            match event {
                Event::Results(result) => match result {
                    Ok(items) => self.youtube.items = items,
                    Err(e) => self.youtube.error = Some(e),
                },
                Event::Downloaded(result) => match result {
                    Ok(paths) => {
                        self.status = if paths.is_empty() { "Nothing new to download (already in download archive).".into() }
                            else { format!("Downloaded {} track(s) to {}", paths.len(), paths[0].parent().unwrap_or(Path::new(".")).display()) };
                        self.browser.rescan();
                    },
                    Err(e) => self.youtube.error = Some(e),
                },
            }
        }
        if !self.youtube.open { return; }
        let mut open = true;
        let mut run_search = false;
        let mut run_link = None;
        let mut download_item = None;
        egui::Modal::new(egui::Id::new("youtube_music")).show(ctx, |ui| {
            ui.set_width(600.0);
            ui.heading("Find music on YouTube (experimental)");
            ui.weak("Search by artist/song, or paste a YouTube video or playlist URL.");
            ui.horizontal(|ui| {
                let edit = ui.add(egui::TextEdit::singleline(&mut self.youtube.query).hint_text("Artist, song, or album").desired_width(440.0));
                let submitted = (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                    || ui.add_enabled(!self.youtube.busy, egui::Button::new("Go")).clicked();
                if submitted {
                    if self.youtube.query.starts_with("http") { run_link = Some(self.youtube.query.trim().to_string()); }
                    else { run_search = true; }
                }
            });
            ui.horizontal(|ui| {
                ui.weak("Download folder:");
                let dir = self.config.youtube_download_dir.clone().unwrap_or_else(|| self.config.browse_start_dir().join("YouTube"));
                ui.label(dir.display().to_string());
                if ui.small_button("Change...").clicked() {
                    if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                        self.config.youtube_download_dir = Some(folder);
                        if let Err(err) = self.config.save() { self.youtube.error = Some(format!("Could not save download folder: {err}")); }
                    }
                }
            });
            if self.youtube.busy { ui.spinner(); ui.weak("Working..."); }
            if let Some(error) = &self.youtube.error { ui.colored_label(ui.visuals().error_fg_color, error); }
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                for item in &self.youtube.items {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(&item.title);
                            let duration = item.duration.map(|s| format!(" • {}:{:02}", s as u64 / 60, s as u64 % 60)).unwrap_or_default();
                            ui.weak(format!("{}{}", item.uploader.as_deref().unwrap_or("Unknown uploader"), duration));
                        });
                        if ui.add_enabled(!self.youtube.busy, egui::Button::new("Download")).clicked() {
                            download_item = Some(item.clone());
                        }
                    });
                    ui.separator();
                }
            });
            if ui.button("Close").clicked() { open = false; }
        });
        self.youtube.open = open;
        if run_search { self.youtube_search(); }
        if let Some(url) = run_link { self.youtube_download_link(url); }
        if let Some(item) = download_item { self.youtube_download(item); }
    }
}

fn is_youtube_url(input: &str) -> bool {
    let Ok(url) = input.split_once("://").ok_or(()) else { return false; };
    if url.0 != "https" { return false; }
    let authority = url.1.split('/').next().unwrap_or("").split('?').next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("").split(':').next().unwrap_or("");
    matches!(host, "youtube.com" | "www.youtube.com" | "music.youtube.com" | "youtu.be" | "www.youtu.be")
}

#[cfg(test)]
mod tests {
    use super::is_youtube_url;

    #[test]
    fn accepts_video_playlist_and_music_links() {
        assert!(is_youtube_url("https://www.youtube.com/watch?v=abc"));
        assert!(is_youtube_url("https://youtu.be/abc"));
        assert!(is_youtube_url("https://music.youtube.com/playlist?list=abc"));
    }

    #[test]
    fn rejects_other_hosts_and_schemes() {
        assert!(!is_youtube_url("https://youtube.com.evil.invalid/watch?v=abc"));
        assert!(!is_youtube_url("http://youtube.com/watch?v=abc"));
        assert!(!is_youtube_url("https://evil.invalid/playlist?list=abc"));
        assert!(!is_youtube_url("not a link"));
    }
}
