# Changes for the next release

Each pull request with a user-facing change adds its own file here, named after its branch (`claude/clear-got-wishes` adds `clear-got-wishes.md`), with its lines under the groups they belong to:

```markdown
### Added
- Wishlist: a broom button on the Got it heading takes every wish that came true off the wishlist at once.

### Fixed
- Seeking in a MIDI song is instant now, even far into a long one.
```

The groups are Added, Changed, Fixed and Removed. Write for people using the app, not for the code. One file per pull request means two pull requests never edit the same lines, so they never conflict over the changelog.

Builds show these under Unreleased in Help > Changelog... (`build.rs`, `src/changelog.rs`). Cutting a release (`release.toml`) gathers every file here into a new version section of `CHANGELOG.md`, each group once, and deletes them. This README stays.
