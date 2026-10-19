//! The Android app: what `android_main` (the APK's native library, the
//! `android/` crate) starts, and the bridge to the app's Java side
//! (`android/app/src/main/java`).
//!
//! The app runs in eframe as on the desktop, always in the mobile layout.
//! What eframe can't do, Java does: the foreground service and media
//! session that keep music playing with the app in the background and put
//! it in the notification and on the lock screen, the document picker for
//! imports, picture-in-picture, and installing updates. Rust calls into
//! Java through methods on the activity ([`call`]); Java calls back
//! through the `Native` class's methods, which the `android/` crate
//! exports and hands to the functions here.
//!
//! While the app is in the background eframe has no window, so it calls
//! nothing at all; a thread of our own keeps [`App::tick`] going then, so
//! a playlist still moves on and the notification's buttons still work.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use android_activity::AndroidApp;
use eframe::egui;
use jni::objects::{JObject, JValue};
use jni::{JNIEnv, JavaVM};

use crate::app::App;
use crate::import::{self, Kind};

/// How often the app ticks while in the background.
const BACKGROUND_TICK: Duration = Duration::from_millis(250);

/// Start the app (from `android_main`).
pub fn main(android: AndroidApp) {
    if let Some(dir) = android.internal_data_path() {
        let _ = crate::config::set_config_dir(&dir.join("config"));
    }
    crate::applog::init(false);
    log::info!("synththing v{} starting on Android", crate::build_info::describe());

    let (app, stream) = match crate::startup::start() {
        Ok(started) => started,
        Err(e) => {
            log::error!("couldn't start: {e:#}");
            return;
        }
    };
    let app = Arc::new(Mutex::new(app));
    let last_frame = Arc::new(Mutex::new(Instant::now()));
    let options = eframe::NativeOptions { android_app: Some(android), ..Default::default() };
    let shared = Shared { app: Arc::clone(&app), last_frame: Arc::clone(&last_frame) };
    let result = eframe::run_native(
        "synththing",
        options,
        Box::new(move |cc| {
            crate::app::install_fonts(&cc.egui_ctx);
            background_ticks(Arc::clone(&app), Arc::clone(&last_frame), cc.egui_ctx.clone());
            Ok(Box::new(shared))
        }),
    );
    if let Err(e) = result {
        log::error!("the GUI stopped: {e}");
    }
    drop(stream);
}

/// The app as eframe sees it: shared with the background ticks.
struct Shared {
    app: Arc<Mutex<App>>,
    /// When eframe last ran a frame.
    last_frame: Arc<Mutex<Instant>>,
}

impl eframe::App for Shared {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.app.lock().unwrap().ui(ui, frame);
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        *self.last_frame.lock().unwrap() = Instant::now();
        self.app.lock().unwrap().logic(ctx, frame);
    }

    fn on_exit(&mut self) {
        self.app.lock().unwrap().on_exit();
    }
}

/// Tick the app whenever eframe hasn't for a while: it's in the
/// background, with no window.
fn background_ticks(app: Arc<Mutex<App>>, last_frame: Arc<Mutex<Instant>>, ctx: egui::Context) {
    let spawned = std::thread::Builder::new().name("background-ticks".to_string()).spawn(move || {
        loop {
            std::thread::sleep(BACKGROUND_TICK);
            let idle = last_frame.lock().unwrap().elapsed();
            if idle > BACKGROUND_TICK * 2 {
                app.lock().unwrap().tick(&ctx);
            }
        }
    });
    if let Err(e) = spawned {
        log::warn!("couldn't start the background ticks: {e}");
    }
}

/// Run `f` with the JNI environment and the activity, logging (and
/// clearing) whatever Java exception it ends in.
fn call<R>(what: &str, f: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<R>) -> Option<R> {
    let context = ndk_context::android_context();
    // SAFETY: android-activity set the context up with the process's JVM
    // and the activity, both alive for as long as the app is.
    let vm = match unsafe { JavaVM::from_raw(context.vm().cast()) } {
        Ok(vm) => vm,
        Err(e) => {
            log::warn!("{what}: no JVM: {e}");
            return None;
        }
    };
    let mut env = match vm.attach_current_thread_permanently() {
        Ok(env) => env,
        Err(e) => {
            log::warn!("{what}: couldn't attach to the JVM: {e}");
            return None;
        }
    };
    // SAFETY: a global reference to the activity, held by android-activity.
    let activity = unsafe { JObject::from_raw(context.context().cast()) };
    match f(&mut env, &activity) {
        Ok(r) => Some(r),
        Err(e) => {
            if env.exception_check().unwrap_or(false) {
                let _ = env.exception_describe();
                let _ = env.exception_clear();
            }
            log::warn!("{what}: {e}");
            None
        }
    }
}

/// Open the document picker for files of `kind` (any of `extensions`); the
/// Java side copies what's picked into `dir` and hands the copies back
/// through [`imported`].
pub fn pick_files(kind: Kind, extensions: &[&str], dir: &Path) {
    call("picking files", |env, activity| {
        let array = env.new_object_array(extensions.len() as i32, "java/lang/String", JObject::null())?;
        for (i, ext) in extensions.iter().enumerate() {
            let ext = env.new_string(ext)?;
            env.set_object_array_element(&array, i as i32, ext)?;
        }
        let dir = env.new_string(dir.to_string_lossy())?;
        env.call_method(
            activity,
            "pickFiles",
            "(I[Ljava/lang/String;Ljava/lang/String;)V",
            &[JValue::Int(kind.code()), JValue::Object(&array), JValue::Object(&dir)],
        )?;
        Ok(())
    });
}

/// From Java: an import's done (`paths` copied in), or failed.
pub fn imported(kind: i32, paths: Vec<PathBuf>, error: Option<String>) {
    let Some(kind) = Kind::from_code(kind) else { return };
    let result = match error {
        Some(error) => Err(error),
        None => Ok(paths),
    };
    import::finish(import::Finished { kind, result });
}

/// What's playing, for the notification, the lock screen and Bluetooth.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Playback {
    pub title: String,
    pub detail: String,
    pub playing: bool,
    pub position_ms: i64,
    pub length_ms: i64,
    pub has_next: bool,
}

/// Tell the Java side what's playing (it starts or stops the playback
/// service with it).
pub fn set_playback(playback: &Playback) {
    call("updating the media session", |env, activity| {
        let title = env.new_string(&playback.title)?;
        let detail = env.new_string(&playback.detail)?;
        env.call_method(
            activity,
            "setPlayback",
            "(Ljava/lang/String;Ljava/lang/String;ZJJZ)V",
            &[
                JValue::Object(&title),
                JValue::Object(&detail),
                JValue::Bool(playback.playing.into()),
                JValue::Long(playback.position_ms),
                JValue::Long(playback.length_ms),
                JValue::Bool(playback.has_next.into()),
            ],
        )?;
        Ok(())
    });
}

/// A button pressed outside the app: in the notification, on the lock
/// screen, on headphones or in picture-in-picture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MediaCommand {
    Play,
    Pause,
    Next,
    Previous,
    Seek(f64),
}

static MEDIA_COMMANDS: Mutex<Vec<MediaCommand>> = Mutex::new(Vec::new());

/// From Java: a media button, by its number (`PlaybackService.COMMAND_*`).
pub fn media_command(command: i32, position_ms: i64) {
    let command = match command {
        0 => MediaCommand::Play,
        1 => MediaCommand::Pause,
        2 => MediaCommand::Next,
        3 => MediaCommand::Previous,
        4 => MediaCommand::Seek(position_ms as f64 / 1000.0),
        _ => return,
    };
    MEDIA_COMMANDS.lock().unwrap().push(command);
}

/// The media buttons pressed since the last call.
pub fn take_media_commands() -> Vec<MediaCommand> {
    std::mem::take(&mut *MEDIA_COMMANDS.lock().unwrap())
}

static PICTURE_IN_PICTURE: AtomicBool = AtomicBool::new(false);

/// From Java: the app went into picture-in-picture, or came out of it.
pub fn set_picture_in_picture(on: bool) {
    PICTURE_IN_PICTURE.store(on, Ordering::Relaxed);
}

/// Whether the app is in picture-in-picture: a small floating window,
/// just the visualizer.
pub fn picture_in_picture() -> bool {
    PICTURE_IN_PICTURE.load(Ordering::Relaxed)
}

/// Whether picture-in-picture should be offered (something's playing):
/// leaving the app then shrinks it into the floating window.
pub fn set_picture_in_picture_allowed(allowed: bool) {
    call("allowing picture-in-picture", |env, activity| {
        env.call_method(activity, "setPictureInPictureAllowed", "(Z)V", &[JValue::Bool(allowed.into())])?;
        Ok(())
    });
}

/// Whether this app may install APKs (Settings > Apps > Special app access
/// > Install unknown apps), for installing updates itself.
pub fn can_install_packages() -> bool {
    call("checking install permission", |env, activity| env.call_method(activity, "canInstallPackages", "()Z", &[])?.z())
        .unwrap_or(false)
}

/// Open the setting that allows installing APKs, for this app.
pub fn open_install_settings() {
    call("opening the install setting", |env, activity| {
        env.call_method(activity, "openInstallSettings", "()V", &[])?;
        Ok(())
    });
}

/// Hand a downloaded APK to Android's installer (which asks the user).
pub fn install_apk(path: &Path) {
    call("installing an update", |env, activity| {
        let path = env.new_string(path.to_string_lossy())?;
        env.call_method(activity, "installApk", "(Ljava/lang/String;)V", &[JValue::Object(&path)])?;
        Ok(())
    });
}
