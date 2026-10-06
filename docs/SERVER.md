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

- **Rotation** (`library/rotation.rs`, `server/authority.rs`): replacing a key with a new one. A rotation record says "key A is now key B": asked for with key A (the request signed by A, and B's signature showing B is held) or confirmed by email (recovery), and signed by the authority, which keeps every record (`GET /api/v1/identity/rotations?key=` gives the chain that led to a key). The new key has to be one the authority hasn't seen.
- **Applying it elsewhere**: servers aren't told. The app keeps the chain beside the key (`identity.rotations.json`, and in an export) and shows it to each server the first time it talks to it in a session (`POST /api/v1/identity/apply`); the server checks the authority's signature (no network call needed) and moves everything from the old key to the new one: scripts, encores, reports, admin rights, and bans (a rotation is no way out of one). The old key is refused from then on. A server that was down or unreachable catches up whenever the app next uses it. Records already issued keep working even if the authority is down; only new rotations wait for it.
- **Precedence**: an email recovery replaces whatever the current key is, including one rotated in by a thief, and a stolen key can't take the email off or change it (that needs a code sent to the address). There's no notice email for a key-signed rotation: the authority keeps only a hash of the address, so it can't write to it unasked.
- **The publisher**: the key the official scripts are published with can be rotated like any other (`synththing publisher-rotate`). Each server follows its `publisher` through its rotations; the official server lists them in `/api/v1/info`, and the app checks them against the official authority and keeps them, so the gold name, updates of the bundled scripts and their links follow the new key (and an app released before the rotation learns of it the first time it talks to the server).
- Each server has a list of authorities it trusts (`authorities` in `server.json`, the official server's key by default); an authority trusts itself too.

### Email (authorities only, and only when configured)

The code is part of the program (`server/mail.rs`); it's off unless the server's config has `mail` settings and marks the server as an authority. Mail goes through SendGrid's HTTP API (with `ureq`, which the program already has), its API key in a file in the data folder.

- Attaching an email: signed by the key, confirmed by a code sent to the address. Codes are eight letters and digits (`ABCD-EFGH`), work for 30 minutes, five tries.
- Changing it, or taking it off: needs a code sent to the current address too (typed in, since the server doesn't have it).
- Stored only as a hash, never the address: Argon2id, salted with a secret of the server's (`email.pepper` in its data folder). Recovery works like a password check: the user types their email, the server hashes it to find the identity, and sends the code to the address just typed.
- Recovery requests are unsigned (the key may be gone) and limited: 5 a day per address asking, 3 a day per identity. The answer is the same whether the address is attached to anything or not (and when an identity's recovery is paused or used up, no email goes, but the answer doesn't say so). The rotation happens only when the emailed code is entered. Every code's email has a "that wasn't me" link (`GET /api/v1/identity/cancel`): it cancels the code and counts a strike against the address that asked for it (three in a week ban it for a week). Cancelling a recovery pauses recovery for that identity for 24 hours; cancelling an email change (from the current address) or an attachment (from the new one) pauses changes to that identity's email for 24 hours.
- On an authority with mail, keys without an email attached (and anonymous uploads) get `unverified_uploads_per_day` (3) instead of `uploads_per_day`.

## Uploads

- **What's uploaded**: the `.lua` only (no settings, saved data or controls files), with metadata: name, description, category, tags, the name posted under, the app version it was published from, its minimum app version, and what it was remixed from (if anything).
- **License**: everything on a server is under its stated license, CC BY 4.0 on the official server, shown in the publish dialog. Credit is given through remix links.
- **Categories**: one of Visualizer, Game, Toy (interactive, not a game), Example. The bundled scripts use the same list.
- **Limits**: a size cap (256 KB, sprites are inline), a few uploads a day per key and per IP.
- **Copies**: each version is fingerprinted (`library/fingerprint.rs`: its Lua tokens, by `full_moon`'s lexer, so spacing and comments don't count, as a MinHash of 5-token runs). An upload that's 80% or more the same as someone else's script here, or a bundled visualizer or game, is refused unless it's a remix of it (or of something that one was remixed from); a remix whose code is exactly the original's is refused too. An author's own scripts, and remixes of them, are never copies of their new ones; scripts under 40 tokens aren't judged.
- **Versions**: the owner's update adds a version; old versions stay downloadable. Installs record which version they are.
- **Minimum app version**: every entry in `HOST_API` gets the version it arrived in (a test makes sure new ones have it). At publish, the app's Lua analysis lists the host functions and `script_options` keys the script uses, and the minimum is the newest of those; the server checks it the same way. Indirect calls (`_G["name"]`) aren't seen; the version it was published from is kept as an upper bound. The Library warns about (or hides) scripts the user's app is too old for.

## Official scripts

The scripts that come with synththing are published on the official server too, by CI from the repository (`synththing publish-bundled`, `.github/workflows/scripts.yml`) on every push that changes them, signed with the official publisher key, whose public half is built into the app (`OFFICIAL_PUBLISHER`): scripts signed by it show as official. Each goes by its slug (its file name), so a changed script is its next version; one deleted from the repository stays on the server.

The app still ships them, so a first run works offline. At startup, each bundled visualizer and game in the scripts folder is marked as installed from the official server (a `.source.json` naming it by slug, its bundled source kept as its original), so Update and Restore original work on it like on anything installed. The first update check finds its ID by slug, and which version it is by its contents; a bundled copy matching no published version is taken to be newer than what's published if the app it came with is (a development build), and isn't offered an older one.

## Remixes

- An installed script's sidecar records where it came from (server, ID, version). Publish remix... (beside Update) publishes it as a remix of that version: the upload's `remix_of` names the original by ID (or, for a bundled script's copy, by author and slug) and the version by its SHA-256. A script on another server is recorded as named (its server, ID, and author and slug), and the app's link to it opens it from there, asking first to add that server, or turn it on, if it isn't in use. A copy of a bundled script with no sidecar is recognized by its fingerprint and published as a remix of it.
- A script lists its remixes ("Remixes (3)"), and a remix links to its original ("Remix of ..."). Encores aren't shared: the original keeps its own, and a remix earns its own.
- Deleting a script whose remixes exist: the original goes; the remixes then say "remixed from a deleted script".

## Previews

Rendered by the server on each upload and each new version, through a queue (one at a time), each in a separate `synththing preview` process with a time limit and a Lua memory limit, so a hostile script can't take the server down (`preview.rs`).

- No audio is encoded and ffmpeg isn't needed: frames are drawn with `offline::Stage` (the synth still runs, since scripts react to the sound, with the starter pack's TimGM6mb), and only images are kept.
- Material: about 12 seconds from the middle of each starter song, at 320 x 180, run at 24 fps and kept at 12.
- Each frame is scored for how much there is to see: contrast, colour variety, how much changed since the previous frame. The best-scoring 48 frames in a row (4 seconds) become the animation, the single best frame the still.
- A script with `record_auto` is rendered in Auto, so a game shows itself being played, not its menu.
- The first frame with anything on it is kept as the still straight away, so a script that stalls (stopped at the time limit) or breaks later still has one. A script that never draws anything has no preview, and the app says so.
- Stored as a PNG sprite sheet (8 frames to a row) plus the still PNG, per version. The app animates the sheet by drawing one cell at a time; a web page can do the same with CSS. Only the newest version's is made; asking for a version without one gets the newest earlier one's.
- Before an upload is taken, the server checks it runs on its version (`preview --check`: it compiles and plays a few seconds without an error), and refuses it with the error if not.
- The app's script picker shows previews too: installed scripts keep the server's, and any other script gets one made by the app the first time it's looked at (cached by its source's SHA-256).

## Encores and reports

- An **encore** is the only vote: one per key per script, signed, can be taken back, not for your own scripts. A user's total is the sum over their uploads.
- A **report** (signed) goes to the admins: a reason (malicious; defamatory or derogatory; intense flashing without a warning; someone else's work, uncredited; spam; other), optional details, and what it's about: the version, lines of its code, and sprites written in it (by the line they're registered on). One open report per key per script. There's no downvote.
- Limits per address: 60 encores an hour, 10 reports a day. Banned keys and addresses can't upload, give encores or report (they can still browse and download).
- Keys cost nothing to make, so encores can be faked with many keys, and a banned key can be swapped for a new one (a rotation carries its bans along; a fresh key doesn't, which is what address bans are for). A key ban can take its addresses along: those it was seen on in the last 30 days are banned for 30 days, and any new address the banned key comes back from is banned for 30 days too, so the ban follows it as long as it keeps coming back (new keys on a banned address are refused while that lasts, but aren't banned themselves, since addresses are shared). An authority with mail is stricter with keys without an email (fewer uploads a day). Every encore counts the same, whatever the key's age: making them count less would punish newcomers. Not perfect; fine at this scale.

## Moderation

- **Admins** are keys listed by the server (added with `synththing admin add-admin <id>`), and the server's own key.
- One endpoint, `POST /api/v1/admin`, signed by an admin, takes an action (`AdminAction`): reports, a script's extra metadata (uploader key and addresses, versions, its reports) and source (hidden or not), hide, unhide, delete, ban (a key or an address, for a while or for good, optionally hiding everything the key uploaded), unban, bans, add-admin, remove-admin, admins, resolve.
- **`synththing admin <command>`** on the server sends those, signed with the server's own key from its data folder, to the running server at its configured address: it works from SSH, under systemd, and in Docker (`docker exec <container> synththing admin ...`). Still to come: `update`.
- **Moderation** in the app's Library tab, for admin keys: reports with the lines they point at highlighted in the code and the sprites drawn, and the actions above, signed with the admin's key.
- **Web admin panel**: parked. Moderation in the app does the job and stays in step with it; if the app ever builds for the web (egui runs in a browser, see TODO), that's the web version.
- **Rules** and a contact address in `/api/v1/info` (contact@p51.nl on the official server), for reports and takedown requests.
- **Documents**: a server can offer documents (`documents` in `server.json`: a title, a Phosphor icon, a Markdown file), like its EULA and privacy policy; `/api/v1/info` lists them, `GET /api/v1/documents/{slug}` serves them, and the app's Script Library has an icon button for each (hover: "Read EULA") that opens it, rendered (`egui_commonmark`). The official server's are in `deploy/official/`.

## Deletion and retention

- **Deleting a script**: by its owner (signed) or an admin.
- **Deleting an identity's data** (`server/deletion.rs`, Delete my data... in Preferences > Library): asked for with the key, on each server separately; where the identity has an email attached on a server with mail, confirmed with a code sent to it too (with a "that wasn't me" link). Nothing goes at once: its scripts are hidden (deleted by their author: out of every listing, search and download), the encores it gave set aside, its admin rights taken away, and the key refused for anything but restoring, for 30 days, during which Restore my data puts it all back. Then a daily job deletes it for good: its scripts (remixes of them then say "remixed from a deleted script") and their previews, the encores, the email hash, and its key on the reports it made. Bans stay: deleting is no way out of one. (Lost the key? Recover it by email first.)
- **IP addresses** are kept only for rate limits and bans, and deleted after 30 days (bans keep theirs until they end).

## API (v1)

```
GET  /api/v1/info                         name, version, server key, mode, license, rules, contact, authority?
GET  /api/v1/scripts?q=&category=&tag=&sort=new|top&page=
GET  /api/v1/scripts/{id}                 details, versions, author ID and names, encores (?viewer=key: whether they gave one), remix of, remixes
GET  /api/v1/scripts/{id}/source          ?version=  (signed by the server)
GET  /api/v1/scripts/{id}/preview.png     the still
GET  /api/v1/scripts/{id}/preview-sheet.png
POST /api/v1/scripts                      upload
POST /api/v1/scripts/{id}/versions        new version (owner)
DELETE /api/v1/scripts/{id}               owner or admin
POST /api/v1/scripts/{id}/encore          (and DELETE to take it back)
POST /api/v1/scripts/{id}/report
POST /api/v1/admin                        an admin action (signed by an admin)
GET  /api/v1/users/{id}                   names, uploads, total encores
POST /api/v1/identity/rotate              a key-signed rotation (authorities)
GET  /api/v1/identity/rotations?key=      the rotations that led to a key (authorities)
POST /api/v1/identity/status              signed: whether an email's attached (authorities)
POST /api/v1/identity/email               signed: codes to attach, change or take off an email (authorities)
POST /api/v1/identity/email/confirm       signed: the codes (authorities)
POST /api/v1/identity/recover             start a recovery: a code to the email (authorities)
POST /api/v1/identity/recover/confirm     the code: the rotation to the new key (authorities)
GET  /api/v1/identity/cancel?token=       a code email's "that wasn't me" link (authorities)
POST /api/v1/identity/apply               present rotation records (any server)
POST /api/v1/identity/delete              signed: delete the key's data here (in 30 days), or send a code first
POST /api/v1/identity/delete/confirm      signed: the code
POST /api/v1/identity/restore             signed: undo a deletion
```

Errors are JSON (`{ "error": "...", "retry_after": ... }`). Requests are signed with headers (`X-Synththing-Key`, `X-Synththing-Time`, `X-Synththing-Nonce`, `X-Synththing-Signature`); the signature covers the server's key, the method and path, the time, the nonce (random per request, so a replay is spotted) and the body's SHA-256.

## In the app

- **Servers** (Preferences): the official server, always listed, can be turned off but not removed; others added by URL, each turned on or off, showing what it requires (signed only, its license).
- **Library tab**: browse and search all enabled servers (or one), by category and tag, sorted by newest or most encores; a script's page shows its animated preview, description, author, versions, remixes and encores; Install, Encore, Report.
- The app only contacts a server while the Library is open, when publishing, or when asked to check installed scripts for updates. Never on its own at launch.
- **Installing** saves the `.lua` to the scripts folder with a sidecar (`<script>.lua.source.json`: server, ID, version, the installed source's hash) and caches its preview, description and category.
- **Updating** (on request, "Check for updates"): if the script was changed since it was installed, the app says so and shows a diff before overwriting. **Restore original** puts back the installed version, like Restore default for bundled scripts, and only shows when there's something to restore.
- **Publishing** ("Publish..." by the script picker): name, description, category, tags, the name to post under, which server; the license; the minimum app version it worked out. If the user's settings differ from the script's defaults, it offers to add them as a preset (below).
- **The script picker** has a sidebar: the preview of the script under the mouse (or the selected one), its description (its leading comment block), author and category.

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
- **Updating** (`server/update.rs`): the Moderation window's Server tab (and `synththing admin update --check`) says which version's running and which is the newest release. The server knows how it's run (Docker, systemd or by hand): under Docker it says to pull the new image; run by hand, an admin's Update downloads and checks the new binary (the app's updater code), swaps it in, starts the new version and exits; under systemd, where the service can't write its own binary, it leaves `update.request` in its data folder, and `synththing-update.path` starts the root-run update script (which also runs hourly from its timer on the official server). Database migrations run forward-only at startup.

## Build order

1. **Basics**: `serve` with open uploads, listing, search, download, server keys; the Library tab (browse, install with sidecar), Servers in Preferences.
2. **Identity**: keys, signed requests, ownership (versions, delete), user pages, export/import, receipts.
3. **Previews**: the queue, frame scoring, sprite sheets; the picker's sidebar.
4. **Encores, reports and moderation**: `synththing admin`, the admin tab, bans, rate limits, proof-of-work.
5. **Publishing extras**: remixes, copy detection, minimum app version (`HOST_API` versions), presets.
6. **Updates and diffs** for installed scripts.
7. **Authorities**: rotation records, applying them, email attach and recovery.
8. **Deployment**: the official server on the VPS, Docker image, in-place updates.
9. Later, maybe: the app itself on the web (WebAssembly), for browsing and moderating without installing it.
