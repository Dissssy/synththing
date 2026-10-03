# Changelog

What changed in each version of synththing, newest first. The app shows the new parts after an update (and all of it under Help > Changelog...).

## Unreleased

### New
- A welcome window the first time synththing runs, offering a starter pack: two soundfonts (GeneralUser GS and TimGM6mb, downloaded in the background and ready to use) and four songs to try, arranged for synththing with each instrument on its own channel. Help > Welcome... brings it back.
- A What's new window after an update, listing what changed since the version you had; Help > Changelog... shows every version.

### Changed
- Only MIDI files are listed as songs by default; Preferences > Songs > "Show audio files" lists MP3, WAV and the rest too (notes-based features don't apply to them).
- The Listening layout (the first one you see) shows the soundfonts below the songs.

## 0.2.2 (2026-10-03)

### New
- Visualizer scripts run on their own thread: a slow or stuck script no longer freezes the window, menus or playback. When a frame takes over a second, a notice in the visualizer's corner says so, with a Stop button.
- A watchdog stops a script whose code runs for over 10 seconds without finishing (an endless loop, say), with an error saying so. Restart (F5) or saving a fix runs it again.
- A warning before playing a very large MIDI file (over 100,000 notes), with "don't warn me again"; such files get a red ! in the song lists.
- Script logs can go to the app's log (Help > Log...): a Preferences option for all scripts, `script_options({ app_log = true })` for one script, or `log_app(message)` for single messages.
- `synththing --console` prints the app's log to the terminal as it runs.

### Changed
- Picking another script, applying an edit or restarting stops a script that's been busy for over a second first, instead of waiting for it.

## 0.2.1 (2026-10-03)

### New
- Game controllers: buttons, the d-pad, triggers and stick directions can be bound to a script's actions like keys, as defaults or in the Controls list. `pad_axis()` reads sticks and triggers directly, and `gamepads()` lists what's connected.
- gamepad_demo.lua (an example): a drawn controller lighting up as yours is used.
- An updated highway.lua.

### Changed
- The whole Script Settings tab scrolls, so a long Controls list no longer squeezes the rest.

## 0.2.0 (2026-10-03)

### New
- Scripts can drive playback through the current playlist: `playlist()`, `play_track`, `next_track`, `previous_track`, `set_speed`, `set_loop` and `set_shuffle`. Only songs already in the playlist; volume stays yours.
- `script_options({ start_paused = true })`: songs load paused while that script runs, for games with their own start button. `playback().song_loads` counts every load, the same song again included.
- highway.lua, a Guitar Hero style game charted from any song.
- player.lua template: a mouse-driven player drawn by a script.
- Bundled scripts are grouped into visualizers, games, templates and examples. New visualizers and games arrive with updates; ones you deleted stay deleted.

### Changed
- game.lua is now note_runner.lua.

### Fixed
- Muting or unmuting a channel no longer skips the song (and the visuals) forward.
- A script reading the new song's notes as it loaded could get the previous song's length.

## 0.1.9 (2026-10-03)

### New
- Record the visualizer to video (MP4 with sound): this song, this playlist, or until you stop, at 720p to 4K and 30 or 60 fps, in standard, sharp or lossless quality. F9 starts and stops, F10 pauses. ffmpeg is downloaded on first use. Also from the command line: `run-script --video`.
- The script editor got line numbers, auto-indent, bracket and quote pairing, comment toggling (Ctrl+/), find and replace (Ctrl+F, Ctrl+H), a choice of when edits apply, and a history of earlier versions to restore.
- The editor understands Lua: problems underlined as you type (unknown names, unused locals, accidental globals), completions, signature help, go to definition (F12), an outline, and Format (Shift+Alt+F).
- Color swatches in the editor, with a picker, and links from function names into the reference.
- A Sprite Editor tab: draw a script's sprites and sprite sheets with pencil, fill and picker, and edit their palettes; changes are written back into the script.
- An Insert menu of snippets: color tables, settings, controls, a text field, sequences, a new sprite, and more.
- Layouts: built-in ones (Listening, Watching, Script writing, Sprite editing) and your own saved by name, under View > Layout. A layout keeps your changes, and Reset puts a built-in one back.
- Sprites can be recolored per draw (`palette` swaps, `tint`), and `playback()` has `song_name`, `song_path` and `song_id`.
- The game example script (now note_runner.lua), a platformer whose level is the music.

### Changed
- Visuals line up with what you hear at any audio buffer size, and notes a script plays show up in the waveform, spectrum and levels.

### Fixed
- Audio right after a seek could be dropped, and a song was marked finished slightly before its end was heard.

## 0.1.8 (2026-10-02)

### New
- Sprites for scripts: palette-indexed images, scaled and flipped, and sprite sheets.
- Rebindable controls: scripts register actions with default keys, and the Controls list in Script Settings rebinds them.
- Typing spans for text input in scripts, and a terminal.lua template.
- Scripts can play notes on the song's instruments (`play_note`, `note_on`, sequences). keyboard.lua lets you play along with A to L.
- A render-time readout in Script Settings; a script too slow for 60 fps drops to 30 fps.
- `run-script --screenshot` saves the last frame as a PNG.
- The disco example visualizer.

### Changed
- snake, keyboard, fft and spectrogram use the new features, and pulse.lua is new.
- `active_notes()` includes notes the script plays, marked as such.

## 0.1.7 (2026-10-02)

### New
- Song lengths in the Songs browser and playlists, a playlist total, and an info window per song (tracks, tempo, channels, tags and more).
- Playlists: multi-select, dragging folders in, and .m3u import and export.
- A README, and synththing is licensed under the WTFPL.

## 0.1.6 (2026-10-02)

### New
- Musical timing for scripts: `beat()`, `bar()`, `tempo()`, `time_signature()` and `time_at_beat()`.
- Audio levels and onset (beat) detection for scripts.
- Filled shapes: `circle()`, `triangle()` and `polygon()`.

## 0.1.5 (2026-10-02)

### New
- Pixel text for scripts (the Monogram font): `text()` and `text_size()`.
- Help > Credits & licenses.
- `synththing run-script`: test a script without a window, from the command line.

## 0.1.4 (2026-10-02)

### New
- `notes_between()`: every note in a stretch of the song, with start and end times.
- Scripts can pause and seek, and save their own data (`store_get`, `store_set`).
- `FRAME` and `TIME`, and `playback().generation` to notice seeks and loops.

## 0.1.3 (2026-10-02)

### New
- Loop and Shuffle are remembered between runs.
- The line a script error points at is underlined, with a Go to line button.
- Rename scripts, settings included.
- The Songs and scripts folders refresh by themselves when files change, and a script edited elsewhere reloads.

## 0.1.2 (2026-10-02)

### New
- An app log (Help > Log...), also written to a file, so problems in release builds can be looked into.
- The scripting reference was rewritten and is easier to read.

## 0.1.1 (2026-10-02)

The first release: a MIDI and audio player with soundfonts, playlists, a dockable tab layout, Lua-scripted visualizers with a script editor, mouse and keyboard input for scripts, a dedicated fullscreen visualizer, background loading, and updates from GitHub.
