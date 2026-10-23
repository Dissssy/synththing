# Running a server

A synththing server is `synththing serve` (docs/cli.md; how it works is in docs/SERVER.md): it holds a script library, and, when turned on, signs devices in, syncs them and relays songs between them. It's one program, one data folder, behind a reverse proxy for HTTPS. Two ways to run it: in Docker, or directly under systemd. The files for both are in `deploy/`.

Either way, its data folder holds:

- `server.json`: its settings, written with defaults on first run. See `deploy/server.example.json` and docs/cli.md.
- `server.key`: its signing key, made on first run. Back it up: apps remember a server's key, and warn when it changes.
- `library.db`: the scripts, and everything else it keeps (SQLite).
- `relay/`: songs waiting to move between someone's devices (with `"sync": true`).

Behind a reverse proxy, set `"behind_proxy": true` so upload limits see the client's address, not the proxy's, and set `contact` to an address for reports and takedown requests.

## Docker

Two compose files, the same apart from where the image comes from:

- `deploy/docker-compose.yml`: the published image, `ghcr.io/dissssy/synththing:latest`. It's from 0.4.1 and not kept up to date for now: release.yml's `docker` job is off until there's a lighter server-only build. Use the next one.
- `deploy/docker-compose.build.yml`: built from this repository (`Dockerfile`; the first build compiles everything, several minutes).

```
cd deploy
docker compose up -d                                # the published image
docker compose -f docker-compose.build.yml up -d    # or built here
```

In Portainer, add either as a stack (from the repository, with the file's path). Its data goes in `deploy/data` (a bind mount: change the left side of `./data:/data` to keep it elsewhere). It listens on port 7381; point the reverse proxy there, and set `behind_proxy` in `data/server.json`, then restart the container.

Updating: pull the new image (`docker compose pull && docker compose up -d`), or rebuild.

## systemd

For a server that keeps itself up to date: the release binary, run by systemd, with an optional timer that installs new releases as they come out.

As root:

```
useradd --system --home /var/lib/synththing --shell /usr/sbin/nologin synththing
mkdir -p /opt/synththing /var/lib/synththing
chown synththing:synththing /var/lib/synththing

# the program (the newest release's Linux build)
curl -fsSL -o /opt/synththing/synththing \
  https://github.com/Dissssy/synththing/releases/latest/download/synththing-linux-x86_64
chmod 755 /opt/synththing/synththing

# settings: edit name and contact
cp server.example.json /var/lib/synththing/server.json
chown synththing:synththing /var/lib/synththing/server.json

# the service
cp synththing.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now synththing
```

(`server.example.json` and the unit files are in `deploy/`; copy them over first, or fetch them from the repository.)

`journalctl -u synththing -f` shows what it's doing (a line per upload).

### Updating by itself

`deploy/synththing-update.sh` checks GitHub for a newer release, downloads its Linux build, checks it against the SHA-256 GitHub publishes, swaps it in and restarts the service. Run it by hand, or every hour with the timer:

```
cp synththing-update.sh /opt/synththing/
chmod 755 /opt/synththing/synththing-update.sh
cp synththing-update.service synththing-update.timer synththing-update.path /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now synththing-update.timer synththing-update.path
```

`systemctl list-timers synththing-update` shows when it runs next; `journalctl -u synththing-update` what it did.

`synththing-update.path` runs it straight away when an admin asks the server to update (`synththing admin update`, or the app's Moderation > Server tab): the service can't write its own binary, so it leaves `update.request` in its data folder for the update script, which runs as root. Without the path unit, the timer picks the request up within the hour.

## Email (an identity authority)

An authority sends codes, for attaching a recovery email and recovering an identity, through SendGrid:

1. In SendGrid, Settings > Sender Authentication: authenticate the domain the mail comes from (its CNAME records go in the DNS; on Cloudflare, DNS only, not proxied).
2. Settings > API Keys > Create API Key, Restricted Access, with only Mail Send (Full Access). It's shown once.
3. Put it in the data folder, readable only by the server: `install -m 600 -o synththing -g synththing /dev/stdin /var/lib/synththing/sendgrid.key`, paste the key, then Ctrl+D.
4. In `server.json`: `"authority": true`, `"public_url": "https://scripts.example.org"` (for the links in the emails), and `"mail": { "from": "synththing@example.org" }`. Restart; it says "an identity authority, sending email" as it starts.

Keep `email.pepper` (made in the data folder on first run) with the backups: without it, the attached emails can't be recognised.

## Sign-in links on Android

The official server's sign-in links (`/pair#...`) open in the Android app once Android trusts the server for them:

1. Get the SHA-256 fingerprint of the key the APKs are signed with: `keytool -list -v -keystore release.jks` (the `SHA256:` line, `AB:CD:...`).
2. In `server.json`: `"android_fingerprints": ["AB:CD:..."]`. Restart. `https://synththing.p51.nl/.well-known/assetlinks.json` should list it.
3. Android checks it when the app is installed or updated. Until then (or without it) the link opens the server's page in the browser, whose Open in synththing button still hands it to the app.

## Sync

Signed-in apps keep their settings, playlists and songs the same across devices through their authority (docs/SERVER.md, Sync). On the authority, add `"sync": true` to `server.json` and restart. Everything is encrypted on the devices, so the server can't read it: at most 16 blobs of 1 MB per identity for settings and playlists, and songs either streamed straight between two open devices (kept nowhere) or left in `relay/` for up to 7 days, up to `relay_song_mb` each (250) and `relay_mb_per_identity` in all (2048). Behind nginx, the relay needs the `/api/v1/relay/` block below (no body size limit, no buffering, long timeouts); Caddy needs nothing extra.

## Feedback intake

A server can hand its feedback out to be filed as GitHub issues by an hourly Claude run (docs/feedback-intake.md says what it does):

1. In `server.json`: `"intake": true`. Restart. The server makes `intake.token` in its data folder: that's the intake token.
2. In the Claude project's settings, the cloud environment: allow the server's host (synththing.p51.nl) under Network access, and add the token as `SYNTHTHING_INTAKE_TOKEN`.
3. Ask the project to set up the "Feedback intake" thread: it schedules its own hourly run.

Notes stay waiting until a run says it has taken them, so nothing is lost if a run fails. To have a note filed again, clear its `taken` (`sqlite3 library.db "UPDATE feedback SET taken = NULL WHERE id = 12"`). To change the token, delete `intake.token` and restart.

## Moderating

`synththing admin` on the server talks to it, signed with its own key (docs/cli.md): `synththing admin --data /var/lib/synththing reports` under systemd (as root, or the synththing user), `docker exec <container> synththing admin --data /data reports` in Docker. To moderate from the app instead, make your identity an admin: `synththing admin --data ... add-admin '#yourid'` (the server needs to have seen your key: give something an encore first, or pass the whole key). The Library tab then has Moderation.

## Reverse proxy

Caddy (HTTPS certificates are automatic):

```
scripts.example.org {
    reverse_proxy localhost:7381
}
```

nginx:

```
server {
    server_name scripts.example.org;
    location / {
        proxy_pass http://127.0.0.1:7381;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    }
    # Songs between devices: streamed straight through as they come (of
    # any size: the server limits those it keeps to relay_song_mb).
    location /api/v1/relay/ {
        proxy_pass http://127.0.0.1:7381;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        client_max_body_size 0;
        proxy_request_buffering off;
        proxy_buffering off;
        proxy_read_timeout 1h;
        proxy_send_timeout 1h;
    }
}
```

Check it from anywhere with `curl https://scripts.example.org/api/v1/info`, then add the address in the app under Preferences > Online Library.

## The official server

synththing.p51.nl runs the systemd setup above, with the update timer, behind Caddy. It's the identity authority (with SendGrid mail, from synththing@p51.nl), and its publisher is the official one (the default), whose key `.github/workflows/scripts.yml` publishes the bundled scripts with (the `SYNTHTHING_PUBLISH_KEY` secret).

Its front page is `deploy/website/index.html`, a plain-language tour of what the app does, copied to `/var/www/synththing/`. Caddy sends the server's own paths to it and serves the page for the rest:

```
synththing.p51.nl {
    @server path /api/* /pair /.well-known/*
    handle @server {
        reverse_proxy localhost:7381
    }
    handle {
        root * /var/www/synththing
        file_server
    }
}
```

The update script keeps it at the newest release's copy once `/var/www/synththing` exists, so update the page in the repo when a release adds something worth telling people about.

The docs are at `/docs/` beside it: this folder's Markdown, rendered by [mdBook](https://rust-lang.github.io/mdBook/) (`book.toml`, with `docs/SUMMARY.md` as the table of contents). Each release carries them as `synththing-docs.tar.gz`, and the update script unpacks the newest into `/var/www/synththing/docs/`. To see them locally: `mdbook serve --open`. A new page goes in `docs/SUMMARY.md` too.

Its EULA and privacy policy are `deploy/official/eula.md` and `privacy.md`, copied to `/var/lib/synththing/` and listed in its `server.json`:

```json
"documents": [
  { "title": "EULA", "icon": "scroll", "file": "eula.md" },
  { "title": "Privacy Policy", "icon": "shield-check", "file": "privacy.md" }
]
```

They're read fresh each time, so an edit shows straight away (no restart needed for the text; a change to the list needs one).

### Replacing the publisher key

If the publisher key might have got out, `deploy/rotate-publisher.sh` (copied to `/opt/synththing/`) moves the official publisher to a new key: run `sh /opt/synththing/rotate-publisher.sh` as root on the VPS and paste the current secret key when asked (it isn't shown). The official scripts move to the new key, the old one stops working, and apps follow along the next time they talk to the server. The new secret goes to GitHub's `SYNTHTHING_PUBLISH_KEY` with `gh` if it's installed and signed in there, and into `/root/synththing-publish.key` either way: put it in Bitwarden (and GitHub, if `gh` didn't), then `shred -u` the file. This is for the official server's setup; elsewhere, `synththing publisher-rotate` does the same (docs/cli.md).
