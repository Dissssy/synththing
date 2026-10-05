# The script library: `synththing serve`

A design for sharing visualizer scripts: servers anyone can run that hold uploaded scripts, and a Library in the app to browse, install, publish and update them. One official server (synththing.p51.nl), plus any self-hosted ones the user adds. Nothing here is built yet; this is the plan to build from, in the order at the end.

Not covered: the local API for editing scripts in VS Code. That's the app talking to an editor on the same machine, a separate piece (see TODO).

## Goals

- Share and find scripts without accounts, emails or passwords: identity is a key pair the app makes.
- Anyone can run a server, and nothing has to depend on the official one, including identity (see Authorities).
- Uploads are free to remix, and remixes credit what they came from.
- Scripts stay safe to run: they're already sandboxed (no files, no OS); the worst one can do is use CPU (the watchdog stops it) or play notes.
- Small and simple to host: one binary, one SQLite file, a folder of files, behind a reverse proxy.

## Pieces

1. **The server**: `synththing serve`, the same binary as the app, headless. A JSON API over HTTP (`/api/v1/...`), SQLite for data, files on disk for sources and previews. HTTPS comes from a reverse proxy (Caddy on the official server).
2. **The admin command**: `synththing admin <command>`, talking to the running server over a local-only channel (see Moderation).
3. **The Library tab** in the app: browse and search every enabled server, see previews, install, publish, update.
4. **Servers in Preferences**: the list of servers the Library uses.

Stack: synchronous and small, so the desktop app doesn't carry an async runtime it doesn't need. `tiny_http` (a thread pool), `rusqlite` (bundled SQLite, FTS5 for search), `ed25519-dalek` (signatures), `lettre` (email, only used when configured), `ureq` (already used by the updater) on the client.

## Identity

- The app makes one Ed25519 key pair the first time it's needed, kept in the config folder. The public key is the identity, the same on every server.
- A user's ID is a short form of the public key (base32 of its hash, e.g. `k3f9q2xa`), shown as `#k3f9q2xa`. Browsing "everything by this person" goes by ID.
- Names are per upload and free-form. The server records which names each key has posted under, so a user page reads: `#k3f9q2xa`, posts as "ethan", "dissy".
- Signed: uploads, new versions, deletes, encores, reports, identity changes. A signature covers a canonical JSON of the request (the script's SHA-256, its metadata), a timestamp (rejected if more than 5 minutes off) and the server's own ID (its public key), so a signed request can't be replayed later or on another server.
- Export identity saves the key to a file, encrypted with a passphrase if one is given; Import loads it on another machine. Losing the key loses the identity (ownership, encores received), never the scripts themselves, unless it's recovered (below).

### Server modes

- **Open** (default): unsigned uploads are allowed, but they're anonymous and can never be edited or deleted by their uploader. Signed uploads get ownership.
- **Signed only**: unsigned uploads are refused. The server says which in `/api/v1/info`, and the app signs everything when a server requires it.

## Server keys

Every server has its own Ed25519 key (separate from HTTPS), made on first run:

- **Recognising a server**: the app remembers each server's key on first contact and warns if it ever changes, which catches a hijacked domain or a swapped server even with a valid certificate.
- **Signed responses**: listings and sources are signed, so a cache or mirror can't alter a script unnoticed.
- **Receipts**: an accepted upload comes back with a signed receipt (script, version, hash, time) the uploader keeps.
- **Authority**: a server whose key is trusted to sign identity changes (next section).

## Authorities: rotation and recovery

An authority is a server whose key is trusted for identity changes. The app and every server keep a list of trusted authority keys, by default only the official one's. Anyone can run their own authority and trust it instead, so identity doesn't have to rely on the official server either.

- **Rotation**: replacing a key with a new one. A rotation record says "key A is now key B", signed by key A (a normal rotation) or confirmed by email (recovery), and countersigned by the authority, which keeps every record.
- **Applying it elsewhere**: servers aren't told. When a user first acts on another server with the new key, the app presents the signed record (or the chain of them); the server checks the authority's signature (no network call needed) and moves everything from the old key to the new one. A server that was down or unreachable catches up whenever the user next uses it. Records already issued keep working even if the authority is down; only new rotations wait for it.
- **Precedence**: an email recovery can replace whatever the current key is, including one rotated in by a thief. A key-signed rotation sends a notice to the attached email (if any) with a "not you? recover" link.

### Email (authorities only, and only when configured)

The code is part of the program; it's off unless the server's config has mail settings (SMTP) and marks the server as an authority.

- Attaching an email: signed by the key, confirmed by a code sent to the address.
- Changing it: needs a code sent to the current address.
- Stored only as a salted hash, never the address. Recovery works like a password check: the user types their email, the server hashes it to find the identity, and sends the code to the address just typed.
- Recovery requests are unsigned (the key may be gone) and rate limited: at most 3 a day per identity and per IP. The rotation happens only when the emailed code is entered. Every recovery email has a "that wasn't me" link: it cancels the request, counts a strike against the requesting IP (enough strikes ban it for a while), and pauses recovery for that identity for 24 hours.

## Uploads

- **What's uploaded**: the `.lua` only (no settings, saved data or controls files), with metadata: name, description, category, tags, the name posted under, the app version it was published from, its minimum app version, and what it was remixed from (if anything).
- **License**: everything on a server is under its stated license, CC BY 4.0 on the official server, shown in the publish dialog. Credit is given through remix links.
- **Categories**: one of Visualizer, Game, Toy (interactive, not a game), Example. The bundled scripts use the same list.
- **Limits**: a size cap (256 KB, sprites are inline), a few uploads a day per key and per IP.
- **Copies**: a byte-identical upload (by SHA-256), or one identical once whitespace is normalized, is refused or marked as a copy of the existing one. Uploading any shipped version of a bundled script is refused.
- **Versions**: the owner's update adds a version; old versions stay downloadable. Installs record which version they are.
- **Minimum app version**: every entry in `HOST_API` gets the version it arrived in (a test makes sure new ones have it). At publish, the app's Lua analysis lists the host functions and `script_options` keys the script uses, and the minimum is the newest of those; the server checks it the same way. Indirect calls (`_G["name"]`) aren't seen; the version it was published from is kept as an upper bound. The Library warns about (or hides) scripts the user's app is too old for.

## Official scripts

The scripts that come with synththing are published on the official server too, by CI from the repository (`synththing publish-bundled`, `.github/workflows/scripts.yml`) on every push that changes them, signed with the official publisher key, whose public half is built into the app (`OFFICIAL_PUBLISHER`): scripts signed by it show as official. Each goes by its slug (its file name), so a changed script is its next version; one deleted from the repository stays on the server.

The app still ships them, so a first run works offline. At startup, each bundled visualizer and game in the scripts folder is marked as installed from the official server (a `.source.json` naming it by slug, its bundled source kept as its original), so Update and Restore original work on it like on anything installed. The first update check finds its ID by slug, and which version it is by its contents; a bundled copy matching no published version is taken to be newer than what's published if the app it came with is (a development build), and isn't offered an older one.

## Remixes

- An installed script's sidecar records where it came from (server, ID, version). Publishing a script with that sidecar marks it a remix of that version.
- A script lists its remixes ("Remixes (3)"), and a remix links to its original ("Remix of ..."). Encores aren't shared: the original keeps its own, and a remix earns its own.
- Deleting a script whose remixes exist: the original goes; the remixes then say "remixed from a deleted script".

## Previews

Rendered by the server on each upload and each new version, through a queue (one at a time), each in a separate `run-script` process with a time limit, so a hostile script can't take the server down.

- No audio is encoded and ffmpeg isn't needed: frames are drawn with `offline::Stage` (the synth still runs, since scripts react to the sound), and only images are kept.
- Material: about 20 seconds from the middle of each of a few standard songs (the starter pack), drawn small (e.g. 320 x 180, or the script's own shape if it's vertical).
- Each frame is scored for how much there is to see: contrast, colour variety, how much changed since the previous frame. The best-scoring 6 seconds becomes the animation, the single best frame the still.
- A script with `record_auto` is rendered in Auto, so a game shows itself being played, not its menu.
- Stored as a PNG sprite sheet (about 48 frames in a grid) plus the still PNG. The app animates the sheet by drawing one cell at a time; a web page can do the same with CSS.

## Encores and reports

- An **encore** is the only vote: one per key per script, signed, can be taken back. A user's total is the sum over their uploads.
- A **report** (signed, with a reason) goes to the admins. There's no downvote.
- Keys cost nothing to make, so encores can be faked with many keys. Mitigations: rate limits per IP, encores from new keys counting less at first, and a small proof-of-work the first time a key uploads or votes. Not perfect; fine at this scale.

## Moderation

- **Admins** are keys listed by the server (added with `synththing admin add-admin <id>`).
- **`synththing admin <command>`**: `add-admin`, `remove-admin`, `reports`, `hide`, `unhide`, `delete`, `ban` (a key or an IP, for a while or for good), `info`, `update`, `login`. It talks to the running server over a Unix socket in the data folder (a localhost port on Windows), so it works from SSH, under systemd, and in Docker (`docker exec <container> synththing admin ...`).
- **Admin tab** in the app, for admin keys: open reports, a script's extra metadata (uploader key and IPs, versions, copies, remix chain), and the actions above, signed with the admin's key.
- **Web admin panel** (later): localhost only unless turned on in the config; signed in with a one-time link from `synththing admin login`.
- **Rules page** and a contact address in `/api/v1/info` (contact@p51.nl on the official server), for reports and takedown requests.

## Deletion and retention

- **Deleting a script**: by its owner (signed) or an admin.
- **Deleting an identity**: signed by the key, or confirmed by email. Removes its names, email hash, encores given, and its scripts (remixes of them then say "remixed from a deleted script").
- **IP addresses** are kept only for rate limits and bans, and deleted after 30 days (bans keep theirs until they end).

## API (v1)

```
GET  /api/v1/info                         name, version, server key, mode, license, rules, contact, authority?
GET  /api/v1/scripts?q=&category=&tag=&sort=new|top&page=
GET  /api/v1/scripts/{id}                 details, versions, author ID and names, encores, remix of, remixes
GET  /api/v1/scripts/{id}/source          ?version=  (signed by the server)
GET  /api/v1/scripts/{id}/preview.png     the still
GET  /api/v1/scripts/{id}/preview-sheet.png
POST /api/v1/scripts                      upload
POST /api/v1/scripts/{id}/versions        new version (owner)
DELETE /api/v1/scripts/{id}               owner or admin
POST /api/v1/scripts/{id}/encore          (and DELETE to take it back)
POST /api/v1/scripts/{id}/report
GET  /api/v1/users/{id}                   names, uploads, total encores
POST /api/v1/identity/rotate              key-signed rotation (authorities)
POST /api/v1/identity/email               attach or change an email (authorities)
POST /api/v1/identity/recover             start a recovery (authorities)
POST /api/v1/identity/apply               present a rotation record (any server)
DELETE /api/v1/identity                   delete everything about a key
```

Errors are JSON (`{ "error": "...", "retry_after": ... }`). Requests are signed with headers (`X-Synththing-Key`, `X-Synththing-Time`, `X-Synththing-Nonce`, `X-Synththing-Signature`); the signature covers the server's key, the method and path, the time, the nonce (random per request, so a replay is spotted) and the body's SHA-256.

## In the app

- **Servers** (Preferences): the official server, always listed, can be turned off but not removed; others added by URL, each turned on or off, showing what it requires (signed only, its license).
- **Library tab**: browse and search all enabled servers (or one), by category and tag, sorted by newest or most encores; a script's page shows its animated preview, description, author, versions, remixes and encores; Install, Encore, Report.
- The app only contacts a server while the Library is open, when publishing, or when asked to check installed scripts for updates. Never on its own at launch.
- **Installing** saves the `.lua` to the scripts folder with a sidecar (`<script>.lua.source.json`: server, ID, version, the installed source's hash) and caches its preview, description and category.
- **Updating** (on request, "Check for updates"): if the script was changed since it was installed, the app says so and shows a diff before overwriting. **Restore original** puts back the installed version, like Restore default for bundled scripts, and only shows when there's something to restore.
- **Publishing** ("Publish..." by the script picker): name, description, category, tags, the name to post under, which server; the license; the minimum app version it worked out. If the user's settings differ from the script's defaults, it offers to add them as a preset (below).
- **The script picker** becomes a list with a sidebar: the selected script's preview animation, description (its leading comment block for local scripts), author and category.

### Presets

- `settings_preset(name, { key = value, ... })` in a script: a named set of setting values, picked in the Settings window. The defaults in the `setting_*` calls stay the only defaults.
- Users can save their own presets (Settings window, Save as preset...), kept beside the script (`<script>.lua.presets.json`) with no change to its code.
- Publishing with settings that differ from the defaults offers to add them as a preset in the script: one `settings_preset(...)` line near the top.

## Hosting

- **The official server**: the Linux release binary on a Hetzner VPS (Ubuntu 24.04, glibc 2.39; the binary is built against 2.35), as a systemd service, data in `/var/lib/synththing`, behind Caddy:

  ```
  synththing.p51.nl {
      reverse_proxy localhost:<port>
  }
  ```

- **Docker**: an image with the binary, the data folder as a volume, and the admin command through `docker exec`.
- **Updating**: no automatic updates. The server checks GitHub for new releases and the admin tab (and `synththing admin info`) says when one is out. It knows how it's run (Docker, systemd or by hand): under Docker it says to pull the new image; otherwise an admin can update it in place (`synththing admin update`, or from the admin tab): it downloads and checks the new binary (the app's updater code), swaps it in, finishes the requests under way, and restarts (systemd restarts it; run by hand, it starts the new version itself). Database migrations run forward-only at startup.

## Build order

1. **Basics**: `serve` with open uploads, listing, search, download, server keys; the Library tab (browse, install with sidecar), Servers in Preferences.
2. **Identity**: keys, signed requests, ownership (versions, delete), user pages, export/import, receipts.
3. **Previews**: the queue, frame scoring, sprite sheets; the picker's sidebar.
4. **Encores, reports and moderation**: `synththing admin`, the admin tab, bans, rate limits, proof-of-work.
5. **Publishing extras**: remixes, copy detection, minimum app version (`HOST_API` versions), presets.
6. **Updates and diffs** for installed scripts.
7. **Authorities**: rotation records, applying them, email attach and recovery.
8. **Deployment**: the official server on the VPS, Docker image, in-place updates.
9. Later: the web admin panel, a public web page for browsing.
