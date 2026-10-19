plugins {
    id("com.android.application")
}

// Android only installs an update with a higher code: a release's is its
// version's (1.2.3 is 10203) times 1000 plus 999, and a canary build's
// (1.2.4-nightly.12, on the way to 1.2.4) is that version's times 1000 plus
// its number, so a canary build installs over the release before it, and
// the next release installs over every canary build leading up to it.
fun codeFor(version: String): Int {
    val (major, minor, patch) = version.substringBefore('-').split('.').map { it.toInt() }
    val build = version.substringAfter("-nightly.", "").toIntOrNull()
    return (major * 10_000 + minor * 100 + patch) * 1000 + (build?.coerceAtMost(998) ?: 999)
}

// The version: CI passes a test build's (-PsynththingVersion=0.8.2-nightly.12);
// otherwise it's Cargo.toml's, a release.
val synththingVersion: String = (findProperty("synththingVersion") as String?)
    ?: Regex("""(?m)^version = "(.+)"""").find(rootProject.file("../Cargo.toml").readText())!!.groupValues[1]

android {
    namespace = "nl.p51.synththing"
    compileSdk = 34

    defaultConfig {
        applicationId = "nl.p51.synththing"
        minSdk = 26
        targetSdk = 34
        versionCode = codeFor(synththingVersion)
        versionName = synththingVersion
        ndk {
            abiFilters += "arm64-v8a"
        }
    }

    // Signed with the release key when CI has it (the ANDROID_KEYSTORE_*
    // secrets, docs/ANDROID.md): every build that's published must be, or
    // Android refuses it as an update. Otherwise with the debug key, for
    // trying a build out.
    val keystore = System.getenv("ANDROID_KEYSTORE_FILE")
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
