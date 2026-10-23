# Local API

synththing can let programs on the same computer (or, if you choose, your local network) talk to it: see what's playing, search the library, follow what happens and run commands. Use it for your own integrations: home automation, notifications, stream overlays, game hooks. It's desktop only and off until you turn it on.

## Turning it on

Preferences > Integrations:

1. Tick **Let programs on this computer use the API**. It listens on `127.0.0.1`, port 7340 unless you pick another. Nothing outside this computer can reach it unless you also tick **Let devices on the local network in too**: then other computers and phones on your network can use it at this computer's address, with a token as always. It's plain HTTP, so tokens and everything else cross the network unencrypted: only turn it on in a network you trust. While it's off, requests passed on by a proxy (with `Forwarded`, `X-Forwarded-For`, `X-Forwarded-Host` or `X-Real-IP`) or addressed to any name but `localhost`, `127.0.0.1` or `[::1]` are turned away with 403, so a reverse proxy or a web page can't reach it on another's behalf.
2. Type a name for your program and click **Add**. Copy the token it shows: only its hash is kept, so it isn't shown again (remove the program and add it again for a new one).
3. Tick what that program may do: **Read** (the library, what's playing, events), **Control** (run commands), **Edit library** (star, tag, make, rename, delete and reorder playlists, and add songs to them or take them out), **Add songs** (send songs into your library) and **Download songs** (copy songs' files out). Edit library, Add songs and Download songs are never on until you tick them.

Send the token with every request:

```
Authorization: Bearer st_...
```

Where a header can't be set (a browser's `EventSource`), add `?token=st_...` to the URL instead.

## Requests

Everything is JSON. Errors come back as `{"error": "..."}` with a status: 400 (something's missing from the request), 401 (no or unknown token), 403 (the token isn't allowed that), 404, 409 (the command can't run right now), 413 (a song over 250 MB), 415 (a file synththing can't play), 500 (the song couldn't be stored) or 503 (synththing didn't answer within 5 seconds, is closing, or already has 32 event streams open).

| Request | Needs | What it gives |
| --- | --- | --- |
| `GET /v1` | any token | `{"app", "version", "read", "control", "add_songs", "edit_library", "download_songs"}`: who you are to it |
| `GET /v1/now-playing` | read | What's playing (below) |
| `GET /v1/songs?q=&offset=&limit=` | read | `{"total", "songs": [...]}`: the songs `q` finds, sorted by file name: `limit` of them (at most and by default 500) after skipping `offset` |
| `GET /v1/songs/{key}` | read | One song, by the `key` the other requests give |
| `POST /v1/songs?name=Song.mp3` | add songs | Adds the file sent as the body (up to 250 MB) to the library: 201 with the song if it's new, 200 if it was there already |
| `GET /v1/playlists` | read | `[{"id", "name", "smart", "songs": [keys]}]` |
| `POST` / `DELETE /v1/songs/{key}/star` | edit library | Stars the song, or unstars it: 204 |
| `POST` / `DELETE /v1/songs/{key}/tags?tag={name}` | edit library | Gives the song that tag, or takes it off: 204, or 404 if there's no such tag (tags are made in synththing) |
| `GET /v1/tags` | read | `[{"name", "color"}]` |
| `POST` / `DELETE /v1/playlists/{id}/songs?song={key}` | edit library | Adds the song to the end of the playlist, or takes it out (every time it's in it): 204, 404 if it isn't in it, 409 for a smart playlist |
| `POST /v1/playlists?name={name}` | edit library | Makes an empty playlist: 201 with `{"id", "name"}` |
| `POST /v1/playlists/{id}/rename?to={name}` | edit library | Renames it: 204 |
| `DELETE /v1/playlists/{id}` | edit library | Deletes it (on all your devices, when your library syncs): 204 |
| `POST /v1/playlists/{id}/songs/move?from={n}&to={m}` | edit library | Moves the song at place `from` so it ends up at place `to` (both from 0): 204, 404 past its end, or 409 for a smart playlist |
| `GET /v1/songs/{key}/file` | download songs | The song's file, with its type, length and its name as imported (`Content-Disposition`), or 404 if it isn't on this computer |
| `GET /v1/songs/{key}/art` | read | The song's picture (its own, else its album's cover) as an image, or 404 |
| `GET /v1/groupings` | read | `[{"id", "name", "split"}]`: the rulesets (By Artist, By Album and yours) |
| `GET /v1/groupings/{id}` | read | `{"id", "name", "split", "groups": [{"id", "title", "subtitle", "songs": [keys]}]}`: the playlists it makes of the songs on this computer, as Groupings shows them |
| `GET /v1/groups?split=album&q=` | read | `[{"id", "title", "subtitle", "songs"}]`: the songs `q` finds, split by `artist`, `album_artist`, `album`, `composer`, `genre`, `year` or `tag` |
| `GET /v1/commands` | read | `[{"id", "name"}]`: every command there is |
| `POST /v1/commands/{id}` | control | Runs it: 204 when it ran |
| `POST /v1/play?song={key}` | control | Plays that song on its own, leaving any playlist: 204 |
| `POST /v1/play?playlist={id}` | control | Plays that playlist from the top (anywhere, with Shuffle on): 204, or 409 if it's empty |
| `POST /v1/play?playlist={id}&song={key}` | control | Plays that playlist from that song: 204, or 404 if the song isn't in it |
| `POST /v1/seek?to={seconds}` | control | Jumps to that point of the song: 204, or 409 if nothing's playing |
| `POST /v1/volume?to={0..1}` | control | Sets the volume: 204 |
| `POST /v1/modes?shuffle=&loop=&autoplay=` | control | Sets any of them: `shuffle` `true` or `false`, `loop` `off`, `one` (this song) or `all` (the playlist), `autoplay` `off`, `group` or `all`: 204 |
| `GET /v1/soundfonts` | read | `[{"id", "name", "size", "active"}]`: the Soundfonts list. `size` is in bytes (`null` if its file is missing), and `active` marks the one MIDI songs play with |
| `POST /v1/soundfonts/{id}/use` | control | MIDI songs play with that soundfont from now on (the one playing switches live): 204, 404 if there's no such soundfont, or 409 if its file is missing |
| `GET /v1/scripts` | read | `[{"id", "name", "active"}]`: the visualizer scripts, as the script picker lists them. `active` marks the one drawing |
| `POST /v1/scripts/{id}/use` | control | The visualizer draws with that script from now on: 204, or 404 if there's no such script |
| `GET /v1/queue` | read | The play queue (Up next): its songs, in the order they'll play |
| `POST /v1/queue?song={key}` | control | Adds the song to the end of the queue (`&next=true`: to its front, to play next): 204 |
| `POST /v1/queue?playlist={id}` | control | Adds the playlist's songs (shuffled while Shuffle is on), at the end or with `&next=true` at the front: 204, or 409 if it has no songs here |
| `POST /v1/queue/move?from={n}&to={m}` | control | Moves the song at place `from` so it ends up at place `to` (both from 0): 204, or 404 past the queue's end |
| `DELETE /v1/queue?at={n}` | control | Takes out the song at that place in the queue (from 0): 204, or 404 past its end |
| `DELETE /v1/queue` | control | Clears the queue: 204 |
| `GET /v1/visualizer/stream?width=&height=&fps=` | read | The visualizer, live, as MJPEG (below) |
| `GET /v1/visualizer/frame.jpg?width=&height=` | read | The visualizer's next picture, as a JPEG: 503 if it doesn't draw one within 3 seconds |
| `GET /v1/visualizer?width=&height=&fps=&token=` | read | A page showing the stream, for OBS's Browser Source or a browser |
| `GET /v1/events` | read | Server-Sent Events (below) |

An album's art is its first song's: `GET /v1/songs/{first key}/art`. Covers that synththing hasn't downloaded yet are downloaded only when online lookups are on.

`q` works like the Songs search box: filters such as `artist:bonno`, `year:1990..1999`, `is:starred` or `tag:chill`, and any other words must each be in the title, artist, album or file name.

A song:

```json
{"key": "3f2a...", "file_name": "Overworld.mid", "title": "Overworld", "artist": "Koji Kondo",
 "album": null, "album_artist": null, "genre": null, "year": 1985, "track": 1, "length": 84.2,
 "starred": true, "tags": ["chill"], "imported": 1761000000, "added_by": null}
```

What's playing (`soundfont` is the one a MIDI song plays with, `null` for any other song):

```json
{"song": {...}, "playlist": {"id": "...", "name": "Mornings"}, "paused": false,
 "position": 12.5, "length": 84.2, "speed": 1.0, "volume": 0.8,
 "shuffle": false, "loop": "off", "autoplay": "all",
 "soundfont": {"id": "9c1e...", "name": "GeneralUser GS"}}
```

Commands are the ones Preferences > Keyboard lists, by id: `play`, `pause`, `toggle_pause`, `next_track`, `previous_track`, `seek_back`, `seek_forward`, `restart_song`, `toggle_visualizer`, `toggle_fullscreen`, `go_to_songs` and so on. `GET /v1/commands` has them all.

## Songs added by programs

Every song a program adds that wasn't in your library yet is marked with that program's name (`added_by`), on all your devices. Search `addedby:name` in Songs to see them, or `-addedby:name` to leave them out. If a program fills your library with songs you don't want, Preferences > Integrations has **Remove the N songs it added...** beside it, which opens the usual Remove dialog (with "Remove from all my devices" when your library syncs). Removing the program itself only stops its token: its songs stay.

## Events

`GET /v1/events` keeps the connection open and sends:

- `now_playing`: the song, playlist, pause, speed, volume, Shuffle, Loop, Autoplay or soundfont changed. Its data is what's playing, as above.
- `seeked`: the song jumped (a seek, or a loop back to the start): `{"position": 30.0}`.
- `library_changed`: songs were added, removed or edited: `{}`. Ask `/v1/songs` again for the new state.
- `playlists_changed`: a playlist was made, renamed, deleted or had songs added, removed or moved (here or by sync): `{}`. Ask `/v1/playlists` again.
- `queue_changed`: songs were queued, played from the queue, moved or taken out: `{}`. Ask `/v1/queue` again.
- `soundfonts_changed`: a soundfont was added, removed or put in use: `{}`. Ask `/v1/soundfonts` again.
- `scripts_changed`: a script was added, removed or put in use: `{}`. Ask `/v1/scripts` again.

Add `?events=now_playing,seeked` (any of them, by name) for only those. A comment line (`: keep-alive`) comes every 25 seconds when nothing else does.

## The visualizer, live

`/v1/visualizer/stream` sends the visualizer's pictures as they're drawn, as MJPEG (`multipart/x-mixed-replace`), at most `fps` a second (1 to 60, 30 if not given). With `width` and `height` (64 to 4096 each, made even), the script draws at exactly that size and synththing's own window just says it's streaming instead of showing it. Without them, you get whatever size it's showing at. A recording's size wins while one is going. Scripts can tell they're being watched: `playback().streaming` is true.

In OBS, add a **Media Source**, untick Local File and give it `http://127.0.0.1:7340/v1/visualizer/stream?width=1920&height=1080&token=st_...`, or a **Browser Source** with `http://127.0.0.1:7340/v1/visualizer?width=1920&height=1080&token=st_...` and the same size. The visualizer has to be showing somewhere in synththing (its tab open, or the dedicated window) for pictures to come.

## System media controls

Separately from the API, synththing shows what's playing in the system's media controls (on by default, Preferences > Integrations): MPRIS on Linux, the media overlay and media keys on Windows, Now Playing on macOS. Anything that already understands those (desktop widgets, KDE Connect, Home Assistant's MPRIS integrations) can see and control it without a token. Play, pause, next, previous, seek and volume work from there.

## Trying it

```sh
TOKEN=st_...
curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:7340/v1/now-playing
curl -X POST -H "Authorization: Bearer $TOKEN" http://127.0.0.1:7340/v1/commands/next_track
curl -X POST -H "Authorization: Bearer $TOKEN" "http://127.0.0.1:7340/v1/play?song=3f2a..."
curl -N -H "Authorization: Bearer $TOKEN" http://127.0.0.1:7340/v1/events
curl -H "Authorization: Bearer $TOKEN" --data-binary @Song.mp3 "http://127.0.0.1:7340/v1/songs?name=Song.mp3"
```

On Windows PowerShell:

```powershell
$h = @{ Authorization = "Bearer st_..." }
Invoke-RestMethod http://127.0.0.1:7340/v1/now-playing -Headers $h
Invoke-RestMethod -Method Post http://127.0.0.1:7340/v1/commands/toggle_pause -Headers $h
```
