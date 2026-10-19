# synththing on Android

An experimental Android app (issue #27): the same Rust app, with a mobile layout made for a phone. It's on GitHub only (no Play Store): each release has `synththing-android-arm64.apk`, and the `nightly` canary has one too, once the release key is set up (section 4).

Status: it builds (CI's `android` job) and the Rust side cross-compiles cleanly, but nobody has run it on a phone yet. Try it, and work through section 6.

## 1. What it is

- **The mobile layout.** One page at a time: Now playing (the visualizer, with the controls under it), Songs, Soundfonts, Playlists, Visualizers, Library and Downloads, from the menu at the top left. The chevron next to the time collapses the controls so the visualizer fills the screen. On Android it's always on. On desktop it's View > Mobile layout (and "Desktop layout" in its menu goes back), so it can be tried and tested without a phone: branch builds have it too, and testers of a branch that touches it should check both layouts.
- **Back.** Android's back (button or gesture), and Escape on desktop, goes back a step at a time: it closes a dialog or popup, then the menu, then a Preferences section, then full screen, then any page but Now playing. On Now playing, back says "Back again to leave synththing", and a second back puts the app in the background (playback carries on). `mobile_back` in `src/app/mobile.rs`; a new screen that needs its own step adds it there.
- **Preferences as a page.** The menu's Preferences opens a page listing the sections (`src/app/preferences.rs`, the same sections and order as the desktop's Preferences window); each opens in its place, with a back arrow.
- **Importing instead of browsing.** Android apps can't wander the file system, so Songs and Soundfonts have "Import..." instead: Android's file picker, and the files are copied into the app (its own `music` and `soundfonts` folders). Songs shows that music folder. The same import is on desktop in the mobile layout (copying into the config folder's `music`), so it works the same everywhere.
- **Playing in the background.** A foreground service with a media session, so playback keeps going with the screen off, the notification and lock screen have the controls, and Bluetooth buttons work. It pauses for calls and other apps' sound, and when headphones come out.
- **The visualizer in a floating window.** Leaving the app while something's playing shrinks it into picture-in-picture, showing only the visualizer, with previous, play/pause and next.
- **Updates.** The app checks for a new version as on desktop, and says once that one's out, to download from the release page. Preferences > Updates > "Install updates in the app" downloads and installs it instead, once Android's "Install unknown apps" setting allows synththing (the app offers to open it).
- **Read-only to the library.** It browses, installs and updates scripts from the Online Library, but has no identity: no publishing, encores, reports, deleting or moderation, and no identity in Preferences > Online Library (`READ_ONLY` in `src/app/library.rs`). Feedback works, as it needs no account.

Left out: script editing (and the sprite editor), recording, the mini player and tray, branch builds, the tour (it shows desktop panels), m3u import and export, and dragging to reorder.

## 2. How it fits together

- `src/lib.rs` is the app (the binary in `src/main.rs` just calls `synththing::main`); `src/startup.rs` starts it up, for desktop (`src/desktop.rs`) and Android (`src/android.rs`) alike.
- `android/` is a small cdylib crate, `synththing-android` (`libsynththing_android.so`): `android_main`, and the JNI functions Java calls. Everything else is in `src/android.rs`: running eframe in Android's activity, the calls into Java (`pick_files`, `set_playback`, picture-in-picture, installing), and ticking the app in the background (eframe draws no frames without a window, but the playlist still has to move on).
- `android/app/` is the Gradle project around it. `MainActivity` is a NativeActivity with what eframe can't do: the file picker, picture-in-picture and installing APKs. `PlaybackService` is the foreground service and media session. `Native` is Java's way back into Rust.
- `src/app/mobile.rs` is the mobile layout, reusing the desktop pages' UI.

The launcher icon is in the build's accent, as on desktop: purple for a release, yellow for canary, a branch build's own color, off-white for a local build. `build.rs` works it out and leaves it beside the native library (`target/aarch64-linux-android/release/synththing-accent`, under `CARGO_TARGET_DIR` if that's set), where Gradle reads it.

Only arm64 is built (every Android phone from the last several years); minSdk 26 (Android 8), targetSdk 34.

## 3. Building it

With the Android SDK and NDK (Android Studio installs both; set `ANDROID_HOME` and `ANDROID_NDK_HOME`), Java 17, Gradle 8.9 or newer, and Rust:

```
rustup target add aarch64-linux-android
cargo install cargo-ndk
cargo ndk -t arm64-v8a --platform 26 -o android/app/src/main/jniLibs build -p synththing-android --release
gradle -p android assembleRelease
adb install android/app/build/outputs/apk/release/app-release.apk
```

Without the release key, that's signed with your debug key: fine to try, but it won't update (or be updated by) a published APK. The app's log is in the app (menu > Log); `adb logcat -s synththing` has the Java side's.

CI does the same in `.github/actions/android-apk`, which `ci.yml` (every pull request), `test-builds.yml` (the nightly) and `release.yml` share. It pins the SDK packages and NDK (27.2.12479018) with setup-android. The jobs run on the self-hosted runner when the `RUNNER` repository variable names one (not for pull requests from forks), and on GitHub's otherwise; the runner keeps the SDK, Gradle's caches, the JDK and cargo's builds on volumes, so the GitHub caches are skipped there.

## 4. The release key (needs doing once)

Android installs an update only when it's signed with the same key as what's installed, so every published APK must have one key, kept for good: lose it and everyone has to uninstall and reinstall. Until it's set up, CI builds the APK but publishes none.

1. Make it (keep the file and both passwords somewhere safe):
   ```
   keytool -genkeypair -v -keystore synththing-release.jks -alias synththing \
       -keyalg RSA -keysize 4096 -validity 36500
   ```
2. In the repository's Settings > Secrets and variables > Actions, add:
   - `ANDROID_KEYSTORE`: the file, base64 (`base64 -w0 synththing-release.jks`)
   - `ANDROID_KEYSTORE_PASSWORD`: its password
   - `ANDROID_KEY_ALIAS`: `synththing`
   - `ANDROID_KEY_PASSWORD`: the key's password (the same one, unless you gave it another)

The next push to master then puts an APK in the nightly, and the next release has one.

## 5. Versions

Android orders updates by `versionCode` (`android/app/build.gradle.kts`): a release's is its version times 1000 plus 999 (0.8.1 is 801999), and a test build's (0.8.2-nightly.12, or a branch's 0.8.2-branch.12) is its version's times 1000 plus its number (802012). So a test build installs over the release before it, and the next release installs over every test build leading up to it. Going back from a test build to the release before it means uninstalling first.

## 6. To check on a phone

- It starts, plays a starter song, and the visualizer draws (wgpu on Vulkan, or GLES).
- Importing songs and a soundfont; they play, and the soundfont can be picked.
- Playback carries on with the screen off and the app in the background; the notification and lock screen controls work, and so do headphone buttons.
- A call or another app's sound pauses it (and a short one resumes it); unplugging headphones pauses.
- Leaving the app while playing goes into the floating window, and its buttons work.
- The update alert, and installing one with the experimental setting.
- Text input: the search boxes, a playlist's name (the on-screen keyboard).
- Rotating, and the notch or punch hole: nothing important under them.

## 7. Defaults picked, open to change

- arm64 only, Android 8 and newer, targetSdk 34 (35 forces drawing under the status and navigation bars, which the layout doesn't handle yet).
- A tap plays a song (on desktop it's a double-click); long-press is the menu.
- No drag to reorder on a touch screen.
- No m3u playlist import or export, and no identity import or export, on Android.
- Later: giving the phone an identity from the desktop app (a QR code), so it can give encores.
