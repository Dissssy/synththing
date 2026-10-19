//! The Android app: what `android_main` (the APK's native library, the
//! `android/` crate) starts, and the bridge to the app's Java side
//! (`android/app/src/main/java`).
//!
//! The app runs in eframe as on the desktop, always in the mobile layout.
//! What eframe can't do, Java does: the foreground service and media
//! session that keep music playing with the app in the background and put
//! it in the notification and on the lock screen, the document picker for
//! imports, picture-in-picture, installing updates, and the keyboard's
//! connection to text fields ([`keyboard`]). Rust calls into Java through
//! methods on the activity ([`call`]); Java calls back through the
//! `Native` class's methods, which the `android/` crate exports and hands
//! to the functions here.
//!
//! While the app is in the background eframe has no window, so it calls
//! nothing at all; a thread of our own keeps [`App::tick`] going then, so
//! a playlist still moves on and the notification's buttons still work.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use android_activity::AndroidApp;
use eframe::egui;
use jni::objects::{GlobalRef, JObject, JString, JThrowable, JValue};
use jni::{JNIEnv, JavaVM};

use crate::app::App;
use crate::import::{self, Kind};

mod keyboard;
pub use keyboard::typed;

/// How often the app ticks while in the background.
const BACKGROUND_TICK: Duration = Duration::from_millis(250);

/// The JVM, for calls into Java from any thread.
static VM: OnceLock<JavaVM> = OnceLock::new();

/// The activity, whose methods those are: the current one (`android_main`
/// runs again with a new one if Android recreates it).
static ACTIVITY: Mutex<Option<GlobalRef>> = Mutex::new(None);

/// Start the app (from `android_main`).
pub fn main(android: AndroidApp) {
    if let Some(dir) = android.internal_data_path() {
        let _ = crate::config::set_config_dir(&dir.join("config"));
    }
    crate::applog::init(false);
    log::info!("synththing v{} starting on Android", crate::build_info::describe());
    keep_activity(&android);

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
            crate::app::fonts::install(&cc.egui_ctx);
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
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        // What the system bars, a notch and the keyboard cover, in points.
        let px = |side: &AtomicI32| side.load(Ordering::Relaxed) as f32 / ctx.pixels_per_point();
        let [left, top, right, bottom] = &INSETS;
        raw_input.safe_area_insets = Some(egui::SafeAreaInsets(egui::epaint::MarginF32 {
            left: px(left),
            right: px(right),
            top: px(top),
            bottom: px(bottom),
        }));
        // Android's back (the button, or the gesture) is the app's Escape:
        // it closes what's open, and the mobile layout goes back a step
        // (`mobile_back`).
        for event in &mut raw_input.events {
            if let egui::Event::Key { key, .. } = event
                && *key == egui::Key::BrowserBack
            {
                *key = egui::Key::Escape;
            }
        }
        keyboard::before_frame(ctx, raw_input);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        // The app draws under the system bars (edge to edge): their part
        // gets the background, and the app the rest. Full screen, what's
        // left (a notch, a camera hole) is black, unless the app is to
        // draw there too.
        let fullscreen = FULLSCREEN.load(Ordering::Relaxed);
        let fill = if fullscreen { egui::Color32::BLACK } else { ui.visuals().panel_fill };
        ui.painter().rect_filled(ui.max_rect(), 0.0, fill);
        let content = if fullscreen && OVER_CUTOUT.load(Ordering::Relaxed) {
            ui.max_rect()
        } else {
            ui.ctx().content_rect()
        };
        ui.scope_builder(egui::UiBuilder::new().max_rect(content), |ui| self.app.lock().unwrap().ui(ui, frame));
        // eframe has no clipboard on Android: what was copied goes to
        // Android's.
        let copied = ui.ctx().output(|o| {
            o.commands.iter().rev().find_map(|c| match c {
                egui::OutputCommand::CopyText(text) => Some(text.clone()),
                _ => None,
            })
        });
        if let Some(text) = copied {
            copy_text(&text);
        }
        keyboard::after_frame(ui.ctx());
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
/// background, with no window. Going there, it saves what it would on
/// closing, as Android may end it there.
fn background_ticks(app: Arc<Mutex<App>>, last_frame: Arc<Mutex<Instant>>, ctx: egui::Context) {
    let spawned = std::thread::Builder::new().name("background-ticks".to_string()).spawn(move || {
        let mut background = false;
        loop {
            std::thread::sleep(BACKGROUND_TICK);
            let idle = last_frame.lock().unwrap().elapsed() > BACKGROUND_TICK * 2;
            if idle {
                let mut app = app.lock().unwrap();
                if !background {
                    app.backgrounded();
                }
                app.tick(&ctx);
            }
            background = idle;
        }
    });
    if let Err(e) = spawned {
        log::warn!("couldn't start the background ticks: {e}");
    }
}

/// Keep the JVM and `android`'s activity for [`call`]. (Not ndk-context's:
/// android-activity gives that the Application, which hasn't the
/// activity's methods.)
fn keep_activity(android: &AndroidApp) {
    // SAFETY: android-activity's pointers to the process's JVM and to the
    // activity, which the global reference keeps alive.
    let vm = match unsafe { JavaVM::from_raw(android.vm_as_ptr().cast()) } {
        Ok(vm) => VM.get_or_init(|| vm),
        Err(e) => return log::error!("no JVM: {e}"),
    };
    let activity = vm.attach_current_thread_permanently().and_then(|env| {
        let activity = unsafe { JObject::from_raw(android.activity_as_ptr().cast()) };
        env.new_global_ref(activity)
    });
    match activity {
        Ok(activity) => *ACTIVITY.lock().unwrap() = Some(activity),
        Err(e) => log::error!("no activity: {e}"),
    }
}

/// Run `f` with the JNI environment and the activity, logging (and
/// clearing) whatever Java exception it ends in. Its local references
/// are freed when it's done: the threads calling are never back in Java to
/// free them.
fn call<R>(what: &str, f: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<R>) -> Option<R> {
    let vm = VM.get()?;
    let activity = ACTIVITY.lock().unwrap().clone()?;
    let mut env = match vm.attach_current_thread_permanently() {
        Ok(env) => env,
        Err(e) => {
            log::warn!("{what}: couldn't attach to the JVM: {e}");
            return None;
        }
    };
    let result = env.with_local_frame(16, |env| match f(env, activity.as_obj()) {
        Ok(r) => Ok(Some(r)),
        Err(e) => {
            let exception = java_exception(env);
            log::warn!("{what}: {}", exception.unwrap_or_else(|| e.to_string()));
            Ok(None)
        }
    });
    result.unwrap_or_else(|e: jni::errors::Error| {
        log::warn!("{what}: {e}");
        None
    })
}

/// The Java exception pending, if there is one, cleared and as its stack
/// trace (the exception, its message and where it was thrown).
fn java_exception(env: &mut JNIEnv) -> Option<String> {
    if !env.exception_check().ok()? {
        return None;
    }
    let exception: JThrowable = env.exception_occurred().ok()?;
    env.exception_clear().ok()?;
    let trace = env
        .call_static_method(
            "android/util/Log",
            "getStackTraceString",
            "(Ljava/lang/Throwable;)Ljava/lang/String;",
            &[JValue::Object(&exception)],
        )
        .and_then(|trace| trace.l());
    match trace {
        Ok(trace) => env.get_string(&JString::from(trace)).ok().map(String::from),
        Err(_) => {
            let _ = env.exception_clear();
            Some("a Java exception (its trace couldn't be read)".to_string())
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

/// From Java: an import's done (`copied` in; `skipped`, the names of
/// those not of a type it takes), or failed.
pub fn imported(kind: i32, copied: Vec<PathBuf>, skipped: Vec<String>, error: Option<String>) {
    let Some(kind) = Kind::from_code(kind) else { return };
    let result = match error {
        Some(error) => Err(error),
        None => Ok(import::Imported { copied, skipped }),
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

static LINKS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// From Java: the app was opened with a link (a sign-in code scanned with
/// the camera).
pub fn open_link(link: String) {
    LINKS.lock().unwrap().push(link);
}

/// The links the app was opened with since the last call.
pub fn take_links() -> Vec<String> {
    std::mem::take(&mut *LINKS.lock().unwrap())
}

/// The phone's model ("Pixel 8"), as Android names it.
pub fn device_model() -> Option<String> {
    call("asking the model", |env, _| {
        let model = env.get_static_field("android/os/Build", "MODEL", "Ljava/lang/String;")?.l()?;
        Ok(env.get_string(&JString::from(model))?.into())
    })
}

/// What the system bars, a notch and the keyboard cover of the window:
/// left, top, right and bottom, in pixels (from Java, as they change).
static INSETS: [AtomicI32; 4] = [const { AtomicI32::new(0) }; 4];

/// From Java: the window's insets changed.
pub fn set_insets(left: i32, top: i32, right: i32, bottom: i32) {
    for (side, value) in INSETS.iter().zip([left, top, right, bottom]) {
        side.store(value, Ordering::Relaxed);
    }
}

/// Whether the system bars are hidden, as last asked.
static FULLSCREEN: AtomicBool = AtomicBool::new(false);
/// Whether full screen draws under a notch or camera hole too.
static OVER_CUTOUT: AtomicBool = AtomicBool::new(false);

/// Hide the status and navigation bars (`on`), or show them again;
/// `over_cutout`: full screen draws under a notch or camera hole as well.
pub fn set_fullscreen(on: bool, over_cutout: bool) {
    OVER_CUTOUT.store(over_cutout, Ordering::Relaxed);
    if FULLSCREEN.swap(on, Ordering::Relaxed) == on {
        return;
    }
    call("going full screen", |env, activity| {
        env.call_method(activity, "setFullscreen", "(Z)V", &[JValue::Bool(on.into())])?;
        Ok(())
    });
}

/// Put the app in the background, as Android's back does on the home
/// screen's apps (playback carries on).
pub fn leave() {
    call("leaving", |env, activity| {
        env.call_method(activity, "moveTaskToBack", "(Z)Z", &[JValue::Bool(1)])?;
        Ok(())
    });
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

/// Whether picture-in-picture should be offered (something's playing and
/// the mini player is on): leaving the app then shrinks it into the
/// floating window, in `shape`.
pub fn set_picture_in_picture_allowed(allowed: bool, shape: crate::config::MiniPlayerShape) {
    use crate::config::MiniPlayerShape;
    let aspect = match shape {
        MiniPlayerShape::Square => (1, 1),
        MiniPlayerShape::Wide => (16, 9),
        MiniPlayerShape::Tall => (9, 16),
    };
    call("allowing picture-in-picture", |env, activity| {
        env.call_method(
            activity,
            "setPictureInPictureAllowed",
            "(ZII)V",
            &[JValue::Bool(allowed.into()), JValue::Int(aspect.0), JValue::Int(aspect.1)],
        )?;
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

/// Put `text` on Android's clipboard.
fn copy_text(text: &str) {
    call("copying", |env, activity| {
        let text = env.new_string(text)?;
        env.call_method(activity, "copyText", "(Ljava/lang/String;)V", &[JValue::Object(&text)])?;
        Ok(())
    });
}

/// The most of the log that's shared: Android passes it between apps in
/// one message, which can't be much more than this.
const SHARED_LOG_BYTES: usize = 256 * 1024;

/// Share the log (`path`, the end of it if it's long) with an app the user
/// picks: to send it, or keep it.
pub fn share_log(path: &Path) {
    let log = match std::fs::read(path) {
        Ok(log) => log,
        Err(e) => return log::warn!("couldn't read {} to share: {e}", path.display()),
    };
    let tail = &log[log.len().saturating_sub(SHARED_LOG_BYTES)..];
    let text = String::from_utf8_lossy(tail);
    call("sharing the log", |env, activity| {
        let subject = env.new_string(format!("synththing {} log", crate::build_info::describe()))?;
        let text = env.new_string(text)?;
        env.call_method(
            activity,
            "shareText",
            "(Ljava/lang/String;Ljava/lang/String;)V",
            &[JValue::Object(&subject), JValue::Object(&text)],
        )?;
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
