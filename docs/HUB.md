# Running a hub

A hub is one of your devices, with no window, that keeps every song. Run it on something that's always on (a home server, a NAS, a VPS) and your devices' songs have somewhere to go even when none of your other devices is open: a song you add on your PC streams straight to the hub, and the hub sends it on to your phone once that's open.

It's signed in like any other device, and syncs what the app does: settings, playlists, the wishlist and the song library. It always wants every song (as if Preferences > Online Library > "Every song, not only those in playlists" were ticked).

## Signing it in

On a signed-in device, open Preferences > Online Library > Sign in another device and copy the link. Then, on the hub's machine:

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

## Docker

`deploy/hub.compose.yml` runs it from the published image (ghcr.io/dissssy/synththing), keeping everything in `./hub`. Put a fresh link in its `SYNTHTHING_PAIR_LINK` and a name in `SYNTHTHING_HUB_NAME`, then start it:

```
docker compose -f hub.compose.yml up -d
```

Or start it without a link and sign it in while it waits: `docker attach synththing-hub` (or Portainer's Attach console), paste the link, and detach with Ctrl+P Ctrl+Q; or `docker exec synththing-hub synththing hub pair '<link>' --name "Home hub"`. Give it a name: in a container the computer's name is the container's random one. Its log is `docker logs synththing-hub`. Watchtower (see docker-compose.yml) keeps it up to date the same way as a server.

## Under systemd

Like the server's unit (deploy/synththing.service), with `ExecStart=/opt/synththing/synththing hub`, `Environment=SYNTHTHING_CONFIG_DIR=/var/lib/synththing-hub` and that folder as `ReadWritePaths`. Sign it in first with the same `SYNTHTHING_CONFIG_DIR`, as the user it runs as.
