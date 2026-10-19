import java.util.Properties

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

// What Rust's build script worked out for this build's channel
// (src/android_id.rs): the app's id and launcher name, and the icon's
// color, the build's accent as on desktop (purple for a release, yellow
// for canary, a branch's own color, off-white built locally). It leaves
// them beside the native library it built (in CARGO_TARGET_DIR, if
// that's set).
val cargoTarget: File = System.getenv("CARGO_TARGET_DIR")?.let { File(it) } ?: rootProject.file("../target")
val stamp = Properties().apply {
    val file = cargoTarget.resolve("aarch64-linux-android/release/synththing-android.properties")
    check(file.exists()) { "no ${file.path}: build the native library first (docs/ANDROID.md)" }
    file.reader().use { load(it) }
}

// Signed with the release key when CI has it (the ANDROID_KEYSTORE_*
// secrets, docs/ANDROID.md): every build that's published must be, or
// Android refuses it as an update. Otherwise with the debug key, for
// trying a build out: that can never update the published app, so it's
// a separate one beside it (".test", "... test"), and trying a pull
// request's APK doesn't mean uninstalling the nightly and its data.
val keystore: String? = System.getenv("ANDROID_KEYSTORE_FILE")
val testSuffix = keystore == null

// A branch build's app is its own (src/android_id.rs), and its updates
// follow the branch's commits, which a rebase can make fewer: its code is
// the workflow run's number instead, which only goes up.
val appVersionCode: Int = System.getenv("GITHUB_RUN_NUMBER")
    ?.takeIf { stamp.getProperty("channel").startsWith("branch-") }
    ?.toInt()
    ?: codeFor(synththingVersion)

android {
    namespace = "nl.p51.synththing"
    compileSdk = 34

    defaultConfig {
        applicationId = stamp.getProperty("applicationId") + if (testSuffix) ".test" else ""
        minSdk = 26
        targetSdk = 34
        versionCode = appVersionCode
        versionName = synththingVersion
        resValue("color", "ic_launcher_background", stamp.getProperty("accent"))
        manifestPlaceholders["appLabel"] = stamp.getProperty("label") + if (testSuffix) " test" else ""
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
