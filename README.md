# synththing

A music player for Windows (with Linux and Android builds, both experimental) that keeps your whole library, MIDI included, tidy and in step across your devices, with a visualizer you can script in Lua. What it does, in plain words: [synththing.p51.nl](https://synththing.p51.nl). The technical docs: [synththing.p51.nl/docs](https://synththing.p51.nl/docs/) (the same as the `docs` folder).

- Plays audio files (MP3, WAV, OGG, Opus, FLAC, M4A/AAC/ALAC, AIFF, WMA, and more through ffmpeg) and MIDI files through any SoundFont (`.sf2`).
- A song library: synththing keeps its own copy of each song, with columns to sort and group by, your own colored tags, starred songs, album art, and a search that takes filters (`artist:nujabes year:>2000 -type:mid`).
- Smart playlists from rules, and Groupings: a page for every album, artist, genre or anything else a ruleset splits by. An Up next queue for what plays next.
- Song details you can edit, one song or many at once, with Clean up suggestions (artists in one, names spelled two ways, track numbers in titles) and a background lookup on MusicBrainz that fills in what's missing and finds album covers.
- Sign in with a QR code, and your settings, playlists and songs sync between your devices, encrypted on the device so the server can't read them.
- Every action has a keyboard shortcut you can change, a command palette (Ctrl+Shift+P), and the system's media keys and controls work.
- A local API for your own programs: see and control what's playing, edit playlists, and stream the visualizer into OBS (docs/API.md).
- Playlists: build them by dragging songs or whole folders in, reorder, shuffle and loop, give MIDI tracks their own soundfont, import and export `.m3u`.
- A visualizer driven by Lua scripts: waveform, spectrum, piano roll, spectrogram, and games that play along: a Guitar Hero style highway charted from any song, a snake game, a platformer, a piano you can play on with your keyboard, and Doom (bring your own WAD). Write your own with a live-reloading editor: scripts can take mouse, keyboard and game controller input (with rebindable controls), take typed text, play notes on the song's instruments and their own sound effects, and read data files from their own assets folder, in a window or fullscreen.
- A sprite editor for the pixel art scripts draw: paint, fill, lines, rectangles, flips, palettes and sprite sheets, written straight back into the script.
- Record the visualizer with its sound to a video (MP4, 720p to 4K, vertical or any size, 30 or 60 fps): a whole song, a whole playlist, or whatever you play, live or rendered in the background frame by frame. Uses ffmpeg, which synththing offers to download the first time.
- Rearrangeable layout: every section is a tab you can drag, split or pop out.
- Themes: Dark, Light, Frutiger Aero and egui's own look, or make your own from any of them (every color, corners, borders, shadows), plus a UI scale.
- A welcome window the first time, with a starter pack (two soundfonts and four songs) and a guided tour of the app.
- A script library: find and install scripts other people have shared, publish your own (signed with a key the app makes; no account), and get updates to them. Anyone can run a library server (docs/DEPLOY.md).
- Updates itself: new releases are offered in the app.

## Getting it

1. Download `synththing-windows-x86_64.exe` from the [latest release](https://github.com/Dissssy/synththing/releases/latest).
2. Run it. It's a single file, no installer; put it wherever you like.
3. The first time, Windows may show "Windows protected your PC", because the program isn't code-signed. Click **More info**, then **Run anyway**.

After that, synththing checks for new versions when it starts and asks before installing one (you can turn that off in synththing > Preferences, or check any time from Help > Check for updates). To get new things early, turn on Canary updates in Preferences: the newest build from master, and releases once they're out. To try a feature before it's merged, install its branch build from Help > Check for updates > Branch builds; it runs separately, with a copy of your settings, so your usual synththing isn't touched.

It needs 64-bit Windows 10 or 11. There's an experimental Linux build on each release too, `synththing-linux-x86_64` (`chmod +x` it, then run it); see [docs/LINUX.md](docs/LINUX.md) for what's known and what still needs testing. An experimental Android app is there too, `synththing-android-arm64.apk`; see [docs/ANDROID.md](docs/ANDROID.md).

## First steps

The first time synththing runs, a welcome window offers a starter pack (the soundfonts download in the background; the songs go into your library) and a tour that walks you through all of this on the real buttons. Help > Welcome... and Help > Tour bring them back. Or, by hand:

1. **Pick your music.** The Songs tab is your library: synththing keeps its own copy of each song, so moving or renaming the originals never breaks a playlist. **+** imports songs, the folder button a whole folder (subfolders too), and the search box finds them by name or tag, or filters them (`type:mid`, `artist:nujabes`, `year:>2000`, `is:starred`...: hover it for the list, Tab completes them, and the engine button beside it saves the search as a smart playlist). Click a column header to sort, right-click one to choose columns or group the songs. Click a song to play it (Ctrl+click or Shift+click to select songs instead); dropping a file on the window plays it without importing it. Right-click a song (or several) for Play next and Add to queue (the Up next button by Shuffle opens the Up next tab, the queue with the Songs list's columns, where a click plays a song now; the playlist or list you were playing from carries on after it), Song info, where you can edit their title, artist, album and more, Tags (your own colored labels), Save a copy as... and Remove from library. The broom in the Songs toolbar suggests clean-ups for your songs' details, like splitting "Artist & Guest" into two artists.
2. **Add a soundfont for MIDI.** MIDI files are just notes; a SoundFont supplies the instruments. In the Soundfonts tab, **+** adds one: pick any General MIDI `.sf2` (plenty are free online). Compressed `.sf3` soundfonts aren't supported; Polyphone can convert them to `.sf2`.
3. **Make a playlist.** Open Playlists in the Groupings tab and drag songs in from Songs (or files and folders from outside, which are imported); dropping them on the playlists themselves starts a new one. Drag a soundfont onto a MIDI entry to give it its own.
4. **Watch the visualizer.** It's on the right to begin with (View > Visualizer if you've closed it); pick a script above it. The corners button (or F11) fills the screen with it; Esc comes back.

The View menu shows and hides each section; drag tabs around to arrange them however you like, and the layout is remembered. View > Layout switches between ready-made arrangements (Listening, Watching, Script writing, Sprite editing) and ones you've saved with "Save current layout as...". Each keeps whatever changes you make while it's showing; a changed built-in one has a Reset button to go back to how it comes.

### Keys

| Key | |
|---|---|
| Space | Play / pause |
| Left / Right | Seek 5 seconds |
| N / P | Next / previous playlist track |
| R | Restart the song |
| V | Show or hide the visualizer |
| F | Fullscreen (the whole app) |
| F11 | Fullscreen visualizer |
| F5 | Restart the visualizer script |
| F9 | Start / stop recording the visualizer |
| F10 | Pause / resume the recording |
| Esc | Back to windowed |
| Shift+Left / Right | Seek 30 seconds |
| Ctrl+Shift+P | Command palette: everything the app can do, by name |
| Ctrl+G, then a letter | Go to a tab (S Songs, G Groupings, U Up next, F Soundfonts, V Visualizer, E Script Editor...): the popup after Ctrl+G lists them |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous tab |

Preferences > Keyboard changes any of them (and they sync with your other devices).

In the Songs, Groupings, Up next and Soundfonts lists: click to play or open (the selection stays as it is), Up/Down, Page Up/Down and Home/End to move (Shift to select as you go) and Enter to play, Ctrl+click and Shift+click to select, Ctrl+A selects all, Esc (or the ✕ under the list) selects none, and drag the selection (from Songs onto a playlist, or within a playlist to reorder it). Delete removes the selection from a playlist or the soundfont list, or (asking first) from the library. Right-click a row for more.

## Visualizer scripts

Scripts are Lua files in `%APPDATA%\synththing\config\visualizers`. The bundled ones are copied there on first run, so they're yours to edit. Open View > Script Editor to edit the running script with live reload, syntax highlighting and help on every function; **New** starts a script from a template.

The full scripting reference is [docs/scripting-reference.md](docs/scripting-reference.md), also built into the app (View > Scripting Reference). To check a script for errors without opening the app:

```
synththing run-script my_script.lua --song song.mid
```

## Command line

`synththing --help` lists everything; [docs/cli.md](docs/cli.md) has the details.

## Where things are kept

Everything lives in `%APPDATA%\synththing\config` (`~/.config/synththing` on Linux; Preferences > App > Storage shows how much each part takes):

- `soundfonts.json`: settings (your own themes included), the soundfont list, the layout
- `music\`: the song library: each song named by its contents, and `library.json` with their details, tags and stars
- `playlists\`: one file per playlist
- `visualizers\`: scripts, plus each script's settings (`.settings.json`) and saved data (`.store.json`)
- `soundfonts\`: the starter pack's soundfonts
- `art\`, `musicbrainz\` and `lookup.json`: album covers and details found online, and what the lookup has matched
- `identity.key`, `sync.key` and `device.json`: who you are online and on which device (back them up with Preferences > Online Library > Export)
- `synththing.log`: the app's log (also under Help > Log...); the previous run's is `synththing.prev.log`

## Building from source

With [Rust](https://rustup.rs) installed:

```
cargo build --release
```

The program ends up at `target\release\synththing.exe`. Run the tests with `cargo test --lib`. How releases are made is in [docs/RELEASING.md](docs/RELEASING.md).

## Credits

synththing is by [Dissssy](https://github.com/Dissssy) and contributors. It plays MIDI with [rustysynth](https://github.com/sinshu/rustysynth) by Nobuaki Tanaka, runs scripts on [Lua](https://www.lua.org) through mlua, and draws script text in [Monogram](https://datagoblin.itch.io/monogram) by Vinícius Menézio. Help > Credits & licenses in the app lists every library it's built from, with their licenses.

## License

synththing's own code is released under the [WTFPL](LICENSE): do whatever you want with it. The libraries, font and code it's built from keep their own licenses (rustysynth and Lua are MIT, Monogram is CC0, and so on), as do the starter pack's soundfonts (GeneralUser GS's own license, TimGM6mb under the GPL 2.0; see `assets/starter/README.md`); Help > Credits & licenses lists them all.
