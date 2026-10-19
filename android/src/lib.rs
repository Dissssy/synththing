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

/// `Native.imported(int kind, String[] paths, String error)`: an import's
/// done (`error` null), or failed.
#[unsafe(no_mangle)]
pub extern "system" fn Java_nl_p51_synththing_Native_imported<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    kind: jint,
    paths: JObjectArray<'local>,
    error: JString<'local>,
) {
    let mut list = Vec::new();
    let len = env.get_array_length(&paths).unwrap_or(0);
    for i in 0..len {
        if let Ok(path) = env.get_object_array_element(&paths, i)
            && let Ok(path) = env.get_string(&JString::from(path))
        {
            list.push(PathBuf::from(String::from(path)));
        }
    }
    let error = if error.is_null() { None } else { env.get_string(&error).ok().map(String::from) };
    synththing::android::imported(kind, list, error);
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
