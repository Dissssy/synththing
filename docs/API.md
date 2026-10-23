# Local API

synththing can let programs on the same computer (or, if you choose, your local network) talk to it: see what's playing, search the library, follow what happens and run commands. Use it for your own integrations: home automation, notifications, stream overlays, game hooks. It's desktop only and off until you turn it on.

## Turning it on

Preferences > Integrations:

1. Tick **Let programs on this computer use the API**. It listens on `127.0.0.1`, port 7340 unless you pick another. Nothing outside this computer can reach it unless you also tick **Let devices on the local network in too**: then other computers and phones on your network can use it at this computer's address, with a token as always. It's plain HTTP, so tokens and everything else cross the network unencrypted: only turn it on in a network you trust.
2. Type a name for your program and click **Add**. Copy the token it shows: only its hash is kept, so it isn't shown again (remove the program and add it again for a new one).
3. Tick what that program may do: **Read** (the library, what's playing, events), **Control** (run commands) and **Add songs** (send songs into your library). Add songs is never on until you tick it.

Send the token with every request:

```
Authorization: Bearer st_...
```

Where a header can't be set (a browser's `EventSource`), add `?token=st_...` to the URL instead.

## Requests

Everything is JSON. Errors come back as `{"error": "..."}` with a status: 400 (something's missing from the request), 401 (no or unknown token), 403 (the token isn't allowed that), 404, 409 (the command can't run right now) or 503 (synththing didn't answer in time).

| Request | Needs | What it gives |
| --- | --- | --- |
| `GET /v1` | any token | `{"app", "version", "read", "control", "add_songs"}`: who you are to it |
| `GET /v1/now-playing` | read | What's playing (below) |
| `GET /v1/songs?q=&offset=&limit=` | read | `{"total", "songs": [...]}`: the songs `q` finds, by file name, at most 500 at a time |
| `GET /v1/songs/{key}` | read | One song |
| `POST /v1/songs?name=Song.mp3` | add songs | Adds the file sent as the body (up to 250 MB) to the library: 201 with the song if it's new, 200 if it was there already |
| `GET /v1/playlists` | read | `[{"id", "name", "smart", "songs": [keys]}]` |
| `GET /v1/commands` | read | `[{"id", "name"}]`: every command there is |
| `POST /v1/commands/{id}` | control | Runs it: 204 when it ran |
| `POST /v1/play?song={key}` | control | Plays that song on its own, leaving any playlist: 204 |
| `POST /v1/play?playlist={id}` | control | Plays that playlist from the top (anywhere, with Shuffle on): 204, or 409 if it's empty |
| `POST /v1/play?playlist={id}&song={key}` | control | Plays that playlist from that song: 204, or 404 if the song isn't in it |
| `GET /v1/events` | read | Server-Sent Events (below) |

`q` works like the Songs search box: filters such as `artist:bonno`, `year:1990..1999`, `is:starred` or `tag:chill`, and any other words must each be in the title, artist, album or file name.

A song:

```json
{"key": "3f2a...", "file_name": "Overworld.mid", "title": "Overworld", "artist": "Koji Kondo",
 "album": null, "album_artist": null, "genre": null, "year": 1985, "track": 1, "length": 84.2,
 "starred": true, "tags": ["chill"], "imported": 1761000000, "added_by": null}
```

What's playing:

```json
{"song": {...}, "playlist": {"id": "...", "name": "Mornings"}, "paused": false,
 "position": 12.5, "length": 84.2, "speed": 1.0, "volume": 0.8}
```

Commands are the ones Preferences > Keyboard lists, by id: `play`, `pause`, `toggle_pause`, `next_track`, `previous_track`, `seek_back`, `seek_forward`, `restart_song`, `toggle_visualizer`, `toggle_fullscreen`, `go_to_songs` and so on. `GET /v1/commands` has them all.

## Songs added by programs

Every song a program adds that wasn't in your library yet is marked with that program's name (`added_by`), on all your devices. Search `addedby:name` in Songs to see them, or `-addedby:name` to leave them out. If a program fills your library with songs you don't want, Preferences > Integrations has **Remove the N songs it added...** beside it, which opens the usual Remove dialog (with "Remove from all my devices" when your library syncs). Removing the program itself only stops its token: its songs stay.

## Events

`GET /v1/events` keeps the connection open and sends:

- `now_playing`: the song, playlist, pause, speed or volume changed. Its data is what's playing, as above.
- `seeked`: the song jumped (a seek, or a loop back to the start): `{"position": 30.0}`.
- `library_changed`: songs were added, removed or edited: `{}`. Ask `/v1/songs` again for the new state.

A comment line (`: keep-alive`) comes every 25 seconds when nothing else does.

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
