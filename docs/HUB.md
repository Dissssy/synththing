# Running a hub

A hub is one of your devices, with no window, that keeps every song. Run it on something that's always on (a home server, a NAS, a VPS) and your devices' songs have somewhere to go even when none of your other devices is open: a song you add on your PC streams straight to the hub, and the hub sends it on to your phone once that's open.

It's signed in like any other device, and syncs what the app does: settings, playlists, the wishlist and the song library. It always wants every song (as if Preferences > Account > "Every song, not only those in playlists" were ticked).

## Signing it in

On a signed-in device, open Preferences > Account > Sign in another device and copy the link. Then, on the hub's machine:

```
synththing hub pair '<link>' --name "Home hub"
```

Quote the link: it has a `#` in it. `--name` is what your other devices show for it (by default the computer's name). The link works once, for 10 minutes.

Or give `synththing hub` the link in the `SYNTHTHING_PAIR_LINK` environment variable (and the name in `SYNTHTHING_HUB_NAME`): it signs in with it when it starts, if it isn't signed in already. A hub that isn't signed in waits for one of these, so `hub pair` works while it runs. Run in a terminal, it also asks for the link: paste it in.

## Running it

```
synththing hub
```

It runs until stopped (Ctrl+C), logging what it does to the terminal and to `synththing.log` in its config folder. Everything it keeps (sign-in, settings, playlists, songs in `music/`) is in the config folder: `--config-dir DIR` or `SYNTHTHING_CONFIG_DIR` puts it somewhere of your choosing, which is the easiest way to run one on a machine that also runs the app. Only one synththing runs per config folder.

A branch build has sync off unless it's asked for: `synththing --online sync hub`.

## Songs straight to and from it

Your devices send the hub the songs it doesn't have, and take the songs they want from it, directly, rather than through the relay on the official server, once they reach it: no size limit, and as fast as your network. It serves them on port 7390 (`SYNTHTHING_HUB_PORT` for another). The songs go sealed with your sync key and every request is signed with your identity, so plain HTTP is safe, and nothing but your own devices can use it.

Where your devices find it goes in the synced song transfers: its address on its network (found by itself, except in Docker), and those in `SYNTHTHING_HUB_URL` (comma-separated, nearest first), such as `http://192.168.1.20:7390` and, with port 7390 forwarded to it on your router, `http://203.0.113.5:7390` or a domain name that points home. A device tries each in order, every ten minutes, and while it reaches none, songs go through the relay as before: the hub listens there too. With several devices sending, each takes different songs, the ones fewest devices have first.

It takes no songs while its disk has under 2 GB free.

The hub can't tell from inside your network whether its public address works, so the official server checks it from outside: as the hub starts and every 6 hours, it fetches the hub's hello at each address in `SYNTHTHING_HUB_URL` that isn't on your own network. The log says `http://203.0.113.5:7390 works from the internet`, or what to fix: the port isn't forwarded or the firewall blocks it, the name points at an old address, another machine answers, and so on. To check now, after changing your router, say, run `synththing hub check` (in Docker: `docker exec synththing-hub synththing hub check`). Once an address has worked, the server keeps checking it itself every 6 hours, even while the hub is down, and if it fails 3 times in a row (about 18 hours) your devices get a notification saying what to fix, and another once it works again. Your devices also show it under Your devices in their sync settings: whether that device reaches the hub directly (with a button to check again), and what the hub's last check found for each address.

Live messages between your devices (panels shared to your other devices, say) go through the hub too while a device reaches it, and through the official server when it doesn't. The hub passes each one on both ways, so devices that reach it and devices that don't still hear each other.

A device can move its songs only through the hub: tick **Only through my hub, once I have one** under Songs in its sync settings (What this device syncs). It then sends its songs to the hub alone and takes the songs it wants from the hub alone, directly when it reaches it and through the relay when it doesn't, and your other devices send it nothing: the hub passes their songs on. Your other devices keep trading among themselves as before. It suits a phone, say, that should only ever talk to your hub. Until you have a hub, it trades with every device as usual.

One device is the hub at a time. Starting a hub makes it the hub; the one before stops serving songs (its log says "another device is the hub now") and carries on as an ordinary device until you stop it.

## Its local API

The hub answers the local app API (docs/API.md) for programs you let in, the same as the app does for what's about the library: songs, playlists, tags, the wishlist, adding songs and checking them first (`/v1/staged`), and the events about them. A hub plays nothing, so what's about playing (now playing, play, the queue, seek, volume, soundfonts, scripts, commands, the visualizer) answers 409.

Let a program in with:

```
synththing hub token bridge --grant read,add_songs,download_songs
```

It prints the program's token once. The permissions are `read`, `add_songs`, `download_songs`, `edit_library`, `control`, `panel` and `share`. `synththing hub token` lists the programs let in, and `synththing hub token bridge --remove` stops letting one in. Run it again for a program let in already to change what it may do: it keeps its token. A running hub picks these up straight away.

A program with `panel` and `share` can show a panel (`PUT /v1/panel`): the hub shows none itself, but it shows on your other devices, in their Apps tab, and what's done with it there goes back to the program, as from any device's shared panel.

The API runs while any program is let in, on port 7340 (`SYNTHTHING_API_PORT` for another), on this machine only. `SYNTHTHING_API_LAN=true` lets in other machines too, which is what another Docker container is: then keep the port off the internet (don't publish it, or only to `127.0.0.1`), as anything that reaches it with a token can use it. In Docker, run the token command inside the container: `docker exec synththing-hub synththing hub token bridge --grant read,add_songs`.

## Docker

`deploy/hub.compose.yml` runs it from the published image (ghcr.io/dissssy/synththing), keeping everything in `./hub`. Put a fresh link in its `SYNTHTHING_PAIR_LINK` and a name in `SYNTHTHING_HUB_NAME`, then start it:

```
docker compose -f hub.compose.yml up -d
```

Set `SYNTHTHING_HUB_URL` to where your devices reach it (the machine's address on your network, port 7390), as a container doesn't know that itself. Or start it without a link and sign it in while it waits: `docker attach synththing-hub` (or Portainer's Attach console), paste the link, and detach with Ctrl+P Ctrl+Q; or `docker exec synththing-hub synththing hub pair '<link>' --name "Home hub"`. Give it a name: in a container the computer's name is the container's random one. Its log is `docker logs synththing-hub`. Watchtower (see docker-compose.yml) keeps it up to date the same way as a server.

## Under systemd

Like the server's unit (deploy/synththing.service), with `ExecStart=/opt/synththing/synththing hub`, `Environment=SYNTHTHING_CONFIG_DIR=/var/lib/synththing-hub` and that folder as `ReadWritePaths`. Sign it in first with the same `SYNTHTHING_CONFIG_DIR`, as the user it runs as.
