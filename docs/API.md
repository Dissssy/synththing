# Local API

synththing can let programs on the same computer talk to it: see what's playing, search the library, follow what happens and run commands. Use it for your own integrations: home automation, notifications, stream overlays, game hooks. It's desktop only and off until you turn it on.

## Turning it on

Preferences > Integrations:

1. Tick **Let programs on this computer use the API**. It listens on `127.0.0.1`, port 7340 unless you pick another. Nothing outside this computer can reach it.
2. Type a name for your program and click **Add**. Copy the token it shows: only its hash is kept, so it isn't shown again (remove the program and add it again for a new one).
3. Tick what that program may do: **Read** (the library, what's playing, events) and **Control** (run commands).

Send the token with every request:

```
Authorization: Bearer st_...
```

Where a header can't be set (a browser's `EventSource`), add `?token=st_...` to the URL instead.

## Requests

Everything is JSON. Errors come back as `{"error": "..."}` with a status: 401 (no or unknown token), 403 (the token isn't allowed that), 404, 409 (the command can't run right now) or 503 (synththing didn't answer in time).

| Request | Needs | What it gives |
| --- | --- | --- |
| `GET /v1` | any token | `{"app", "version", "read", "control"}`: who you are to it |
| `GET /v1/now-playing` | read | What's playing (below) |
| `GET /v1/songs?q=&offset=&limit=` | read | `{"total", "songs": [...]}`: the songs `q` finds, by file name, at most 500 at a time |
| `GET /v1/songs/{key}` | read | One song |
| `GET /v1/playlists` | read | `[{"id", "name", "smart", "songs": [keys]}]` |
| `GET /v1/commands` | read | `[{"id", "name"}]`: every command there is |
| `POST /v1/commands/{id}` | control | Runs it: 204 when it ran |
| `GET /v1/events` | read | Server-Sent Events (below) |

`q` works like the Songs search box: filters such as `artist:bonno`, `year:1990..1999`, `is:starred` or `tag:chill`, and any other words must each be in the title, artist, album or file name.

A song:

```json
{"key": "3f2a...", "file_name": "Overworld.mid", "title": "Overworld", "artist": "Koji Kondo",
 "album": null, "album_artist": null, "genre": null, "year": 1985, "track": 1, "length": 84.2,
 "starred": true, "tags": ["chill"], "imported": 1761000000}
```

What's playing:

```json
{"song": {...}, "playlist": {"id": "...", "name": "Mornings"}, "paused": false,
 "position": 12.5, "length": 84.2, "speed": 1.0, "volume": 0.8}
```

Commands are the ones Preferences > Keyboard lists, by id: `play`, `pause`, `toggle_pause`, `next_track`, `previous_track`, `seek_back`, `seek_forward`, `restart_song`, `toggle_visualizer`, `toggle_fullscreen`, `go_to_songs` and so on. `GET /v1/commands` has them all.

## Events

`GET /v1/events` keeps the connection open and sends:

- `now_playing`: the song, playlist, pause, speed or volume changed. Its data is what's playing, as above.
- `seeked`: the song jumped (a seek, or a loop back to the start): `{"position": 30.0}`.
- `library_changed`: songs were added, removed or edited: `{}`. Ask `/v1/songs` again for the new state.

A comment line (`: keep-alive`) comes every 25 seconds when nothing else does.

## Trying it

```sh
TOKEN=st_...
curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:7340/v1/now-playing
curl -X POST -H "Authorization: Bearer $TOKEN" http://127.0.0.1:7340/v1/commands/next_track
curl -N -H "Authorization: Bearer $TOKEN" http://127.0.0.1:7340/v1/events
```

On Windows PowerShell:

```powershell
$h = @{ Authorization = "Bearer st_..." }
Invoke-RestMethod http://127.0.0.1:7340/v1/now-playing -Headers $h
Invoke-RestMethod -Method Post http://127.0.0.1:7340/v1/commands/toggle_pause -Headers $h
```
