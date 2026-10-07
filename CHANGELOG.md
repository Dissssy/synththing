# Changelog

What changed in each version of synththing, newest first. The app shows the new parts after an update (and all of it under Help > Changelog...).

## Unreleased

### New
- Colored chips in the Online Library: each script's kind (visualizer, game, toy, example) and each song's source (original, video game, film or TV, anime, popular, classical, folk) has a color, shown as a chip in the list and the details, and as a dot in the dropdowns that pick them. They're from the same palette as the channel toggles.

## 0.5.1 (2026-10-07)

### New
- Channel toggles in color: the toggles above the visualizer are filled with each channel's color while it's on, and outlined in it while it's off. They're the keyboard visualizer's colors (as in song previews), unless the script says which it draws each channel in (`channel_colors`).

## 0.5.0 (2026-10-07)

### Fixed
- On Windows, the app has room to read very large scripts (in the editor, or publishing one) without crashing: its main thread's stack is 16 MB, as on the threads that run scripts, instead of Windows' 1 MB.

## 0.4.9 (2026-10-07)

### Fixed
- The scripts that come with synththing (keyboard, disco, the games...) can be updated again on Windows: they were added with Windows line endings, so they never matched a version on the official server and stayed at "version 0" with nothing to update to. Copies added that way are put right by their next update check.

## 0.4.8 (2026-10-07)

### New
- Library servers can host songs (MIDI files), when their owner turns it on (`"songs": true` in `server.json`): each upload declares the poster's right to share it, says where it's from, and is checked by its notes, so the same song posted again by someone else isn't taken.
- Songs in the Online Library: a Songs side beside Scripts, for the servers that take them, with each song's credits, where it's from and the rights it's shared under. Download saves one in a Library folder beside your songs, and the new Downloaded songs tab (View menu) plays them, shows them in their folder, or deletes them.
- Publish song..., in the song browser's right-click menu: share a MIDI file on a library server that takes songs. Its title, composer and arranger are filled in from the file when it says.
- Song previews: the selected song plays its liveliest stretch on the keyboard visualizer, muted; click it to hear it with your soundfont.

### Changed
- The Script Library is the Online Library now, since it has songs too.
- The keyboard visualizer's keys keep a grand piano's shape (about 6.4 times as long as they're wide) whatever the window's, instead of always taking 28% of its height.

### Fixed
- A preview's "Loading..." (or "No preview.") space no longer overlaps what's below it in the Online Library.
- Notifications stop asking a library server that doesn't have them (one running an older version), instead of trying every half minute.

## 0.4.7 (2026-10-07)

### New
- Copyright notices on library servers: Report... has "It's my copyrighted work", a legal notice (as the US DMCA has them) that takes the script down at once. Its uploader is notified, and can dispute it (a counter-notice) or let it stand; moderators decide disputes (in Moderation's new Copyright tab), an upheld one keeping the script safe from further notices. Takedowns that stand can't simply be published again, and three of them get an identity banned.
- Notifications you haven't read stand out in the list, and once you've acted on one (disputed a notice, say), it says what you did instead of offering it again.
- Notifications from library servers: the envelope at the right of the header turns red with a count when there's news about your things (encores and installs adding up, a script hidden or back, a remix of yours, a report of yours dealt with; for moderators, new reports). Click it for the list, every server's together, and one for what can be done about it. Preferences > Library > Notifications turns it off.
- `--config-dir DIR` (or `SYNTHTHING_CONFIG_DIR`) keeps everything in a folder of its own, for trying a build without touching your real setup.

### Changed
- The Script Library searches again each time it's opened, so it's always current.
- Publishing ends with what was published and a button to open it in the Library, instead of the window just closing.

## 0.4.6 (2026-10-06)

### New
- Library servers can offer documents to read, like their EULA and privacy policy: icon buttons in the Script Library (hover for which) open them. The official server has both.
- Delete my data... (Preferences > Library) has a library server delete what it has of yours: your scripts, encores, admin rights and email. It's out of sight at once and gone for good after 30 days; until then, Restore my data puts it back. Where your identity has an email attached, a code sent to it confirms it.
- Library servers can be updated by their admins: the Moderation window's Server tab (and `synththing admin update`) says when a newer release is out and updates to it.

## 0.4.5 (2026-10-06)

### New
- Adding soundfonts takes several at once: pick as many as you like in the file dialog (or drop them on the window), and the status line says how many were added and what went wrong with any that weren't.
- The official scripts' publisher key can be replaced, should it ever get out (`synththing publisher-rotate`); the app learns the new one from the official server, so official scripts stay marked and updatable.

## 0.4.4 (2026-10-06)

### New
- A recovery email for your library identity (Preferences > Library): attach one, and if you lose your key, Recover... gets your identity back with a code sent to it. The server keeps only a hash of the address.
- New key... moves your identity to a fresh key, if the old one might have got out: your scripts, encores and admin rights follow it, and the old key stops working. Other servers catch up the next time the app talks to them.

## 0.4.3 (2026-10-05)

### New
- Encores, in the Script Library: say you liked a script (and take it back); they add up on its author's page.
- Report... in the Script Library: tell a server's moderators what's wrong with a script, pointing at the lines of its code and the sprites in it that are the problem.
- Remixes: Publish remix... (by Update, for a script installed from someone else) shares your take on it, credited to the original. A script lists its remixes, and a remix links back to what it came from (or says it was remixed from a deleted script), even on another server: following the link asks to add that server (or turn it on) if it isn't in use.
- Library servers refuse a near-copy of someone else's script (or of one that comes with synththing) unless it's published as a remix of it, and a remix with nothing changed. A copy of a bundled script is published as a remix of it by itself.
- Presets: `settings_preset(name, {...})` in a script offers named sets of setting values, picked at the top of the Settings window (with Defaults). Save your own there too, and publishing offers to add your settings to the script as a preset. highway.lua has Relaxed, Expert and Focus; settings_demo.lua shows how.
- Moderation, for a library server's admins (in the Script Library): reports with what they point at highlighted, hiding, deleting, bans (a key or an address, for a while or for good; a key's ban can follow it to the addresses it uses) and admins. `synththing admin` does the same on the server itself.

## 0.4.2 (2026-10-05)

### New
- Previews: the Script Library shows each script playing, a few looping seconds picked from the liveliest part of the starter songs (a game is seen playing itself). The server makes them after each upload.
- The script picker has a sidebar: the script under the mouse, playing, with where it's from, who made it and its description. Scripts without a preview from a library get one made here the first time they're looked at (and again after they're changed).
- Library servers refuse a script that doesn't run (it doesn't compile, or errors in its first seconds), saying why.
- `synththing preview` makes a script's preview from the command line (`--check` only checks it runs).

## 0.4.1 (2026-10-05)

### New
- Render, in the Record window: the video is made in the background, frame by frame, by its own copy of the script and the synth, with the visualizer showing its progress (and a glimpse of it) meanwhile. Every frame is drawn and the sound is exactly in step at any size, so frames never repeat, whatever the script or the encoder can manage live. A song or a whole playlist; Stop keeps what's done.
- A saved recording says how big the file is.
- The Script Library (View > Script Library): search scripts shared on library servers, install them, and publish your own. The official one is synththing.p51.nl; more can be added in Preferences > Library, and anyone can run one with `synththing serve`, directly or in Docker (`ghcr.io/dissssy/synththing`, docs/DEPLOY.md).
- Library identities: what you publish is signed with a key the app makes (no account, no email), shown as an ID like `#k3f9q2xa`. Publishing the same script again (by its slug) adds a new version, only you can delete your uploads, and clicking an author's ID lists everything they've posted. Export and import it (sealed with a passphrase) in Preferences > Library. Posting anonymously is still an option, except on servers that only take signed uploads.
- The visualizer's script row has Publish... (your scripts) or Update (scripts installed from a library): an update shows what the author changed and, separately, what you changed in your copy; changes that don't overlap can be kept. Restore original undoes your changes to an installed script.
- The bundled scripts are also on the official library, published from the repository whenever they change, so fixes and new versions arrive through Update without waiting for a release. Scripts get the newest version your app can run: each version records the oldest synththing it needs.

### Fixed
- A live recording's "frames repeating" warning blamed the script even when the encoder was the one behind, and didn't count frames skipped for the encoder at all. It now says which.

## 0.4.0 (2026-10-05)

### New
- Recording moved to synththing > Record..., a window to pick what to record (a song from the start, the one playing or any other, and for MIDI its soundfont; the playlist; or free), which script, the video's size, frame rate and quality, and for scripts that can play themselves, Auto or Manual. F9 still starts a free recording at once, and a recording's Pause and Stop stay above the visualizer.
- Scripts can take part: `recording()` says what's being recorded and whether to play themselves, and with `script_options({ record_prepare = true })` a script gets ready before each song (nothing recorded meanwhile), says `recording_ready()` to start the video and `recording_done()` to end each take. Playlists move on only once each take is over.
- `run-script --video` records prepared scripts the same way, and takes `--auto`.
- Vertical (720 x 1280, 1080 x 1920) and square recording sizes for phone-shaped videos, and Custom for any size (also `--video-size vertical`, `square` or `WIDTHxHEIGHT`).
- The Script Settings tab is gone, to leave room: Settings, Controls and Debug buttons above the visualizer each open a window laid out like Preferences. Scripts can sort their settings and controls into groups and explain them on hover: `{ group = "Movement", info = "..." }` as the last argument of `setting_*` and `input_register` (gamepad_demo and settings_demo show it).
- In Controls, each binding is a button: click to remove it, right-click to rebind it in place. + adds one and waits for its key.
- Debug has the script's variables, live: its top-level locals and its own globals, with tables opened one level at a time (50 entries, then more on request), plus its last `debug_locals()`, its log, and how long it takes to draw.
- Scripts get Lua's `coroutine` library, to spread big jobs over frames (the watchdog covers coroutines too).
- highway.lua: big songs are read and charted a bit per frame with a progress line, so the visualizer no longer freezes (a 280,000-note song that used to hit the watchdog charts in a few seconds); recordings start when the game does and end after a few seconds of results (or on leaving them), and in Auto it plays itself (the track last played, else PLAY ALL) without keeping the score, the take ending with the song. Its controls and settings are grouped.
- highway.lua: the track menu plays the song (scroll to seek, Space to pause) and SOLO (I) plays only the selected track, so you can tell which is which. GUITAR mode (G) plays it like Guitar Hero: five frets held and a strum (Up/Down, or a guitar controller's strum bar), with HOPOs and taps that can be fretted without strumming.

## 0.3.3 (2026-10-03)

### New
- An experimental Linux build on each release, `synththing-linux-x86_64`, which updates itself like the Windows one. It builds and passes every test on Ubuntu, but hasn't been played with yet: reports welcome (docs/LINUX.md).

## 0.3.2 (2026-10-03)

### New
- Mouse buttons can be bound to script actions (`"mouse_left"`, `"mouse_right"`, `"mouse_middle"`), and captured in the Controls list by clicking.
- `playback().recording`: whether the visualizer is being recorded, so a script can keep hints or overlays out of the video.
- snake.lua and keyboard.lua take a controller: the d-pad (and the left stick, in snake) steers, or moves the span and key.

## 0.3.1 (2026-10-03)

### New
- `input_value(id)` for scripts: how far an action is pressed, 0 to 1 (keys 0 or 1, triggers and stick directions how far), so analogue steering stays rebindable. gamepad_demo.lua shows it.
- Sprite Editor: line (L) and rectangle (U, Shift for filled) tools, and Flip H / Flip V, which mirror each frame of a sheet in place.

## 0.3.0 (2026-10-03)

### New
- A welcome window the first time synththing runs, offering a starter pack: two soundfonts (GeneralUser GS and TimGM6mb, downloaded in the background and ready to use) and four songs to try, arranged for synththing with each instrument on its own channel. Help > Welcome... brings it back.
- A guided tour (offered by the welcome window, and Help > Tour): it points at the real buttons one at a time, with everything else dimmed, and moves on as you try each thing: playing a song, the controls, soundfonts, visualizers, channels, playlists, a soundfont for one song, fullscreen, and a game.
- Themes (synththing > Themes..., a window of its own that stays on top while you look around the app): Dark (the new default), Light, Frutiger Aero and Programmer art (egui's own look). Duplicate one to make your own: every color, the corner roundness, borders and shadows, changed live. The UI scale is there too, and Ctrl + / Ctrl - / Ctrl 0 change it anywhere.
- A What's new window after an update, listing what changed since the version you had; Help > Changelog... shows every version.

### Changed
- The File menu is now the synththing menu (Preferences, Themes, Quit).
- Preferences is organized into categories (General, Songs, Loading, Scripts, Recording, Experimental), one line per setting; the longer explanations show on hover, wherever there's an (i).
- Only MIDI files are listed as songs by default; Preferences > Songs > "Show audio files" lists MP3, WAV and the rest too (notes-based features don't apply to them).
- The Listening layout (the first one you see) has the songs and soundfonts on the left and the visualizer on the right. Playlists are a View menu click away (the tour opens them).

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
