# Releasing synththing

Releases are built by GitHub Actions (`.github/workflows/release.yml`) when a `vX.Y.Z` tag is pushed, and the in-app updater offers them to everyone on an older version. `cargo release` makes the tag.

## Before every commit or push

- **Docs match the code.**
  - `docs/scripting-reference.md` (the in-app Scripting Reference) covers every Lua function, global and behavior scripts can see, with units. When the Lua API changes, also update the one-line hover help in `src/lua_completion.rs` (`HOST_API`) and the function list in the blank script template (`BLANK_SCRIPT_TEMPLATE` in `src/lua_visualizer.rs`).
  - `docs/cli.md` covers every command-line command and option, and agrees with `synththing --help` (generated from `src/cli.rs`).
- **New Lua functions have a version.** Every entry in `HOST_API` needs one in `API_SINCE` (`src/library/mod.rs`; a test checks), and so does every new `script_options` key in `OPTION_SINCE`. Use the version it will ship in (the next release), not the one in Cargo.toml: the library uses it to keep scripts that need it away from apps that don't have it.
- **The changelog is current.** User-facing changes get a line under `## Unreleased` in `CHANGELOG.md` (New / Changed / Fixed), written for people using the app, not for the code. The app shows it after an update and under Help > Changelog....
- **Checks pass:** `cargo build`, `cargo clippy` (no warnings), `cargo test --bin synththing`.
- **Dependencies changed?** Regenerate the license list with `cargo run --example gen_licenses` (the `license_list_matches_the_dependency_tree` test fails until you do). A dependency licensed under something with no text in `assets/licenses/spdx/` makes the generator stop and say which one to add.

## Bundled scripts on the library

The scripts in `assets/visualizers` (not the templates) are also published to the official script library, each by its slug (its file name), signed with the official publisher key, so a changed script becomes its next version and apps get it through Update:

- On every push to master that changes them (`.github/workflows/scripts.yml`): the newest release's binary publishes them from the checkout (`publish-bundled --dir`), without building anything. A script using a function that release doesn't have is skipped.
- With every release (`release.yml`'s `scripts` job): the release's own binary publishes its built-in scripts, including any skipped before, once the official server runs that release (its update timer installs it within the hour; the job waits). That needs the `SYNTHTHING_PUBLISH_KEY` repository secret (the publisher's secret key, in hex; its public half is `OFFICIAL_PUBLISHER` in `src/library/mod.rs`); without it the workflow does nothing. By hand: `SYNTHTHING_PUBLISH_KEY=... synththing publish-bundled`.

## Making a release

```
cargo release patch            # dry run: shows what it would do (or minor / major)
cargo release patch --execute  # bumps Cargo.toml, commits "Release X.Y.Z", tags vX.Y.Z, pushes
```

cargo-release renames `## Unreleased` in `CHANGELOG.md` to the new version's heading, dated, with an empty `## Unreleased` above it (`pre-release-replacements` in `release.toml`). Then watch the run (`gh run list`, `gh run watch`): it checks the tag matches `Cargo.toml`, runs the tests, builds the release exe and publishes the GitHub Release with `synththing-windows-x86_64.exe`, its notes taken from the version's section of `CHANGELOG.md` (the run fails if there isn't one). Running copies pick it up at their next launch (or Help > Check for updates).

`release.toml` keeps cargo-release from publishing to crates.io (`Cargo.toml` also has `publish = false`) and only allows releases from `master`.
