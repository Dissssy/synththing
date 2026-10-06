# synththing command line

Run with no arguments, `synththing.exe` opens the app as usual. With arguments, it works from a terminal (cmd, PowerShell, Windows Terminal): help, errors and command output print there.

`synththing --help` prints the same information as this page, generated from the code (`src/cli.rs`). Keep the two in step.

## Opening the app

```
synththing [--visualizer] [--console]
```

- `--visualizer` (or `--viz`): open with the Visualizer tab showing.
- `--console`: also print the app's log (what Help > Log... shows: script errors, warnings, panics) to the terminal as it happens. Handy for watching a release build, which otherwise has no console (see below).
- `--version` / `-V`: print the version.
- `--help` / `-h`: print help. `synththing help run-script` (or `synththing run-script --help`) for a command's options.

## run-script: test a visualizer script without a window

```
synththing run-script <SCRIPT> [--song FILE] [--soundfont FILE] [--frames N] [--fps N]
                               [--width PX] [--height PX] [--start SECONDS] [--save-data] [--quiet]
                               [--screenshot FILE] [--video FILE] [--video-size SIZE]
                               [--video-quality QUALITY] [--auto]
```

Runs a script the way the app does, with a song playing into it, but with no window, then prints what happened. It's for catching errors in a script without opening the app and playing through the song by hand, for example in an automated check.

It uses the same engine and script code as the app. Each frame it plays `1/fps` seconds of the song, then runs the script's `render()` with that audio, the current notes and the playback state, then carries out anything the script asked for (`set_paused`, `seek`, `set_speed`, `set_channel_enabled`, notes to play). There's no playlist in a run, so `playlist()` is nil and the playlist controls do nothing. A script with `script_options({ start_paused = true })` gets the song paused at the start, as in the app, until it unpauses it itself. Notes the script plays (`play_note` and the like) are mixed into the audio it gets, as in the app. `DT` is exactly `1/fps` every frame (0.0 on the first), and `TIME` counts frames, so a run is the same every time no matter how fast the machine is.

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
| `--video` | none | Record the run to a video at this path (`.mp4`: H.264 video, AAC sound), the same way the app records. The video is `--video-size` instead of `--width`/`--height`, at `--fps` (a whole number). It's written as fast as the script and the encoder allow, faster or slower than real time, and every frame is in it: the same run gives the same video. Needs ffmpeg, on the PATH or downloaded by the app (Preferences > Recording). |
| `--video-size` | 1080p | The video's size: `720p`, `1080p`, `1440p` or `4k`; `vertical` (1080 x 1920, for phone-shaped videos), `720p-vertical` or `square` (1080 x 1080); or any `WIDTHxHEIGHT` from 64 to 4096 a side (`800x600`; made even if odd). |
| `--auto` | off | With `--video`: the script plays itself, if it can (`script_options({ record_auto = true })`; `recording().mode` is `"auto"`). |
| `--video-quality` | standard | How it's encoded: `standard` (H.264 4:2:0, plays everywhere; single-pixel colored details soften slightly), `sharp` (H.264 4:4:4, exact pixel edges; plays in desktop players, not reliably in browsers or Discord) or `lossless` (every pixel exactly; big files, for editing). |

Output: each distinct error once, with the frame and song position it happened at; then the script's log (each line prefixed `log: `, repeats shown as `(xN)`), unless `--quiet`; then how long `render()` took (average and worst, real time on this machine, and whether the app would drop the script to 30 fps for averaging over 16.7 ms); then a summary line:

```
frame 30 (0.50s): error: runtime error: [string "visualizer"]:2: attempt to call a nil value (global 'nope')
log: started
render() took 4.03 ms on average, 6.45 ms at worst (a 60 fps frame allows 16.7 ms)
ran 600 frames at 60 fps, song at 10.00s of 109.92s, 1 error
```

With `--video`, a line before the log says where it went: `saved a 10.0 s video to clip.mp4`. A script with `script_options({ record_prepare = true })` is recorded the way the app records it (see Recording in the Scripting Reference): the frames before its `recording_ready()` aren't in the video or counted in `--frames` (it gets 2 minutes of them at most), and its `recording_done()` ends the run early, with a line saying so.

Exit codes:

- `0`: ran cleanly.
- `1`: the script had a compile error or runtime errors.
- `2`: something couldn't be loaded (the script file, the song, the soundfont), or an option was invalid.

Not simulated: mouse, keyboard and typed input (scripts see no pointer, no keys and no typing, and `display_mode()` is `"window"`), and the app's own UI.

## preview: make a script's preview

```
synththing preview SCRIPT [--out DIR] [--soundfont FILE] [--check]
```

Makes the preview the Script Library and the script picker show: the script plays about 12 seconds from the middle of each starter song at 320 x 180, as a recording in Auto (a game that can play itself is seen playing), and each frame is scored for how much there is to see (contrast, colours, motion). The best-scoring 48 frames in a row become the animation, `preview-sheet.png` (a sprite sheet, 8 frames to a row, played at 12 a second), and the best single frame the still, `preview.png`, both in `--out` (the current folder by default). The first frame with anything on it is written as the still straight away, so a run stopped early still leaves one; a script that never draws anything (all one colour) gets no preview.

With `--check`, it only checks that the script runs: it compiles and plays a few seconds of a song without an error. Library servers check every upload this way.

`--soundfont` defaults to the first one in the app's Soundfonts list. Exit codes: `0` made it (or it checked out), `1` the script has an error, `2` something couldn't be loaded.

## serve: run a script library server

```
synththing serve [--data DIR] [--bind ADDR]
```

Runs a script library server (the design is in docs/SERVER.md): the app's Script Library tab browses, installs and publishes scripts on it once its address is added under Preferences > Library. It serves until stopped (Ctrl+C), printing a line per upload.

| Option | Default | |
|---|---|---|
| `--data` | `server` in the app's config folder | The folder it keeps everything in: `server.json` (its settings), `server.key` (its signing key), `email.pepper` (what email addresses are hashed with), `library.db` (the scripts), `previews/` and `soundfonts/` (what previews are made with). Made on first run, with default settings to edit. |
| `--bind` | `server.json`'s `bind` (127.0.0.1:7381) | The address and port to listen on. |

`server.json`:

- `name`: shown in the app.
- `bind`: where it listens. Behind a reverse proxy, leave it on 127.0.0.1.
- `behind_proxy`: true when a reverse proxy (Caddy, nginx) passes requests on, so the client's address is taken from `X-Forwarded-For` (for upload limits).
- `mode`: `"open"` (anyone can upload, signed or anonymously) or `"signed"` (only uploads signed with the uploader's identity).
- `license`, `rules`, `contact`: shown to people publishing (CC BY 4.0 by default).
- `uploads_per_day`: per address (10). Unchanged re-uploads don't count.
- `unlimited_keys`: keys (hex) whose uploads aren't limited: by default the official publisher's, which publishes the bundled scripts.
- `threads`: request workers (4).
- `check_uploads`: refuse uploads that don't run on this server's version (`preview --check`): they don't compile, or error in their first seconds (true). One whose check takes over 12 seconds is taken: slow isn't broken.
- `previews`: make a preview of each upload (true), one at a time, each by `synththing preview` in a process of its own; at startup, any script's newest version without one is queued. They play the starter songs with TimGM6mb, downloaded into `soundfonts/` the first time, unless `preview_soundfont` names another.
- `preview_seconds`: how long making one may take before it's stopped (120); the first frame it drew stays as the still.
- `authorities`: the identity authorities whose rotation records it takes (their keys, hex): the official server's by default.
- `authority`: this server is an identity authority itself (false): it issues rotations (an identity moving to a new key), and with `mail`, attaches recovery emails and recovers identities by them (docs/SERVER.md).
- `public_url`: the address people reach it at, for links in its emails (e.g. `https://scripts.example.org`).
- `mail`: sending email, for an authority: `{ "from": "synththing@example.org", "from_name": "synththing", "api_key_file": "sendgrid.key" }`. Mail goes through SendGrid, with the API key read from `api_key_file` (in the data folder unless it's a full path). Without it, an authority does rotations only.
- `unverified_uploads_per_day`: on an authority with mail, uploads a day for a key without an email (and anonymous ones), instead of `uploads_per_day` (3).

For the internet, put it behind a reverse proxy for HTTPS. With Caddy:

```
scripts.example.org {
    reverse_proxy localhost:7381
}
```

## admin: moderate a library server

```
synththing admin [--data DIR] [--server URL] <command>
```

Moderates a running library server from the machine it runs on: each command goes to the server (at `server.json`'s `bind`, or `--server`), signed with the server's own key from `--data` (the same folder as `serve`'s). Over SSH, run it as a user that can read `server.key`; in Docker, `docker exec <container> synththing admin --data /data <command>`. Admins can do the same from the app (Moderation, in the Library tab).

| Command | |
|---|---|
| `reports [--all]` | Open reports (and dealt-with ones), newest first: the script, version, reason, reporter, details, and the lines and sprites it points at. |
| `info SCRIPT` | A script, hidden or not: its uploader's key, each version with the address it came from (kept 30 days), its reports. |
| `hide SCRIPT [--reason TEXT]`, `unhide SCRIPT` | Take it out of listings and downloads, or put it back. |
| `delete SCRIPT` | Delete it, every version, for good (its reports stay). |
| `ban TARGET --reason TEXT [--hours N] [--hide-scripts] [--ban-addresses]` | Ban a key (an ID like `#k3f9q2xa` the server has seen, or the whole key in hex) or an address, for N hours or for good, optionally hiding everything the key uploaded. With `--ban-addresses`, the addresses the key was seen on in the last 30 days are banned for 30 days too, and so is any new one it comes back from while banned (so a new key on the same connection gets nowhere). Banned keys and addresses can't upload, give encores or report; they can still browse and download. |
| `unban TARGET`, `bans` | Lift a ban; list the bans in force. |
| `add-admin KEY`, `remove-admin KEY`, `admins` | Who can moderate from the app (an ID the server has seen, or a whole key). |
| `resolve REPORT [--note TEXT]` | Mark a report dealt with. |

## publish-bundled: publish the bundled scripts

```
SYNTHTHING_PUBLISH_KEY=<secret key, hex> synththing publish-bundled [--server URL] [--dir DIR]
```

Publishes every script bundled with this build (visualizers, games and examples; not the templates), or with `--dir` the ones in that folder (laid out like `assets/visualizers`), to a library server, `https://synththing.p51.nl` unless `--server` says otherwise, signed with the secret key in `SYNTHTHING_PUBLISH_KEY`. Each goes by its slug (its file name): a changed script becomes its next version, an unchanged one stays as it is. With `--dir`, a script using something this build doesn't know (a function newer than it) is skipped, since its minimum app version couldn't be worked out. Prints a line per script; exit code 1 if any failed. CI runs it (`.github/workflows/scripts.yml`, docs/RELEASING.md).

## Environment variables

- `SYNTHTHING_OFFICIAL_URL`: use another server as the official one (for trying things against a server of your own, e.g. `http://127.0.0.1:7381`). What's recorded about installed scripts still names the real one.

## Running from a terminal on Windows

Release builds are Windows GUI programs, so they don't own a console. When run with arguments, synththing attaches to the terminal it was started from and prints there. Two things follow from that:

- The terminal doesn't wait for it to finish, so the prompt can come back before the output does, and the exit code isn't reported to the shell. To wait for it and get the exit code:
  - cmd: `start /wait synththing.exe run-script my.lua --song song.mid` then `echo %ERRORLEVEL%`
  - PowerShell: `$p = Start-Process synththing.exe -ArgumentList 'run-script','my.lua' -NoNewWindow -Wait -PassThru; $p.ExitCode`
- Output redirected to a file or pipe (`> out.txt`, `| more`) works directly, since then there's no console to attach to.

Development builds (`cargo run -- run-script ...`) are console programs and behave normally.
