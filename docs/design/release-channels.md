# Release channels: canary (nightly) and branch builds

Status: being built in #4 (tracked in #3). Updated as parts land: it describes what the code does.

## Goal

Two kinds of test build, besides stable releases:

- **Canary**: the latest commit on `master`, for people who want new things first. Canary also installs full releases when they're newer, so a canary user never has to switch back to stable.
- **Branch builds**: the latest commit on a feature branch, so testers can try one feature before it's merged. Example: a collaborator working on YouTube support (#1) on a `youtube` branch, with several testers switching to it.

Testers shouldn't need Rust, and trying a branch must never put their stable install or its data at risk.

## Options considered

### Where the builds live

- **GitHub Actions artifacts** (`upload-artifact`), with a job deleting old ones. Rejected: downloading an artifact needs a GitHub login even on a public repo, they come zipped and expire, and the in-app updater can't fetch them without a token. (Cleanup wouldn't need a job anyway: `retention-days` expires them.)
- **Building on the tester's machine** (require rustup and cargo, compile the branch). Rejected as a feature: on Windows it needs the Visual Studio Build Tools (several GB) and a cold build of this dependency tree (eframe, vendored Lua, bundled SQLite, stylua) takes minutes. It stays available to developers with no code at all: `cargo install --git https://github.com/Dissssy/synththing --branch <name>`.
- **Rolling prerelease GitHub Releases** (chosen). One release per channel, tagged `nightly` or `branch-<name>`, marked as a prerelease. Each build moves its tag to the new commit and replaces the files (`gh release upload --clobber`), so only the newest build exists: nothing to clean up, no storage quota, no expiry. Downloads need no login, and GitHub records each file's sha256, which `src/updater.rs` (`verify`) already checks. Stable users aren't affected: the updater asks for `/releases/latest`, which skips prereleases.

### Runners

The repo is public, so GitHub-hosted runners cost nothing; a self-hosted runner wouldn't save minutes. It would also be a security risk, since `ci.yml` runs on `pull_request` and a fork's PR would run its code on the runner's host (GitHub advises against self-hosted runners on public repos). And the existing self-hosted runner is Linux only, while the Windows exe would need cross-compiling (`cargo-xwin`) with the tray, `winresource` and `windows-sys` dependencies involved. Decision: stay on GitHub-hosted runners. If a self-hosted runner is ever used, only for Linux jobs triggered by pushes, never by `pull_request`.

### Backups vs. separate installs

Backing up the stable exe before switching doesn't protect the user's data. `Config::load` (`src/config.rs`) does:

```rust
Ok(serde_json::from_str(&data).unwrap_or_default())
```

If a branch changes the type of a config field, then going back to stable, stable can't parse the file, silently starts from defaults, and its next save overwrites `soundfonts.json`: soundfonts, settings and theme are gone. Rolling back from a backup also depends on the broken build working well enough to do it.

Decision: **branch builds get an install of their own** (exe and config folder), so stable is never touched and always works as a fallback. Separately, `Config::load` should back up a file it can't read instead of letting it be overwritten; that's worth doing on its own.

## Design

### Versions

Builds from CI report a version that sorts correctly against releases, so the updater and the script library's `runs_on` (`src/library/mod.rs`) need no special cases:

- **Nightly**: the next patch version as a semver prerelease, numbered by commits since the last release tag: `0.7.1-nightly.<N>`, with N from `git rev-list --count v0.7.0..HEAD` (the checkout needs `fetch-depth: 0`). Semver puts `0.7.0 < 0.7.1-nightly.3 < 0.7.1-nightly.9 < 0.8.0`. On master, N also says whether a build is before or after a given commit. This assumes master is never force-pushed (a rewrite reuses numbers).
- **Branch builds**: `0.7.1-branch.<N>`, numbered the same way. The branch's name isn't in the version (a name like `0123` would make it invalid semver); it's the channel.
- **Local builds**: the plain Cargo.toml version, as now.

`build.rs` stamps the version, the channel and the commit from environment variables the workflows set: `SYNTHTHING_CHANNEL` (`nightly` or `branch-<name>`), `SYNTHTHING_BUILD_NUMBER` (N) and `SYNTHTHING_COMMIT`. A channel without a build number, or a channel it doesn't know, fails the build. `src/build_info.rs` reports them. The window title, the About line, the log and `--version` say which build it is ("synththing (youtube, abc1234)"), so screenshots and bug reports do too. The changelog still goes by Cargo.toml's version: it's about releases.

### Script library and nightly builds

`API_SINCE` already gives a new Lua function the version it will ship in (the next release, see docs/RELEASING.md), so a script using it needs, say, `0.8.0`. Every `0.7.1-nightly.N` sorts below that, but the library doesn't compare scripts against the app's version directly: it uses `supported_version()` (`src/library/mod.rs`), which for a build ahead of its version number is the newest version in its `API_SINCE` and `OPTION_SINCE`. So a canary build that has `0.8.0` functions already sees scripts that need `0.8.0`, from the library too.

The catch: a script using a function added after that canary's commit (also `0.8.0`) is offered to it as well, and fails when it calls the function. Updating canary fixes it. Accepted, as is sharing `.lua` files directly for anything else. Bundled scripts are compiled into the exe, so canary users get the newest ones regardless.

The existing workflows already handle the release side: `scripts.yml` skips scripts using functions the newest release doesn't know, and `release.yml`'s `scripts` job publishes them with the new release, which works out their minimum versions.

### Online features

- **Branch builds**: online and library features are off. They're for testing a feature, not for uploading to the real library server. `build_info::online_features()` is false, so `enabled_servers()` is empty (Publish and Remix are disabled, nothing is searched or checked), the Online Library tab and Preferences > Library say so instead, and as a backstop every request through the library client fails before it's sent (a ureq middleware in `library/client.rs`; tests still reach their own server).
- **Canary**: online features stay on. When master changes anything about uploads or the library API (not scripts, covered above), a test server is set up by hand and testers add it as needed.

### Channels in the updater

`updater::Track` is what a build follows:

- **Stable**: `/releases/latest`, as before.
- **Canary** (Preferences > General > Canary updates, `Config::canary_updates`): whichever is higher, `/releases/latest` or the `nightly` release. After 0.8.0 ships, it sorts above every `0.7.1` nightly, so canary installs the release; the next master commit is `0.8.1-nightly.1` and canary moves back to nightlies. Canary only ever moves forward, so it's a setting on the normal install, with no folder of its own (the config wipe above happens only when moving to an older build). Turning it off keeps the build you have until the next release.
- **Branch**: a branch build follows its own `branch-<name>` release, by commit (a rebased branch can count fewer commits). When that release is gone (the branch was merged or deleted), the startup check opens the Updates window, which says so and offers to open the usual synththing (the exe that installed it).

The setting is a bool, not an enum of channels: a build reading a channel name it didn't know would fail to read the whole config.

Branch builds are tried from Help > Check for updates > Branch builds: the list of `branch-*` prereleases (version and commit; the notes, with the branch's commits, on hover), Install (downloads, verifies, installs into the branch's folder and starts it), and Open and Remove for installed ones, including ones whose build is gone.

### Folders

Stable stays where it is. Moving `%APPDATA%\synththing\config` would break anyone who downgrades or runs an exe from 0.7.0 or older (they'd see an empty config), and a migration that moves the user's real config is the riskiest step there could be.

Each branch build gets one folder for everything, its exe and its config, so removing it is deleting one folder (`channels.rs`):

```
%APPDATA%\synththing\config\                         stable and canary (unchanged)
%LOCALAPPDATA%\synththing\data\channels\branch-youtube\
  synththing.exe
  config\                                            its settings
  installed-by                                       the exe that installed it
```

On Linux the same under `~/.local/share/synththing/channels/`. (Not beside the config folder: on Linux its parent is `~/.config` itself. And test installs shouldn't roam.)

A branch build works out its config folder from the channel compiled into it (`config::config_dir`), not from how it's started, so it never touches the stable one however it's launched. Its settings start as a copy of the stable config when it's installed, never synced back; one started some other way (its exe downloaded from the release page) makes the copy on its first launch. Left out: the identity (branch builds are offline), logs, files set aside as unreadable, and the downloads they share instead.

Shared (`config::shared_dir`): the downloaded ffmpeg and the starter soundfonts, which have no format of synththing's own, stay in the stable config folder and branch builds use them there. Anything a build writes in its own format (config, playlists, the song library database, the identity) stays per install, or a branch build could corrupt it for stable.

With `--config-dir` (test runs), branch builds install into `channels/` inside that folder and are started with their config folder in `SYNTHTHING_CONFIG_DIR`, so a test run touches nothing outside it.

### Single-instance lock

Per install, keyed on the config folder (`instance.rs`, #5): stable and a branch build run side by side, two copies of the same one can't. The app holds an exclusive lock on `synththing.lock` in its config folder (let go however it ends); a second launch finds it locked, leaves a `show-request` file and exits, and the running app, which checks for it a few times a second, brings its window back.

### Workflow example: the `youtube` branch

1. The collaborator pushes `youtube` to `Dissssy/synththing` and, when it's ready to try, runs Actions > Test builds > Run workflow on it (or `gh workflow run test-builds.yml --ref youtube`). They need write access: workflows on a fork's PR get a read-only token and can't publish a release (otherwise a maintainer pushes the branch and runs it). Branch builds are manual so work in progress doesn't reach testers by accident.
2. CI builds both platforms and publishes the `branch-youtube` prerelease, its notes listing the commits since `master`, the author and the commit.
3. Testers open Help > Check for updates > Branch builds and press Install next to "youtube". The app installs it in its own folder with a copy of their settings and starts it. Their stable install is untouched; going back is opening the normal shortcut, and the branch build can be opened again from the same list.
4. Running the workflow on `youtube` again rebuilds the release from its newest commit, and the branch install updates itself at its next launch.
5. When `youtube` is merged and deleted, a workflow deletes the release and tag. The branch build says so at its next launch and offers to open the usual synththing, where the branch build can be removed.

## Plan

1. `Config::load`: back up a config file it can't parse instead of overwriting it.
2. Version stamping in `build.rs` (nightly, branch, local) and the build shown in the title bar and About box.
3. Workflows: nightly on push to master, branch builds (run by hand) into `branch-<name>` prereleases, deleting a branch's release when the branch is deleted.
4. Updater: the channel setting, canary following the higher of nightly and the latest release, branch installs into `channels\` folders, and the "this branch is gone" check.
5. Branch builds: online and library features off.
6. Docs: RELEASING.md (channels, versions, the branch workflow), the changelog, and this file updated to describe what was built.
7. Later, separately: the single-instance rework, keyed per channel.
