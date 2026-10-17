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
    Downloaded(Result<PathBuf, String>),
}

pub(super) struct YoutubeState {
    pub open: bool,
    query: String,
    items: Vec<Video>,
    error: Option<String>,
    busy: bool,
    events: (Sender<Event>, Receiver<Event>),
    destination: Option<PathBuf>,
    yt_dlp: String,
}

impl YoutubeState {
    pub fn new() -> Self {
        Self {
            open: false, query: String::new(), items: Vec::new(), error: None,
            busy: false, events: mpsc::channel(), destination: None,
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
    String::from_utf8(output.stdout).map_err(|e| e.to_string())?
        .lines().filter(|s| !s.trim().is_empty())
        .map(|line| serde_json::from_str::<Video>(line).map_err(|e| e.to_string()))
        .collect()
}

fn download(exe: &str, url: &str, dir: &Path, ffmpeg: Option<&Path>) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    // The video ID makes names stable and avoids accidental overwrites.
    let mut cmd = command(exe);
    cmd.args(["--ignore-config", "--no-playlist", "--no-overwrites", "--restrict-filenames",
        "--extract-audio", "--format", "bestaudio/best", "--print", "after_move:filepath",
        "--paths"]).arg(dir)
        .args(["--output", "%(title).160B [%(id)s].%(ext)s"]);
    if let Some(path) = ffmpeg { cmd.arg("--ffmpeg-location").arg(path); }
    let output = cmd.arg("--").arg(url)
        .stdout(Stdio::piped()).stderr(Stdio::piped()).output()
        .map_err(|e| format!("Couldn't start yt-dlp: {e}"))?;
    if !output.status.success() {
        return Err(format!("Download failed: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    let path = String::from_utf8_lossy(&output.stdout);
    let path = path.lines().last().ok_or("yt-dlp did not report a downloaded path")?.trim();
    let path = PathBuf::from(path);
    if !path.is_file() { return Err("yt-dlp finished without a playable audio file".into()); }
    Ok(path)
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
        let folder = self.youtube.destination.clone().unwrap_or_else(|| self.config.browse_start_dir().join("YouTube"));
        let ffmpeg = crate::ffmpeg::find();
        thread::spawn(move || {
            let url = format!("https://www.youtube.com/watch?v={}", item.id);
            let result = download(&exe, &url, &folder, ffmpeg.as_deref());
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
                    Ok(path) => {
                        self.status = format!("Downloaded {}", path.display());
                        self.browser.rescan();
                    },
                    Err(e) => self.youtube.error = Some(e),
                },
            }
        }
        if !self.youtube.open { return; }
        let mut open = true;
        let mut run_search = false;
        let mut download_item = None;
        egui::Modal::new(egui::Id::new("youtube_music")).show(ctx, |ui| {
            ui.set_width(600.0);
            ui.heading("Find music on YouTube (experimental)");
            ui.horizontal(|ui| {
                let edit = ui.add(egui::TextEdit::singleline(&mut self.youtube.query).hint_text("Artist, song, or album").desired_width(440.0));
                run_search = (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                    || ui.add_enabled(!self.youtube.busy, egui::Button::new("Search")).clicked();
            });
            ui.horizontal(|ui| {
                ui.weak("Download folder:");
                let dir = self.youtube.destination.clone().unwrap_or_else(|| self.config.browse_start_dir().join("YouTube"));
                ui.label(dir.display().to_string());
                if ui.small_button("Change...").clicked() {
                    if let Some(folder) = rfd::FileDialog::new().pick_folder() { self.youtube.destination = Some(folder); }
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
        if let Some(item) = download_item { self.youtube_download(item); }
    }
}
