// The Android app (docs/ANDROID.md). Gradle packages the APK; the native
// library in it (app/src/main/jniLibs) is built first, with cargo-ndk.
pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "synththing"
include(":app")
