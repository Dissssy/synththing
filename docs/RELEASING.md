# Releasing synththing

Releases are built by GitHub Actions (`.github/workflows/release.yml`) when a `vX.Y.Z` tag is pushed, and the in-app updater offers them to everyone on an older version. `cargo release` makes the tag.

## Before every commit or push

- **Docs match the code.**
  - `docs/scripting-reference.md` (the in-app Scripting Reference) covers every Lua function, global and behavior scripts can see, with units. When the Lua API changes, also update the one-line hover help in `src/lua_completion.rs` (`HOST_API`) and the function list in the blank script template (`BLANK_SCRIPT_TEMPLATE` in `src/lua_visualizer.rs`).
  - `docs/cli.md` covers every command-line command and option, and agrees with `synththing --help` (generated from `src/cli.rs`).
- **Checks pass:** `cargo build`, `cargo clippy` (no warnings), `cargo test --bin synththing`.
- **Dependencies changed?** Regenerate the license list with `cargo run --example gen_licenses` (the `license_list_matches_the_dependency_tree` test fails until you do). A dependency licensed under something with no text in `assets/licenses/spdx/` makes the generator stop and say which one to add.

## Making a release

```
cargo release patch            # dry run: shows what it would do (or minor / major)
cargo release patch --execute  # bumps Cargo.toml, commits "Release X.Y.Z", tags vX.Y.Z, pushes
```

Then watch the run (`gh run list`, `gh run watch`): it checks the tag matches `Cargo.toml`, runs the tests, builds the release exe and publishes the GitHub Release with `synththing-windows-x86_64.exe`. Running copies pick it up at their next launch (or Help > Check for updates).

`release.toml` keeps cargo-release from publishing to crates.io (`Cargo.toml` also has `publish = false`) and only allows releases from `master`.
