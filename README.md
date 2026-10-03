# synththing

A MIDI and audio player for Windows with a visualizer you can script in Lua.

- Plays MIDI files through any SoundFont (`.sf2`), and audio files (MP3, WAV, OGG Vorbis, FLAC, M4A/AAC).
- Playlists: build them by dragging songs or whole folders in, reorder, shuffle and loop, give MIDI tracks their own soundfont, import and export `.m3u`.
- A visualizer driven by Lua scripts: waveform, spectrum, piano roll, spectrogram, and games that play along: a Guitar Hero style highway charted from any song, a snake game, a platformer, and a piano you can play on with your keyboard. Write your own with a live-reloading editor: scripts can take mouse and keyboard input (with rebindable controls), take typed text, and play notes on the song's instruments, in a window or fullscreen.
- A sprite editor for the pixel art scripts draw: paint, fill, palettes and sprite sheets, written straight back into the script.
- Record the visualizer with its sound to a video (MP4, 720p to 4K, 30 or 60 fps): a whole song, a whole playlist, or whatever you play. Uses ffmpeg, which synththing offers to download the first time.
- Rearrangeable layout: every section is a tab you can drag, split or pop out.
- Updates itself: new releases are offered in the app.

## Getting it

1. Download `synththing-windows-x86_64.exe` from the [latest release](https://github.com/Dissssy/synththing/releases/latest).
2. Run it. It's a single file, no installer; put it wherever you like.
3. The first time, Windows may show "Windows protected your PC", because the program isn't code-signed. Click **More info**, then **Run anyway**.

After that, synththing checks for new versions when it starts and asks before installing one (you can turn that off in File > Preferences, or check any time from Help > Check for updates).

It needs 64-bit Windows 10 or 11.

## First steps

1. **Pick your music.** The Songs tab is a file browser: **Browse...** jumps to any folder or drive, **Set as default** makes it open there next time, and the search box finds songs in subfolders too. Click a song to play it.
2. **Add a soundfont for MIDI.** MIDI files are just notes; a SoundFont supplies the instruments. In the Soundfonts tab, **Add soundfont...** and pick any General MIDI `.sf2` (plenty are free online). Compressed `.sf3` soundfonts aren't supported; Polyphone can convert them to `.sf2`.
3. **Make a playlist.** Open View > Playlists, then **New playlist**, and drag songs (or folders) in from Songs. Drag a soundfont onto a MIDI entry to give it its own.
4. **Turn on the visualizer.** View > Visualizer, then pick a script above it. **Fullscreen visualizer** (or F11) fills the screen; Esc comes back.

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

In a playlist: click to select (Ctrl and Shift for more), double-click to play, Ctrl+A selects all, Delete removes. Right-click a row for more.

## Visualizer scripts

Scripts are Lua files in `%APPDATA%\synththing\config\visualizers`. The bundled ones are copied there on first run, so they're yours to edit. Open View > Script Editor to edit the running script with live reload, syntax highlighting and help on every function; **New** starts a script from a template.

The full scripting reference is [docs/scripting-reference.md](docs/scripting-reference.md), also built into the app (View > Scripting Reference). To check a script for errors without opening the app:

```
synththing run-script my_script.lua --song song.mid
```

## Command line

`synththing --help` lists everything; [docs/cli.md](docs/cli.md) has the details.

## Where things are kept

Everything lives in `%APPDATA%\synththing\config`:

- `soundfonts.json`: settings, the soundfont list, the layout
- `visualizers\`: scripts, plus each script's settings (`.settings.json`) and saved data (`.store.json`)
- `playlists\`: one file per playlist
- `synththing.log`: the app's log (also under Help > Log...); the previous run's is `synththing.prev.log`

## Building from source

With [Rust](https://rustup.rs) installed:

```
cargo build --release
```

The program ends up at `target\release\synththing.exe`. Run the tests with `cargo test --bin synththing`. How releases are made is in [docs/RELEASING.md](docs/RELEASING.md).

## Credits

synththing is by [Dissssy](https://github.com/Dissssy) and contributors. It plays MIDI with [rustysynth](https://github.com/sinshu/rustysynth) by Nobuaki Tanaka, runs scripts on [Lua](https://www.lua.org) through mlua, and draws script text in [Monogram](https://datagoblin.itch.io/monogram) by Vinícius Menézio. Help > Credits & licenses in the app lists every library it's built from, with their licenses.

## License

synththing's own code is released under the [WTFPL](LICENSE): do whatever you want with it. The libraries, font and code it's built from keep their own licenses (rustysynth and Lua are MIT, Monogram is CC0, and so on); Help > Credits & licenses lists them all.
