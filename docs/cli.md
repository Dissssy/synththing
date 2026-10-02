# synththing command line

Run with no arguments, `synththing.exe` opens the app as usual. With arguments, it works from a terminal (cmd, PowerShell, Windows Terminal): help, errors and command output print there.

`synththing --help` prints the same information as this page, generated from the code (`src/cli.rs`). Keep the two in step.

## Opening the app

```
synththing [--visualizer]
```

- `--visualizer` (or `--viz`): open with the Visualizer tab showing.
- `--version` / `-V`: print the version.
- `--help` / `-h`: print help. `synththing help run-script` (or `synththing run-script --help`) for a command's options.

## run-script: test a visualizer script without a window

```
synththing run-script <SCRIPT> [--song FILE] [--soundfont FILE] [--frames N] [--fps N]
                               [--width PX] [--height PX] [--start SECONDS] [--save-data] [--quiet]
                               [--screenshot FILE]
```

Runs a script the way the app does, with a song playing into it, but with no window, then prints what happened. It's for catching errors in a script without opening the app and playing through the song by hand, for example in an automated check.

It uses the same engine and script code as the app. Each frame it plays `1/fps` seconds of the song, then runs the script's `render()` with that audio, the current notes and the playback state, then carries out anything the script asked for (`set_paused`, `seek`, `set_channel_enabled`). `DT` is exactly `1/fps` every frame (0.0 on the first), and `TIME` counts frames, so a run is the same every time no matter how fast the machine is.

| Option | Default | |
|---|---|---|
| `SCRIPT` | | The `.lua` file to run. Its settings file (`<script>.lua.settings.json`) is read, as in the app. |
| `--song`, `-s` | none | A song to play into it: MIDI (`.mid`, `.midi`) or audio (`.wav`, `.mp3`, `.ogg`, `.flac`, ...). Without one the script still runs, with no audio, no notes and the position at 0. |
| `--soundfont` | first in the app's list | The soundfont for a MIDI song. Defaults to the first soundfont in the app's Soundfonts list that still exists; a MIDI song with no soundfont at all is an error. |
| `--frames`, `-f` | 600 | How many frames to run. |
| `--fps` | 60 | Frames per second: how much song time each frame covers, and `DT`. |
| `--width`, `--height` | 640, 360 | The visualizer's size in pixels, what `render()` gets as `width`/`height`. |
| `--start` | 0 | Start this many seconds into the song. |
| `--save-data` | off | Let the script write its saved data (`store_set`) to `<script>.lua.store.json`. Off by default so test runs don't overwrite real saved data; the script still reads what's saved either way. |
| `--quiet`, `-q` | off | Print only errors and the summary, not the script's log. |
| `--screenshot` | none | Save the last frame as a PNG image at this path, to see what the script drew. |

Output: each distinct error once, with the frame and song position it happened at; then the script's log (each line prefixed `log: `, repeats shown as `(xN)`), unless `--quiet`; then how long `render()` took (average and worst, real time on this machine, and whether the app would drop the script to 30 fps for averaging over 16.7 ms); then a summary line:

```
frame 30 (0.50s): error: runtime error: [string "visualizer"]:2: attempt to call a nil value (global 'nope')
log: started
render() took 4.03 ms on average, 6.45 ms at worst (a 60 fps frame allows 16.7 ms)
ran 600 frames at 60 fps, song at 10.00s of 109.92s, 1 error
```

Exit codes:

- `0`: ran cleanly.
- `1`: the script had a compile error or runtime errors.
- `2`: something couldn't be loaded (the script file, the song, the soundfont), or an option was invalid.

Not simulated: mouse and keyboard input (scripts see no pointer and no keys, and `display_mode()` is `"window"`), and the app's own UI.

## Running from a terminal on Windows

Release builds are Windows GUI programs, so they don't own a console. When run with arguments, synththing attaches to the terminal it was started from and prints there. Two things follow from that:

- The terminal doesn't wait for it to finish, so the prompt can come back before the output does, and the exit code isn't reported to the shell. To wait for it and get the exit code:
  - cmd: `start /wait synththing.exe run-script my.lua --song song.mid` then `echo %ERRORLEVEL%`
  - PowerShell: `$p = Start-Process synththing.exe -ArgumentList 'run-script','my.lua' -NoNewWindow -Wait -PassThru; $p.ExitCode`
- Output redirected to a file or pipe (`> out.txt`, `| more`) works directly, since then there's no console to attach to.

Development builds (`cargo run -- run-script ...`) are console programs and behave normally.
