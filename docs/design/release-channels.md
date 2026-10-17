# Release channels: canary (nightly) and branch builds

Status: design, not built yet. Tracked in #3.

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
- **Branch builds**: `0.7.1-<branch>.<short hash>`, for display only.
- **Local builds**: the plain Cargo.toml version, as now.

`build.rs` stamps the version and commit hash from environment variables the workflows set. The build is shown in the title bar and the About box ("youtube · abc1234"), so screenshots and bug reports say which build they came from.

### Script library and nightly builds

`API_SINCE` already gives a new Lua function the version it will ship in (the next release, see docs/RELEASING.md). A script using it needs, say, `0.8.0`, and every `0.7.1-nightly.N` sorts below that, so nightly users don't get it from the library until the release: nobody is offered a script their build can't run. Bundled scripts are compiled into the exe, so canary users get the newest ones anyway.

Accepted gap: a script someone publishes from a canary build using a brand-new function isn't visible to other canary users until the release. Sharing the `.lua` file directly covers that.

The existing workflows already handle the release side: `scripts.yml` skips scripts using functions the newest release doesn't know, and `release.yml`'s `scripts` job publishes them with the new release, which works out their minimum versions.

### Online features

- **Branch builds**: online and library features are off. They're for testing a feature, not for uploading to the real library server.
- **Canary**: online features stay on. When master changes anything about uploads or the library API (not scripts, covered above), a test server is set up by hand and testers add it as needed.

### Channels in the updater

A channel setting under updates:

- **Stable**: `/releases/latest`, as now.
- **Canary**: whichever is higher, `/releases/latest` or the `nightly` release. After 0.8.0 ships, it sorts above every `0.7.1` nightly, so canary installs the release; the next master commit is `0.8.1-nightly.1` and canary moves back to nightlies. Canary only ever moves forward, so it can be a setting on the normal install, with no folder of its own (the config wipe above happens only when moving to an older build).
- **Branch**: picked from the `branch-*` prereleases (name, author, age, commit). Installs into its own folder and launches from there. Its updater follows that branch by commit hash. When the branch's release is gone (merged and deleted), the branch build says so at launch and offers to switch to canary or stable and delete its folder.

### Folders

Stable stays where it is. Moving `%APPDATA%\synththing\config` would break anyone who downgrades or runs an exe from 0.7.0 or older (they'd see an empty config), and a migration that moves the user's real config is the riskiest step there could be.

```
%APPDATA%\synththing\
  config\                     stable (unchanged), and canary
  shared\                     downloaded ffmpeg, caches: anything with no format version
  channels\
    branch-youtube\config\
%LOCALAPPDATA%\synththing\channels\branch-youtube\synththing.exe
```

What goes in `shared\` is decided per item: only files with no format of synththing's own (downloaded tools, caches). Anything a build writes in its own format (config, playlists, the song library database, the identity) stays per channel, or a branch build could corrupt it for stable. Branch builds have online features off, so they don't need the identity.

A branch install's config starts as a one-time copy of the stable config (so the tester's soundfonts and settings come along) and never syncs back. A "copy settings from stable again" button covers testers who change something on stable later.

### Single-instance lock

Due for a rework regardless (it's unreliable now). For this design, the lock is per channel, keyed on the config folder: stable and a branch build can run side by side, two copies of the same channel can't.

### Workflow example: the `youtube` branch

1. The collaborator pushes `youtube` to `Dissssy/synththing`. They need write access: workflows on a fork's PR get a read-only token and can't publish a release (otherwise a maintainer pushes the branch).
2. CI builds both platforms and publishes the `branch-youtube` prerelease, its notes listing the commits since `master`, the author and the commit.
3. Testers pick "youtube" under the update channel. The app installs it in `channels\branch-youtube\` with a copy of their config and launches it. Their stable install is untouched; going back is opening the normal shortcut.
4. New pushes to `youtube` rebuild the release, and the branch install updates itself.
5. When `youtube` is merged and deleted, a workflow deletes the release and tag, and the branch install offers to switch to canary or stable and remove itself.

## Plan

1. `Config::load`: back up a config file it can't parse instead of overwriting it.
2. Version stamping in `build.rs` (nightly, branch, local) and the build shown in the title bar and About box.
3. Workflows: nightly on push to master, branch builds into `branch-<name>` prereleases, deleting a branch's release when the branch is deleted.
4. Updater: the channel setting, canary following the higher of nightly and the latest release, branch installs into `channels\` folders, and the "this branch is gone" check.
5. Branch builds: online and library features off.
6. Docs: RELEASING.md (channels, versions, the branch workflow), the changelog, and this file updated to describe what was built.
7. Later, separately: the single-instance rework, keyed per channel.
