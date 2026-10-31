# The script library: `synththing serve`

How sharing visualizer scripts works: servers anyone can run that hold uploaded scripts, and a Library in the app to browse, install, publish and update them. The app talks to one server, the official one (synththing.p51.nl); `SYNTHTHING_OFFICIAL_URL` points it at another for testing (docs/cli.md). The same server also signs devices in, syncs them and relays songs between them (Signing in another device, Sync). It's all built; the order it was built in is at the end. How to run one is docs/DEPLOY.md.

Not covered: the app's own local API, for programs on the same computer (docs/API.md).

## Goals

- Share and find scripts without accounts, emails or passwords: identity is a key pair the app makes.
- Anyone can run a server, and nothing has to depend on the official one, including identity (see Authorities).
- Uploads are free to remix, and remixes credit what they came from.
- Scripts stay safe to run: they're already sandboxed (no files, no OS); the worst one can do is use CPU (the watchdog stops it) or play notes.
- Small and simple to host: one binary, one SQLite file, a folder of files, behind a reverse proxy.

## Pieces

1. **The server**: `synththing serve`, the same binary as the app, headless. A JSON API over HTTP (`/api/v1/...`), SQLite for data, files on disk for sources and previews. HTTPS comes from a reverse proxy (Caddy on the official server).
2. **The admin command**: `synththing admin <command>`, talking to the running server over a local-only channel (see Moderation).
3. **The Library tab** in the app: browse and search the server, see previews, install, publish, update.

Stack: synchronous and small, so the desktop app doesn't carry an async runtime it doesn't need. `tiny_http` (a thread pool), `rusqlite` (bundled SQLite, FTS5 for search), `ed25519-dalek` (signatures), `lettre` (email, only used when configured), `ureq` (already used by the updater) on the client.

## Identity

- The app makes one Ed25519 key pair the first time it's needed, kept in the config folder. The public key is the identity, the same on every server.
- A user's ID is a short form of the public key (base32 of its hash, e.g. `k3f9q2xa`), shown as `#k3f9q2xa`. Browsing "everything by this person" goes by ID.
- Names are per upload and free-form. The server records which names each key has posted under, so a user page reads: `#k3f9q2xa`, posts as "ethan", "dissy".
- Signed: uploads, new versions, deletes, encores, reports, identity changes. A signature covers a canonical JSON of the request (the script's SHA-256, its metadata), a timestamp (rejected if more than 5 minutes off) and the server's own ID (its public key), so a signed request can't be replayed later or on another server.
- Export identity saves the key to a file, encrypted with a passphrase if one is given; Import loads it on another machine. Losing the key loses the identity (ownership, encores received), never the scripts themselves, unless it's recovered (below).

### Signing in another device

Every device signed in as someone holds the same key: signing one in copies it there (`library/pairing.rs`, `server/pairing.rs`, `app/pairing.rs`).

- A signed-in app exports the identity (unsealed, with its rotation records), seals it with ChaCha20-Poly1305 under a random 32-byte secret, and parks it on the authority (`POST /api/v1/pair`, signed). It shows `<server>/pair#<id>.<secret>` as a QR code and a link. The secret is after the `#`, which browsers never send, so the server only ever holds the sealed copy.
- The other device takes it (`POST /api/v1/pair/{id}/take`, unsigned, with its name from `device.json`), opens it with the secret and saves the key. A handoff can be taken once and waits 10 minutes; they're kept in memory only, at most 5 per key.
- The app that parked it asks whether it's been taken (`POST /api/v1/pair/{id}`, signed by the same key) and shows the device's name.
- Outside the app, a link lands on `GET /pair`: a page whose Open in synththing button hands the link (still with its `#`) to the Android app as `synththing:<link>`. With `android_fingerprints` in `server.json`, `/.well-known/assetlinks.json` lets Android open the official server's links in the app directly.
- The handoff and an exported identity also carry the sync key (`sync.key`, below), so every device signed in has it.
- A rotation (or a recovery) signs every other device out: servers refuse the old key once it's applied, with a 410 whose `replaced` names it, and an app holding that key forgets it and says it's signed out. The app warns about this before New key... and Recover....

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

An authority is a server that signs identity changes: the official server is one (`"authority": true`), and the app trusts its key, which is built in (`rotation::OFFICIAL_AUTHORITY`, also the key the app expects the server to have: `library::server_key`, which `SYNTHTHING_OFFICIAL_KEY` overrides for a test server).

- **Rotation** (`library/rotation.rs`, `server/authority.rs`): replacing a key with a new one. A rotation record says "key A is now key B": asked for with key A (the request signed by A, and B's signature showing B is held) or confirmed by email (recovery), and signed by the authority, which keeps every record (`GET /api/v1/identity/rotations?key=` gives the chain that led to a key). The new key has to be one the authority hasn't seen.
- **Applying it**: the authority moves everything from the old key to the new one as it issues the record: scripts, encores, reports, admin rights, and bans (a rotation is no way out of one). The old key is refused from then on. The app keeps the chain beside the key (`identity.rotations.json`, and in an export).
- **Precedence**: an email recovery replaces whatever the current key is, including one rotated in by a thief, and a stolen key can't take the email off or change it (that needs a code sent to the address). There's no notice email for a key-signed rotation: the authority keeps only a hash of the address, so it can't write to it unasked.
- **The publisher**: the key the official scripts are published with can be rotated like any other (`synththing publisher-rotate`). Each server follows its `publisher` through its rotations; the official server lists them in `/api/v1/info`, and the app checks them against the official authority and keeps them, so the gold name, updates of the bundled scripts and their links follow the new key (and an app released before the rotation learns of it the first time it talks to the server).

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

- An installed script's sidecar records what it is on the server (ID, version). Publish remix... (beside Update) publishes it as a remix of that version: the upload's `remix_of` names the original by ID (or, for a bundled script's copy, by author and slug) and the version by its SHA-256. An original that isn't on the server (deleted, or a bundled script that was never published there) is recorded as named, and the remix says it was remixed from a deleted script. A copy of a bundled script with no sidecar is recognized by its fingerprint and published as a remix of it.
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
- The app's script picker shows previews too: installed scripts keep the server's, and any other script gets one made by the app the first time it's looked at (cached by its source's SHA-256). Android can't make previews (no second process), and has only bundled scripts and ones from the Online Library, all on the official server: it asks the servers for the preview of the same source by its SHA-256 (`/api/v1/previews/{sha256}/...`).

## Encores and reports

- An **encore** is the only vote: one per key per script, signed, can be taken back, not for your own scripts. A user's total is the sum over their uploads.
- A **report** (signed) goes to the admins: a reason (malicious; defamatory or derogatory; intense flashing without a warning; someone else's work, uncredited; spam; other), optional details, and what it's about: the version, lines of its code, and sprites written in it (by the line they're registered on). One open report per key per script. There's no downvote.
- Limits per address: 60 encores an hour, 10 reports a day. Banned keys and addresses can't upload, give encores or report (they can still browse and download).
- Keys cost nothing to make, so encores can be faked with many keys, and a banned key can be swapped for a new one (a rotation carries its bans along; a fresh key doesn't, which is what address bans are for). A key ban can take its addresses along: those it was seen on in the last 30 days are banned for 30 days, and any new address the banned key comes back from is banned for 30 days too, so the ban follows it as long as it keeps coming back (new keys on a banned address are refused while that lasts, but aren't banned themselves, since addresses are shared). An authority with mail is stricter with keys without an email (fewer uploads a day). Every encore counts the same, whatever the key's age: making them count less would punish newcomers. Not perfect; fine at this scale.

## Feedback

- **Feedback** (`server/feedback.rs`, Help > Send feedback... and the speech bubble in the app's menu bar) is a note about the app for the admins: unsigned, so it needs no identity, with the build it came from (version, channel, OS) and, if the sender ticks them, the end of the app's log and how long its frames took over the last minute (`performance`, on unless they untick it). The app sends it to the official server. Branch builds can send it although their other online features are off: trying a feature and saying how it went is what they're for.
- Limits per address: 10 a day. Banned addresses can't send it. The admins get a `new_feedback` notification.
- Read it in Moderation's Feedback tab or with `synththing admin feedback`, and mark it done (with a note, like the issue it became) with `synththing admin feedback-done <number>`.
- **Intake** (`server/intake.rs`, `"intake": true` in `server.json`, docs/DEPLOY.md): an hourly Claude run collects the feedback waiting (`GET /api/v1/intake`, up to 20, oldest first: each note, its build, its performance stats and the last 8 KB of its log, not the address), files it as GitHub issues (docs/feedback-intake.md), and marks it taken (`POST /api/v1/intake/taken`). Both need the intake token (`intake.token` in the data folder, made on first run). Until a note is taken, the next run gets it again; one run takes a whole batch, so notes about the same thing are filed together.
- Kept until it's done, then 90 days; addresses 30 days, as for reports. A branch build's feedback is deleted once that branch's test build is gone from GitHub's releases (`branch-cleanup.yml` removes it when the branch is deleted), checked daily; if GitHub can't be reached, nothing is deleted.
- **Usage data** (`server/usage.rs`, docs/USAGE.md): anonymous counts of log lines and panics from apps that opted in, added up by version, platform, level, target and signature, kept until an admin clears them. The server's rules say which lines it keeps.

## Moderation

- **Admins** are keys listed by the server (added with `synththing admin add-admin <id>`), and the server's own key.
- One endpoint, `POST /api/v1/admin`, signed by an admin, takes an action (`AdminAction`): reports, a script's extra metadata (uploader key and addresses, versions, its reports) and source (hidden or not), hide, unhide, delete, ban (a key or an address, for a while or for good, optionally hiding everything the key uploaded), unban, bans, add-admin, remove-admin, admins, resolve, feedback, feedback-done, usage, usage-clear, usage-rules.
- **`synththing admin <command>`** on the server sends those, signed with the server's own key from its data folder, to the running server at its configured address: it works from SSH, under systemd, and in Docker (`docker exec <container> synththing admin ...`), and `update` asks it to update itself (Hosting, below).
- **Moderation** in the app's Library tab, for admin keys: reports with the lines they point at highlighted in the code and the sprites drawn, and the actions above, signed with the admin's key.
- **Web admin panel**: parked. Moderation in the app does the job and stays in step with it; if the app ever builds for the web (egui runs in a browser, see TODO), that's the web version.
- **Rules** and a contact address in `/api/v1/info` (contact@p51.nl on the official server), for reports and takedown requests.
- **Documents**: a server can offer documents (`documents` in `server.json`: a title, a Phosphor icon, a Markdown file), like its EULA and privacy policy; `/api/v1/info` lists them, `GET /api/v1/documents/{slug}` serves them, and the app's Online Library has an icon button for each (hover: "Read EULA") that opens it, rendered (`egui_commonmark`). The official server's are in `deploy/official/`.

## Deletion and retention

- **Deleting a script**: by its owner (signed) or an admin.
- **Deleting an identity's data** (`server/deletion.rs`, Delete my data... in Preferences > Account): asked for with the key, on each server separately; where the identity has an email attached on a server with mail, confirmed with a code sent to it too (with a "that wasn't me" link). Nothing goes at once: its scripts are hidden (deleted by their author: out of every listing, search and download), the encores it gave set aside, its admin rights taken away, and the key refused for anything but restoring, for 30 days, during which Restore my data puts it all back. Then a daily job deletes it for good: its scripts (remixes of them then say "remixed from a deleted script") and their previews, the encores, the email hash, and its key on the reports it made. Bans stay: deleting is no way out of one. (Lost the key? Recover it by email first.)
- **IP addresses** are kept only for rate limits and bans, and deleted after 30 days (bans keep theirs until they end).

## Songs (sunset)

The library took MIDI songs for a while (`"songs": true`); it doesn't any more: songs are to sync between a user's own devices instead. Songs a server already stored stay in its database (`kind = 'song'` rows and the `songs` table, schema version 12), but nothing lists, serves or takes them. Their code is in git history, up to the commit before they were removed.

## Sync

Devices signed in as the same identity keep their settings, playlists and songs the same (`library/sync.rs`, `server/sync.rs`, `server/relay.rs`, `settings_sync.rs`, `playlist_sync.rs`, `song_sync.rs`, `device_sync.rs` for the syncing itself, `app/sync.rs` for the app's side). On with `"sync": true` in `server.json` (off by default; on for the official server); apps sync with the authority.

- **Encrypted on the device**: every synced thing is a named blob (`settings`, `playlists`), sealed with ChaCha20-Poly1305 under the identity's sync key: 32 random bytes made by the first device that needs them, kept in `sync.key` beside `identity.key`, and carried by sign-in handoffs and identity exports (a file from before sync has none). A rotation doesn't change it. The server stores the sealed blob and its revision per key (a `sync` table), never readable copies. A recovery by email leaves the old blobs unreadable (the sync key was on the lost devices): sync starts over from what the devices have.
- **Revisions**: `POST /api/v1/sync/{name}` (signed) writes a blob with the revision it was made from, and is refused (`"stored": false`, with the server's revision) if the server has a newer one; `POST /api/v1/sync/{name}/get` (signed) reads it. Each write is pushed on the notification stream as `{"sync": {"name": ..., "revision": ...}}` (unstored; older apps skip it). At most 16 blobs per key, 1 MB each. A rotation moves them to the new key; deleting the identity's data deletes them with the rest.
- **Live messages** (`device_messages.rs`, `panel_share.rs`): what one device tells the others while they're online, such as the panels programs share. `POST /api/v1/live` (signed, `{"live": "<sealed>"}`, at most 192 KB) passes it to the signer's open notification streams as `{"live": ...}` and answers how many it reached; nothing is kept. Inside the seal (the sync key, deflated): which device it's from, which it's for (or every one), a topic and its body, so the server sees neither. Each device drops its own and those for another. A hub's direct link carries them first while a device reaches it, with the server for when it doesn't.
- **Checking a hub's address** (`server/reach.rs`, `library/reach.rs`): `POST /api/v1/reach` (signed, `{"url": ..., "hub_key": ...}`) fetches `<url>/hub/v1/hello` from the server, since a hub can't test its own public address from inside its network. The server resolves the name itself and tries the first IPv4 and IPv6 address, each only if it's public (not private, shared/CGNAT, loopback, link-local, multicast or reserved) and connecting to that very address, with no redirects, 5 s to connect, 10 s in all and 4 KB read. It answers per address what it found (`ok` with the time, `private`, `refused`, `timed_out`, `tls`, `status`, `not_a_hub`, `other_hub`, `failed`), or why the name didn't resolve, and the address the request came from. Six checks an hour per identity, 30 a day, and 10 an hour to one host, so it can't be used to scan anyone. Once an address passes, the server remembers it (a `hubs` table: the key, the address, the hub's key, when it was last checked and last asked about, failures in a row) and checks it again itself whenever no one has for 6 hours, so it notices a hub that's down and can't ask. After 3 failures in a row it notifies the identity (`hub_unreachable`, with the address and what to fix), and once one passes again (`hub_reachable`). A passing check for another hub forgets the identity's other addresses, and an address no one has asked about for 30 days is forgotten. A rotation moves them to the new key; deleting the identity's data deletes them.
- **Settings** (`settings_sync.rs`): `config.json` flattened to its leaves (`theme`, `custom_themes/<name>/palette/accent`, `library/author_name`, ...), less those that are one device's own (folders, layouts, the audio buffer, the open soundfont and script, updates, the mobile layout, the UI scale). Each leaf carries when it last changed: `Config::save` stamps the leaves that changed since the last save, so no setting needs to know about sync. `sync.json` holds the device's leaves, the server's as of the last sync (the base) and that revision; it's for one sync key, so signing out or in as someone else turns sync off.
- **Playlists** (`playlist_sync.rs`): a leaf per playlist, by its ID (random, the playlist's file name; its name is a field, so renaming on one device renames it everywhere): its name and its songs, each by its key in the song library with its name. Merged like settings, so a playlist changed on two devices takes the newer change. Which soundfont an entry uses stays on its device. A song a device doesn't have becomes known to its library by name (`SongStore::know`) and shows as unavailable until it's there. Its own state, `sync-playlists.json` (the settings' is `sync-settings.json`); Playlists, under Songs in What this device syncs, turns it on or off on a device.
- **Songs** (`song_sync.rs`): two more blobs. `library` has a leaf per song in any device's library (its key: name), only ever added to, so every device knows every song by name. `transfers` says which songs each device wants (`want/<device>/<key>`, the device by `device.json`'s random ID), which are waiting on the relay (`ready/<key>`), and the devices themselves (`device/<device>`: its name, and when it last synced, noted at most once a day). Your devices, under the switch, lists them; removing one there drops it and its wants, and a device that hasn't synced for 14 days is dropped by whichever device notices first (one dropped comes back if it syncs again). A device wants the songs it lacks in its playlists or, with Every song, in the whole library, less those it removed: removing a song deletes it on that device only and blocks it there (the song store's `blocked.json`; Songs removed from this device, under the switch, fetches one again; importing it again does too). Every device listens for songs streamed to it while it's open (a thread of its own, by its inbox ID: a hash of the sync key and its device ID). A device that has a song another wants seals it (in 64 KB chunks, so neither end holds a whole song) and, if that device is listening, streams it straight there through the server, which keeps none of it, of any size; the device remembers it did, so as not to send it again before the other says it has it. If no device that wants it is listening, it sends it to the relay instead and marks it ready; the device that wanted it fetches it; once no device wants it, whoever sees that first deletes it from the relay. Either way the device that wanted it checks it hashes to its key, and stops wanting it. Songs move one at a time, on a thread of their own; a failure pauses them for a minute. Before leaving songs on the relay, a device asks how much room it has left for the identity, and sends only what fits: the rest wait (a minute, then it asks again) for the other devices to fetch what's there, so a fast sender never gets more than the relay's room ahead of a slow receiver, and a song too big for the relay waits until the device that wants it is open. Songs from my other devices and Every song, under Songs in What this device syncs, choose per device (on, and off).
- **The relay** (`server/relay.rs`): `POST /api/v1/relay/inbox/{inbox}` (signed) listens for a song for up to 40 seconds (204 if none came; one comes with its ID in `X-Synththing-Relay-Id`), and `POST /api/v1/relay/{id}/send/{inbox}` (signed, the sealed song as the body) streams one straight to it, waiting up to 30 seconds for it to be ready for it (404 if it isn't listening); `POST /api/v1/relay/listening` (signed, `{"inboxes": [...]}`) says which are, counting one that listened in the last 15 seconds. Each waits on a thread of its own, at most 256 at once (8 listening per key). `POST /api/v1/relay/{id}` (signed, but not its body: a song is sealed, so it can't be changed unseen; the sealed file as the body) leaves a file, `.../get` fetches it and `.../delete` deletes it, each only for the key that left it. The ID is a hash of the sync key and the song's key, so the server can't tell which song a file is, nor that two identities have the same one. Files are kept in the data folder's `relay` (a `relay` table names them) for at most 7 days, up to `relay_song_mb` each (250) and `relay_mb_per_identity` in all (2048; `POST /api/v1/relay/room` says how much is left, and devices send only what fits; past it anyway, the oldest go, and a device that wanted one asks for it again). A rotation moves them to the new key; deleting the identity's data deletes them.
- **Merging**: per leaf, against the base: a leaf only one side changed takes that side's value; one both changed (a device offline, say) takes the newer change. The merged leaves are applied together when the config reads with all of them (a new custom theme is many leaves, none of which reads alone), else one by one. Leaves an app doesn't have, or can't read (a newer app's), are carried along untouched, never applied or overwritten. The first device to turn sync on uploads its settings; one turning it on later takes the server's where it has them.
- **What each device syncs**: `CATEGORIES` in `settings_sync.rs` groups the synced settings (Appearance, Playback, ...), each naming its leaves and whether it syncs by default (last volume and speed don't). Each device chooses for itself, under What this device syncs: a category's checkbox covers all of it, and opening it shows one per setting. A setting a device doesn't sync is neither sent nor applied there: its merge carries the server's value along untouched. Turning one back on takes the server's value. Leaves no category names (a newer app's) go with Anything else.
- **In the app**: Preferences > Account > Sync settings and playlists between my devices. It syncs at launch, two seconds after a synced setting, a playlist or the songs here change, and when the stream says another device wrote (the stream stays open for sync even with notifications off). Show what each sync changed (under it) shows a toast listing the settings a sync changed, and which side won a conflict.

## Notifications

For what happens to your things on a server: encore and download milestones, an item hidden, deleted or reinstated, a remix of your script, a report you made handled, a copyright notice or a dispute's outcome, and for moderators, a new report or notice in the queue.

- **On the server** (`server/notifications.rs`): a `notifications` table: the recipient's key, a kind, a time, read and handled flags, and the kind's fields (`{"script": "...", "name": "Disco", "count": 100}`), never wording. Milestones are noticed as they're crossed (encores and downloads: 10, 25, 50, 100, 250, 500, 1000, ...; downloads of a script's source are counted). Read ones that needed nothing done are forgotten after 90 days.
- **Kinds so far** (and their fields): `encore_milestone` and `download_milestone` (script, name, count); `hidden` (script, name, reason), `reinstated` and `removed` (script, name), for an item's author; `remixed` (script, name, remix, remix_name, by), for the original's author; `report_handled` (report, script, name, outcome), for its reporter; `new_report` (report, script, name, reason), for every moderator.
- **Wording is the app's**: each type has a template in the app (`"You've had {count} encores on \"{name}\"!"`), filled in from the fields, so wording changes without touching servers. The server also sends a template of its own with each notification, for an app that doesn't know the type (a newer server): filled in the same way, or, if even that fails, shown as it is, braces and all, rather than not at all.
- **Actions** (Dispute..., Show it, the moderators' response to a report) are the app's too, per type, from the fields; the fields of a type only ever gain more, never change meaning, so older apps keep working.
- **Delivery**: `POST /api/v1/notifications` (signed) lists the newest 200 (after one, optionally), and `/api/v1/notifications/mark` marks some or all read (or handled); `GET /api/v1/notifications/stream` (signed) is a Server-Sent Events stream that pushes new ones while it's open, written straight to the connection on a thread of its own (a keep-alive comment every 25 seconds), at most 256 open at once and 4 per key. The app keeps one open with each server in use while there's an identity, starting over half a minute after it drops (fetching the list again first): the Notifications preference (Preferences > Account, on by default) turns that off, and the privacy policy says it happens.
- **In the app**: an envelope at the right of the header, red with a count when there's something unread; it opens a list, newest first, merged across servers by time; an entry opens a window with all of it and its actions. Read ones leave the count; ones that need something done (a copyright notice to dispute or accept) stay in the list until they're handled.

## Copyright notices

Formal notices from a rights holder (or someone acting for one), as the US DMCA has them, for any script. Separate from reports: a report goes to the moderators; a notice takes the item down at once. (`server/copyright.rs`, `app/copyright.rs`): `POST /api/v1/notices` files one, `/api/v1/notices/{id}` shows it to its uploader (without the claimant's contact details) or claimant, `/dispute` and `/accept` are the uploader's; moderators list and decide them with `AdminAction::Notices` and `AdminAction::Copyright` (the Moderation window's Copyright tab). Notifications carry each step, and the uploader's says what they did (`resolution`) once they've done it.

- **A notice** (signed, from the app: Report... > It's my copyrighted work) needs everything a DMCA notice does: the claimant's full name, who they act for, their postal address, email and phone, the copyrighted work and where it's from, a statement that they believe in good faith the use isn't authorized, a statement under penalty of perjury that the notice is accurate and they're authorized to act, and a signature (their full name, typed). Asking for all of that, and saying it's a legal statement, is what keeps it from being abused.
- **The item is hidden at once** (`hidden = 3`, under a copyright notice), and its uploader gets a notification of it (from whom, for whom, the work named; not the claimant's address or phone), with **Dispute...**; it stays in their list until they dispute it or let it stand.
- **Unless it's locked**: an item whose dispute was upheld before can't be taken down by another notice without a moderator: the notice goes to the moderators' queue instead, and the item stays up. That's what stops one bogus claimant (or several) from keeping something down.
- **A dispute** (a counter-notice: the uploader's name, address, a statement under penalty of perjury that it was taken down by mistake or misidentification, consent to the jurisdiction, a signature) goes to the moderators with the notice. They **uphold** it (the item comes back, locked against further notices) or **reject** it (it stays down). With no dispute in 14 days, the takedown stands.
- **Taken down for good**: the script's fingerprint goes on a blocklist, so the same code can't simply be uploaded again ("taken down after a copyright notice").
- **Bogus notices**: moderators can mark a notice bogus (the item comes back, and the claimant's key or address can be banned). Notices are limited per key and address.
- **Repeat infringers**: a key with three takedowns that stood is banned (DMCA asks for a policy like this).
- **The official server** registers a DMCA agent with the US Copyright Office (contact@p51.nl), and the EULA has a Copyright section: how to send a notice (in the app, or by email), what a dispute needs, and the repeat-infringer policy.

## API (v1)

```
GET  /api/v1/info                         name, version, server key, mode, license, rules, contact, authority?
GET  /api/v1/scripts?q=&category=&tag=&sort=new|top&page=
GET  /api/v1/scripts/{id}                 details, versions, author ID and names, encores (?viewer=key: whether they gave one), remix of, remixes
GET  /api/v1/scripts/{id}/source          ?version=  (signed by the server)
GET  /api/v1/scripts/{id}/preview.png     the still
GET  /api/v1/scripts/{id}/preview-sheet.png
GET  /api/v1/previews/{sha256}/preview.png  the still of the version whose source has that SHA-256
GET  /api/v1/previews/{sha256}/preview-sheet.png
POST /api/v1/scripts                      upload (signed with a slug its author used before: a new version of that script)
DELETE /api/v1/scripts/{id}               owner or admin
POST /api/v1/scripts/{id}/encore          (and DELETE to take it back)
POST /api/v1/scripts/{id}/report
POST /api/v1/feedback                     feedback for the admins (unsigned)
GET  /api/v1/intake                       feedback waiting to be filed (the intake token)
POST /api/v1/usage                        an anonymous usage report (docs/USAGE.md)
GET  /api/v1/usage/rules                  which log lines usage reports should carry
GET  /api/v1/intake/usage                 usage data new since intake last took it (the intake token)
POST /api/v1/intake/usage/taken           intake took it, and which issues lines were filed as
POST /api/v1/intake/taken                 mark it filed (the intake token)
POST /api/v1/admin                        an admin action (signed by an admin)
GET  /api/v1/users/{id}                   names, uploads, total encores
GET  /api/v1/users/{id}/scripts/{slug}    a script by its author and slug
GET  /api/v1/documents/{slug}             one of the server's documents (EULA, privacy policy)
POST /api/v1/notices                     signed: a copyright notice (takes the item down at once)
POST /api/v1/notices/{id}                signed: the notice, for its uploader or claimant
POST /api/v1/notices/{id}/dispute        signed: the uploader's counter-notice
POST /api/v1/notices/{id}/accept         signed: the uploader lets it stand
POST /api/v1/notifications               signed: your notifications, newest first
POST /api/v1/notifications/mark          signed: mark some (or all) read, or handled
GET  /api/v1/notifications/stream         signed: new notifications as they come (Server-Sent Events)
POST /api/v1/identity/rotate              a key-signed rotation (authorities)
GET  /api/v1/identity/rotations?key=      the rotations that led to a key (authorities)
POST /api/v1/identity/status              signed: whether an email's attached (authorities)
POST /api/v1/identity/email               signed: codes to attach, change or take off an email (authorities)
POST /api/v1/identity/email/confirm       signed: the codes (authorities)
POST /api/v1/identity/recover             start a recovery: a code to the email (authorities)
POST /api/v1/identity/recover/confirm     the code: the rotation to the new key (authorities)
GET  /api/v1/identity/cancel?token=       a code email's "that wasn't me" link (authorities)
POST /api/v1/pair                         signed: park a sealed sign-in handoff
POST /api/v1/pair/{id}                    signed by the key that parked it: whether it's been taken, and by which device
POST /api/v1/pair/{id}/take               the handoff, once (and the taking device's name)
GET  /pair                                where a sign-in link lands outside the app
GET  /.well-known/assetlinks.json         lets Android open sign-in links in the app (with android_fingerprints)
POST /api/v1/sync/{name}                  signed: write synced data made from a revision (servers with "sync": true)
POST /api/v1/sync/{name}/get              signed: read synced data and its revision
POST /api/v1/live                         signed: pass a sealed message to the signer's open notification streams
POST /api/v1/reach                        signed: whether a hub answers at an address from the internet
POST /api/v1/relay/inbox/{inbox}          signed: listen for a song streamed to this device (servers with "sync": true)
POST /api/v1/relay/listening              signed: which of these inboxes are listening
POST /api/v1/relay/{id}/send/{inbox}      signed: stream a sealed song straight to a device listening
POST /api/v1/relay/room                   signed: how much more of the key's songs the relay holds now (bytes)
POST /api/v1/relay/{id}                   signed: leave a sealed song for another device (servers with "sync": true)
POST /api/v1/relay/{id}/get               signed: fetch a sealed song left by one of the key's devices
POST /api/v1/relay/{id}/delete            signed: delete it, once it's been taken
POST /api/v1/identity/delete              signed: delete the key's data here (in 30 days), or send a code first
POST /api/v1/identity/delete/confirm      signed: the code
POST /api/v1/identity/restore             signed: undo a deletion
```

Errors are JSON (`{ "error": "...", "retry_after": ... }`). Requests are signed with headers (`X-Synththing-Key`, `X-Synththing-Time`, `X-Synththing-Nonce`, `X-Synththing-Signature`); the signature covers the server's key, the method and path, the time, the nonce (random per request, so a replay is spotted) and the body's SHA-256.

## In the app

- **Library tab**: browse and search the server, by category and tag, sorted by newest or most encores; a script's page shows its animated preview, description, author, versions, remixes and encores; Install, Encore, Report.
- For scripts, the app only contacts a server while the Library is open, when publishing, or when asked to check installed scripts for updates. With an identity, the notifications stream (on unless turned off) and sync (when it's on) talk to it on their own, from launch.
- **Installing** saves the `.lua` to the scripts folder with a sidecar (`<script>.lua.source.json`: ID, version, the installed source's hash) and caches its preview, description and category.
- **Updating** (on request, "Check for updates"): if the script was changed since it was installed, the app says so and shows a diff before overwriting. **Restore original** puts back the installed version, like Restore default for bundled scripts, and only shows when there's something to restore.
- **Publishing** ("Publish..." by the script picker): name, description, category, tags, the name to post under, which server; the license; the minimum app version it worked out. If the user's settings differ from the script's defaults, it offers to add them as a preset (below).
- **The script picker** has a sidebar: the preview of the script under the mouse (or the selected one), its description (its leading comment block), author and category.

### Presets

- `settings_preset(name, { key = value, ... })` in a script: a named set of setting values, picked in the Settings window. The defaults in the `setting_*` calls stay the only defaults.
- Users can save their own presets (Settings window, Save as preset...), kept beside the script (`<script>.lua.presets.json`) with no change to its code.
- Publishing with settings that differ from the defaults offers to add them as a preset in the script: one `settings_preset(...)` line near the top.

## Hosting

- **The official server**: the Linux release binary on a Hetzner VPS (Ubuntu 24.04, glibc 2.39; the binary is built against 2.35), as a systemd service, data in `/var/lib/synththing`, behind Caddy, which also serves the website and these docs (docs/DEPLOY.md, The official server).

- **Docker**: an image with the binary, the data folder as a volume, and the admin command through `docker exec`.
- **Updating** (`server/update.rs`): the Moderation window's Server tab (and `synththing admin update --check`) says which version's running and which is the newest release. The server knows how it's run (Docker, systemd or by hand): under Docker it says to pull the new image; run by hand, an admin's Update downloads and checks the new binary (the app's updater code), swaps it in, starts the new version and exits; under systemd, where the service can't write its own binary, it leaves `update.request` in its data folder, and `synththing-update.path` starts the root-run update script. Database migrations run forward-only at startup.

## How it was built

In this order:


1. **Basics**: `serve` with open uploads, listing, search, download, server keys; the Library tab (browse, install with sidecar), Servers in Preferences.
2. **Identity**: keys, signed requests, ownership (versions, delete), user pages, export/import, receipts.
3. **Previews**: the queue, frame scoring, sprite sheets; the picker's sidebar.
4. **Encores, reports and moderation**: `synththing admin`, the admin tab, bans, rate limits.
5. **Publishing extras**: remixes, copy detection, minimum app version (`HOST_API` versions), presets.
6. **Updates and diffs** for installed scripts.
7. **Authorities**: rotation records, applying them, email attach and recovery.
8. **Deployment**: the official server on the VPS, Docker image, in-place updates.
9. **Notifications**: the table, milestones (and download counts), templates and actions in the app, the list and stream endpoints, the header's envelope.
10. **Copyright notices** (for scripts first): notices that take an item down at once, the uploader's notification and disputes, the moderators' queue, locks, the blocklist, the repeat-infringer ban; the EULA's Copyright section.
11. **Songs** (sunset): hosted for a while, then taken out in favour of syncing songs between a user's own devices.
12. Later, maybe: the app itself on the web (WebAssembly), for browsing and moderating without installing it.
