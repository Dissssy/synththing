//! The Android app's native library (see `src/android.rs` in the synththing
//! library, which is the app): `android_main`, which android-activity
//! calls once the activity is up, and the `Native` class's methods, which
//! the Java side calls.

#![cfg(target_os = "android")]

use std::path::PathBuf;

use android_activity::AndroidApp;
use jni::JNIEnv;
use jni::objects::{JClass, JObjectArray, JString};
use jni::sys::{jboolean, jint, jlong};

#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    synththing::android::main(app);
}

/// `Native.imported(int kind, String[] paths, String[] skipped, String
/// error)`: an import's done (`error` null), or failed.
#[unsafe(no_mangle)]
pub extern "system" fn Java_nl_p51_synththing_Native_imported<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    kind: jint,
    paths: JObjectArray<'local>,
    skipped: JObjectArray<'local>,
    error: JString<'local>,
) {
    let copied = strings(&mut env, &paths)
        .into_iter()
        .map(PathBuf::from)
        .collect();
    let skipped = strings(&mut env, &skipped);
    let error = if error.is_null() {
        None
    } else {
        env.get_string(&error).ok().map(String::from)
    };
    synththing::android::imported(kind, copied, skipped, error);
}

/// A Java `String[]`'s strings.
fn strings(env: &mut JNIEnv, array: &JObjectArray) -> Vec<String> {
    let len = env.get_array_length(array).unwrap_or(0);
    (0..len)
        .filter_map(|i| {
            let item = env.get_object_array_element(array, i).ok()?;
            env.get_string(&JString::from(item)).ok().map(String::from)
        })
        .collect()
}

/// `Native.mediaCommand(int command, long positionMs)`: a media button.
#[unsafe(no_mangle)]
pub extern "system" fn Java_nl_p51_synththing_Native_mediaCommand<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    command: jint,
    position_ms: jlong,
) {
    synththing::android::media_command(command, position_ms);
}

/// `Native.insets(int left, int top, int right, int bottom)`: what the
/// system bars, a notch and the keyboard cover, in pixels.
#[unsafe(no_mangle)]
pub extern "system" fn Java_nl_p51_synththing_Native_insets<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    left: jint,
    top: jint,
    right: jint,
    bottom: jint,
) {
    synththing::android::set_insets(left, top, right, bottom);
}

/// `Native.pictureInPicture(boolean on)`: into or out of picture-in-picture.
#[unsafe(no_mangle)]
pub extern "system" fn Java_nl_p51_synththing_Native_pictureInPicture<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    on: jboolean,
) {
    synththing::android::set_picture_in_picture(on != 0);
}

/// `Native.openLink(String link)`: the app was opened with a link.
#[unsafe(no_mangle)]
pub extern "system" fn Java_nl_p51_synththing_Native_openLink<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    link: JString<'local>,
) {
    if let Ok(link) = env.get_string(&link) {
        synththing::android::open_link(link.into());
    }
}
