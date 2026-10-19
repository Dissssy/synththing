plugins {
    id("com.android.application")
}

// Android only installs an update with a higher code: a release's is its
// version's (1.2.3 is 10203) times 1000 plus 999, and a test build's
// (1.2.4-nightly.12 or 1.2.4-branch.12, on the way to 1.2.4) is that
// version's times 1000 plus its number, so a test build installs over the
// release before it, and the next release installs over every test build
// leading up to it.
fun codeFor(version: String): Int {
    val (major, minor, patch) = version.substringBefore('-').split('.').map { it.toInt() }
    val build = version.substringAfter('-', "").substringAfter('.', "").toIntOrNull()
    return (major * 10_000 + minor * 100 + patch) * 1000 + (build?.coerceAtMost(998) ?: 999)
}

// The version: CI passes a test build's (-PsynththingVersion=0.8.2-nightly.12);
// otherwise it's Cargo.toml's, a release.
val synththingVersion: String = (findProperty("synththingVersion") as String?)
    ?: Regex("""(?m)^version = "(.+)"""").find(rootProject.file("../Cargo.toml").readText())!!.groupValues[1]

// The icon's color: the build's accent, as on desktop (purple for a
// release, yellow for canary, a branch's own color, off-white built
// locally). Rust's build script works it out, and leaves it beside the
// native library it built (in CARGO_TARGET_DIR, if that's set).
val cargoTarget: File = System.getenv("CARGO_TARGET_DIR")?.let { File(it) } ?: rootProject.file("../target")
val accent: String = cargoTarget.resolve("aarch64-linux-android/release/synththing-accent").let { file ->
    check(file.exists()) { "no ${file.path}: build the native library first (docs/ANDROID.md)" }
    file.readText().trim()
}

// Signed with the release key when CI has it (the ANDROID_KEYSTORE_*
// secrets, docs/ANDROID.md): every build that's published must be, or
// Android refuses it as an update. Otherwise with the debug key, for
// trying a build out: that can never update the published app, so it's
// a separate app beside it ("synththing test"), and trying a pull
// request's APK doesn't mean uninstalling the nightly and its data.
val keystore: String? = System.getenv("ANDROID_KEYSTORE_FILE")

android {
    namespace = "nl.p51.synththing"
    compileSdk = 34

    defaultConfig {
        applicationId = if (keystore != null) "nl.p51.synththing" else "nl.p51.synththing.test"
        minSdk = 26
        targetSdk = 34
        versionCode = codeFor(synththingVersion)
        versionName = synththingVersion
        resValue("color", "ic_launcher_background", accent)
        manifestPlaceholders["appLabel"] = if (keystore != null) "synththing" else "synththing test"
        ndk {
            abiFilters += "arm64-v8a"
        }
    }

    signingConfigs {
        if (keystore != null) {
            create("release") {
                storeFile = file(keystore)
                storePassword = System.getenv("ANDROID_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("ANDROID_KEY_ALIAS")
                keyPassword = System.getenv("ANDROID_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = if (keystore != null) signingConfigs.getByName("release") else signingConfigs.getByName("debug")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}
