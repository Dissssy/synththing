# Changelog

What changed in each version of synththing, newest first. The app shows the new parts after an update (and all of it under Help > Changelog...).

## Unreleased

### Added
- See what your other devices are playing: with sync on, a Devices button by the transport (under the visualizer picker on a phone) shows up while another of your devices is online, listing each one with its song, artist and where it is.

## 1.7.3 (2026-10-27)

### Added
- A hub hosts programs' panels for your devices: let one in with `synththing hub token <name> --grant read,panel,share` and its panel shows in the Apps tab on your other devices, as one shared from a PC does.

## 1.7.2 (2026-10-27)

### Fixed
- A wishlist that can't be read (saved by a different build, say) is set aside as wishlist.json.unreadable-<time> and the app says so, keeping the wishes that can be read, instead of starting empty and being saved over.

## 1.7.1 (2026-10-27)

### Added
- A hub answers the local API: let a program in with `synththing hub token <name> --grant read,add_songs`, and it can read the library and the wishlist, add songs and check them first, as with the app (playing aside: a hub plays nothing). `SYNTHTHING_API_LAN=true` lets in other Docker containers. See docs/HUB.md.

## 1.7.0 (2026-10-27)

### Added
- Panels on your other devices: a program you also allow to Show on my other devices (Preferences > Integrations) has its panel in the Apps tab of every device you've signed in with sync on, as "Program · device". What you do there reaches the program like on its own device, saying which device it was. It travels sealed with your sync key through the server, which keeps none of it.
- Panels can hold lists: rows with a subtitle and up to two buttons, and rows that stand for a song (with its art: click to play, right-click to queue) or a wish. Programs change one element without resending the panel (`PATCH /v1/panel/elements/{id}`) and hear when their panel comes into view or leaves it. In the mobile layout, Apps is in the menu while a program has a panel.
- Programs can show a panel of their own: with the new Show a panel permission (Preferences > Integrations), a program sends `PUT /v1/panel` with labels, textboxes, buttons and rows, and it shows in the new Apps tab (View > Apps), under a chip with the program's name. What the user types and clicks reaches the program as `panel` events on its event stream, and the panel greys out while the program isn't running. See docs/API.md.
- Run a hub: `synththing hub` is one of your devices with no window that keeps every song, for an always-on machine (a home server, a NAS, a VPS, or Docker with deploy/hub.compose.yml). Sign it in with `synththing hub pair '<link>'` and your devices' songs stream to it, and from it to each device once that's open. See docs/HUB.md.
- The local API tells programs only what changed: `GET /v1/songs?since=<revision>` gives the songs that came or changed since an earlier answer, and the keys of those that left. See docs/API.md.
- Programs can check songs before adding them: `/v1/staged` takes a file and reads it without putting it in the library, `/v1/staged/check` matches what's staged at MusicBrainz (and says which wishes it would fill), and `/v1/staged/{key}/import` adds it as checked without sending it again. See docs/API.md.
- The local API has the wishlist: `GET`, `POST` and `DELETE /v1/wishlist` (an artist comes with the albums of theirs wished for), notes, `GET /v1/musicbrainz/search` and an artist's albums and a release's track list at MusicBrainz to wish from, and a `wishlist_changed` event. See docs/API.md.
- Filter the Wishlist: its box takes kind:album, got:no (partly, yes), type:ep, artist:, title:, note:, addedby:, year:2010..2019 and added:30d, with completion, and words found in a wish's title, artist or note. In the Songs search and smart playlists, is:wished finds the songs filling a wish and wish:"Album name" those filling that one (is:wished added:30d is what came in from your wishlist lately).
- Wish for an artist's albums: search MusicBrainz for the artist in the Wishlist, right-click them in By Artist, or use the gift on their page or in Song info. Click them in the Wishlist to see their albums at MusicBrainz (Albums and EPs, or tick other kinds to show) and tick the ones you want: those go on the wishlist under the artist, and the artist's got once they are. Open an album there (or click a wished album) to untick the songs you don't want, or tick just the ones you do. The kinds shown are chips. Click an artist or album in the search results to open its page before wishing for it, and songs you already have are marked Here. An artist's page in By Artist lists their albums you don't have, faint, under Not in your library. Artists not matched at MusicBrainz get a wish for anything by their name.
- Wish for the rest of an album from the library: right-click a song (or an album in By Album), or use the gift on an album's page or in Song info, on an album matched at MusicBrainz that isn't all here (its tracks are asked of MusicBrainz first if this device hasn't kept them). By Album shows the albums on your wishlist you have none of yet, dimmed with a gift (the gift button hides them); click one to see it in the wishlist. An album's page lists the songs of its MusicBrainz release you don't have, faint, in their places.
- A wishlist (View > Wishlist, or Wishlist in the phone menu): search MusicBrainz for albums and songs you want and add them, or wish for something by name. Each wish shows as Wanted, Partly got ("7 of 12") or Got it as matching songs reach your library, with a note when one comes true; click one to play what's here. It syncs between your devices with the rest (Preferences > Online Library > Wishlist).
- Write scripts in your favorite editor: the scripts folder now has the script API's definitions for the Lua language server, so VS Code's Lua extension (or Neovim, Zed, JetBrains) gives completion, hover help and type checks for every synththing function. Open the folder and go.
- The Scripting Reference lists every function with the version it came in, and has a new Tables section with every table scripts get and give, field by field.
- Visualizer pixel scale (Preferences > Visualizers): visualizers now draw at your screen's full resolution (4x by default on phones), or at 2x, 4x and up for chunkier pixels and faster drawing. It replaces the old cap of about 1280x720 pixels and stays on each device.

### Changed
- Groupings' list view and the Wishlist's MusicBrainz results are drawn like the other lists: striped rows, the same hover and selection, and Ctrl/Shift+click to select.
- The app talks to one server, synththing.p51.nl: the list of library servers in Preferences > Online Library is gone, along with remix links to scripts on other servers. Its key is built in, so signing in another device, sync, notifications and the rest work from launch without opening the Online Library first. Testing against a server of your own works with the `SYNTHTHING_OFFICIAL_URL` and `SYNTHTHING_OFFICIAL_KEY` environment variables.

### Fixed
- Scripts using `blit`, `read_asset`, `list_assets`, `midi_send`, `sound_register`, `sound_play` or the `assets` option are no longer offered to 1.5.0, which doesn't have them (they came in 1.6.0).

## 1.6.0 (2026-10-23)

### Added
- Doom, a new game visualizer (New > Games): plays the first maps of a Doom WAD you put in a `doom.lua.assets` folder next to it (shareware DOOM1.WAD or Freedoom), with the map's music on your soundfont, its sound effects, and fights with zombies, imps and demons.
- Scripts can keep the cursor in the window for mouse look in any view, not just the dedicated fullscreen, and `mouse_delta()` is raw mouse motion while the visualizer has focus, so turning doesn't stop at the edge.
- SF3 soundfonts (compressed samples, like GZDoom's) are refused with a tip to convert them, instead of playing as loud noise.
- Scripts can draw a whole palette image in one call (`blit`), read and list data files in a `<script>.lua.assets` folder next to them (`read_asset`, `list_assets`, and `script_options({ assets = true })` for a button that opens it), send raw MIDI messages to their synthesizer (`midi_send`), and play sound effects from samples (`sound_register`, `sound_play`).

## 1.5.0 (2026-10-23)

### Added
- The local API shows Shuffle, Loop and Autoplay with what's playing, and can set them (`POST /v1/modes`).
- The local API lists your soundfonts and can switch which one MIDI songs play with (`GET /v1/soundfonts`, `POST /v1/soundfonts/{id}/use`). What's playing tells which soundfont a MIDI song plays with, and a `soundfonts_changed` event tells when the list or the one in use changes.
- The local API can reorder the play queue (`POST /v1/queue/move`).
- Programs allowed to Edit library can make, rename and delete playlists and reorder their songs through the local API.
- Song downloads from the local API answer `Range` requests, so a player can stream a song and seek in it.
- The local API streams the visualizer live as MJPEG (`/v1/visualizer/stream`, or a page for OBS's Browser Source at `/v1/visualizer`), drawn at the size the program asks for, and gives single pictures (`/v1/visualizer/frame.jpg`). Scripts see `playback().streaming`. It keeps drawing while watched with its tab hidden or the window minimized, and OBS's Browser Source no longer shows an old copy of the page.
- Now playing, a new visualizer for stream overlays: a pixel-art card with the song's title, artist, time and album art, in the album's colors, see-through around it. In OBS, add a Browser Source with the local API's visualizer page and `&format=png`.
- Script text draws Japanese, Korean and Chinese with the built-in Noto fonts, instead of `?`.
- Script color settings can let you pick their opacity too (`alpha = true`); Now playing's card background does.
- Scripts can read the playing song's title, artist and album (`playback()`), and draw its album art (`album_art()`, which also gives the color it's mostly made of).
- FFT, Pulse, Keyboard, Letters and Dashboard have a Background opacity setting: below 1 the background is see-through, for OBS.
- The See-through button above the visualizer shows see-through visualizers' transparency in the app; off (the default), they're shown over black, so they stay readable on every theme.
- Visualizers can have see-through frames: `script_options({ transparent = true })`. Each frame starts transparent, `clear()` keeps its alpha, and the clear color's alpha fades the last frame out. The local API can send the visualizer as PNG pictures (`?format=png` on the stream, `/v1/visualizer/frame.png`), which keep them see-through, for OBS's Browser Source; recordings and JPEG frames show them over black.
- The local API lists the visualizer scripts and can switch which one draws (`GET /v1/scripts`, `POST /v1/scripts/{id}/use`), with a `scripts_changed` event.
- The local API gives songs' album art and your Groupings (each ruleset's playlists), and can group any search by artist, album, genre, year or tag.
- The local API can seek to an exact point and set the volume (`POST /v1/seek`, `POST /v1/volume`), and an event stream can ask for only the events it wants.
- Programs you allow to Edit library (a new permission, off until you tick it) can star songs, tag them, and add songs to playlists or take them out through the local API. A new `playlists_changed` event tells when any playlist changes.
- The local API can see and change the play queue (`/v1/queue`), and a `queue_changed` event tells when it changes.
- Programs you allow to Download songs (a new permission, off until you tick it) can copy songs' files out of your library through the local API.
- A play queue: right-click songs (or an album, artist or playlist) for Play next or Add to queue, and the Up next button by Shuffle opens the Up next tab (View > Up next, or G then U): the queue with the Songs list's columns, where a click plays a song now, songs drag in from Songs and Groupings, rows drag to reorder, and right-click moves songs to the top or takes them out. Queued songs play after the one playing, then the playlist or list it was playing from carries on. A playlist's or album's songs go in in order, or shuffled while Shuffle is on.

### Changed
- Preferences > Integrations shows programs as a table: one row per program, a column per permission (hover an icon for what it allows).
- Audio files (MP3, FLAC, OGG and the rest) are always songs now, alongside MIDI: the Show audio files setting is gone.
- A single click plays a song again (and uses a soundfont, or plays a playlist's song), leaving the selection as it was; Ctrl+click and Shift+click select rows without playing them. While rows are selected, a line under the list says how many, with a button to clear them; Esc clears them too.

## 1.4.0 (2026-10-23)

### Added
- A local API for your own integrations (Preferences > Integrations, desktop only, off by default): programs on this computer can see what's playing, search your songs and playlists, follow events (song changed, paused, seeked, library changed) and run any command the keyboard can, including new Play and Pause commands. Each program gets its own token, allowed to read, control or both. See docs/API.md.
- synththing shows in your system's media controls: media keys, the volume overlay on Windows, Now Playing on macOS and MPRIS on Linux (so desktop widgets, KDE Connect and Home Assistant's MPRIS support see it too). Play, pause, skip, seek and volume work from there. On by default; Preferences > Integrations turns it off.
- Programs can add songs to your library through the local API, only when you tick Add songs for them. Each song one adds is marked with its name on all your devices: search `addedby:name` to find them, and Preferences > Integrations can remove everything a program added in one go.
- The local API can play a song or playlist (`POST /v1/play`), and can be let in from the local network as well as this computer (off by default; it's unencrypted, so only on a network you trust). While it's off, requests through a proxy or from web pages are turned away.

## 1.3.1 (2026-10-21)

### Added
- Search filters in Songs: type key:value terms beside (or instead of) your search, like `type:mid`, `artist:bonno`, `year:1990..1999`, `length:>3:00`, `added:30d`, `tag:chill`, `is:starred` or `has:album`, and put - in front of one to leave those songs out (`-type:mid`). With only filters, the list stays sorted and grouped. Hover the search box for all of them.
- Three ways to match: as typed or in 'single quotes' fuzzily (`artist:'daft pk'`), in "double quotes" closely (the same past case, spacing and punctuation: `artist:"daft punk"`, or a phrase in the search), and after a ^ exactly (`artist:^"Daft Punk"`). Smart playlists can use "is like (fuzzy)" and "is exactly (case too)" rules, and their "is" sees past punctuation too.
- The search looks in your Songs columns too (the ones you've picked, shown or not), and highlights what it matched. Preferences > Songs > Search looks in leaves out what it shouldn't.
- The search box completes filters: their keys, then your artists, albums, genres, tags... (only those of the songs your other filters keep): Tab or a click puts one in, the arrows pick, and Alt switches it between fuzzy, close and exact. A filter that doesn't make sense (`year:soon`) is underlined, saying why on hover. It says how many songs it found, Ctrl+F or / goes to it and Esc empties it.
- Lengths, sizes and dates the way you'd say them, in the search and in smart playlist rules: a length as 6:00, 1:02:30, 6m, 90s or 1h2m (a bare number is now minutes, not seconds), a size in MB or as 800kb or 1.2gb, added as 12h, 30d, 2w, 6mo or 1y ago, or in a year, month or day (`added:2026-10` is all of October). Numbers, lengths, sizes and dates take >, <, >=, <= and ranges, open-ended too (`length:3m..5m`, `track:..3`, `added:2026..`), and `year:90s` or `year:1990s` is a decade. A rule's value that doesn't make sense shows in the warning color.
- Save a search as a smart playlist (the engine button beside it): its filters become the rules, and the editor says what couldn't be one.
- Remove songs from all your devices: with your song library syncing, removing songs offers "Remove from all my devices", and your other devices delete them too. A song you'd only removed from one device can go everywhere from the sync settings' "Songs removed from this device" list. Importing a song again brings it back everywhere.
- Command palette: Ctrl+Shift+P lists what the app can do with its keyboard shortcut beside it. Type to filter, Up/Down and Enter (or a click) to run one, Esc to close.
- Change the keyboard shortcuts: Preferences > Keyboard lists every command with its keys. Click a key to change it, + to add another, right-click one to remove it, or Reset. A key bound to two commands shows in the warning color. Your keys sync with your other devices (Keyboard shortcuts, under App in what syncs), and "This device only" keeps a command's keys to the device you're on.
- Key sequences: a command's keys can be up to three presses one after another. Ctrl+G then a letter goes to a tab (S Songs, G Groupings, F Soundfonts, V Visualizer, E Script Editor, R Scripting Reference, P Sprite Editor, L Online Library), and Ctrl+Tab and Ctrl+Shift+Tab step through the open tabs. Holding Ctrl through the letter works too. A moment into a sequence, a panel at the bottom lists what can follow; Esc cancels. Set your own in Preferences > Keyboard by pressing the keys one after another.
- Lists from the keyboard: in the tab in front (Songs, Groupings or Soundfonts), Up/Down move through the rows, Page Up/Down and Home/End jump, Shift held selects as it goes, and Enter plays or opens the row. Ctrl+A and Delete work there too, not just under the pointer. Shift+Left/Right seek 30 seconds.

### Changed
- Branch builds can use online features again, one kind at a time: the Online Library, sign-in and sync, and album art and details lookup each start off and are turned on for that run only, in Preferences > Online Library, with Turn on for this run where they'd be, or with `--online scripts,sync,lookup`. Never saved, so every launch starts offline.
- Scripts now see the function keys the app doesn't use: F5, F9, F10 and F11 reach a script once they're rebound away from their commands. Escape always stays the app's.
- The player buttons' hover text shows their keyboard shortcut (Play/Pause (Space), Next (N), Fullscreen (F)...). Shortcuts no longer fire while a script control is waiting for a key to bind.

## 1.3.0 (2026-10-21)

### Added
- Edit songs' details: Song info (a song's right-click menu, or the info button with songs selected) has a Details tab with the title, artist, album artist, album, composer, genre, track, disc and year to change, and the file's own details under File. Select several songs to change them all at once: a field they differ in says Mixed, and only the fields you change are written. Edits sync with the library and never touch the files; the arrow beside an edited field goes back to what the file says, and the undo button in the Songs toolbar takes back the last save.
- Clean up (the broom in the Songs toolbar): suggestions for your songs' details. It splits several artists in one ("\"Perry Porter\"& Nujabes" at "&", ",", " x ", "feat." and "ft.") and genres ("Hip-Hop/Rap"), writes a name spelled two ways one way ("Kan R. Gao" and "Kan (Reives) Gao"), and tidies stray quotes, brackets and spacing. Apply one, change what it'll be written as first, apply them all, or dismiss the ones that are wrong ("Simon & Garfunkel"); dismissed ones stay dismissed on all your devices until you bring them back. They're made as edits, so the undo button takes them back too.
- Playback options (Song info > Options): where a song starts and stops (drag either end of the bar over the song, or type "0:30"; buttons play from the start or just before the stop to hear them), its volume up or down (a slider, -100 to +100%), skip it when shuffling, and remember where it was left (it starts there next time, on any of your devices).
- Play together: select songs in Songs and pick Play together from their menu, and shuffling plays them as one song, in that order (an album's sides, a medley, a song and its intro). Songs linked this way have a link beside their name in Songs, and Groupings has a Play together page of the sets (once there are any) to see each in order, play it, put it in another order or take songs out. Song info > Options lists the songs it plays together with, to put in another order or take one out. A song plays together with one set at most, and the sets sync with your library.
- Missing info: Preferences > Songs lists the songs each ruleset leaves out because they don't say what it splits by (songs with no album aren't in By Album), with Edit them to fill it in for them all at once in Song info. The warning button in Songs, there while any are missing, shows only those songs.
- More Songs list columns: Grouping, Comment, Bit rate and Sample rate. Grouping and Comment are read from the files' tags and can be changed in Song info, and the list can be grouped by Grouping or Sample rate. Songs already in the library are read once more for them, in the background.
- Sorting past "The": names sort past a leading The, A or An (The Beatles with the Bs) in the Songs list, smart playlists and Groupings. Preferences > Songs can turn it off.
- Sort as (Song info > Sorting): what a song's title, album, artist, album artist and composer sort as, read from the file's sort tags when it has them ("Beatles, The"). They're Songs list columns too.
- Compilations: Song info has a Compilation box (ticked already for files tagged as one). A compilation's album is by Various Artists, and By Artist files it whole under them unless it has an album artist. Clean up suggests it for albums by several artists with no album artist.
- Use composer as artist (Song info): the song's artist is its composer, for classical music filed under who wrote it.
- Artists, album artists and composers are lists, split at semicolons: a song by "Perry Porter; Nujabes" is in both artists' groups and By Artist playlists, and an album by "Uyama Hiroto; Nujabes" says "Uyama Hiroto & Nujabes", each a link to their page. A comma or "&" doesn't split them by itself, since they can be part of a name; Clean up suggests those splits instead.

## 1.2.0 (2026-10-21)

### Added
- Smart playlists: + > Smart playlist... in Playlists makes a playlist of the songs that match your rules (artist contains, year in a range, added in the last 30 days, has a tag, starred...), all of them or any, optionally limited to some songs, minutes, hours, MB or GB picked at random, by title, by album, newest... It keeps up as songs come in, the same on all your devices (random picks too: Re-seed picks again, everywhere). Edit its rules from the playlist's sliders button, or keep a copy that stays as it is. On a phone the rules can be looked at, not edited yet.
- Rulesets (Preferences > Songs): rules that make a playlist for each artist, album, genre, year, composer or tag of the songs they match. By Artist and By Album are built in, and you can make your own. They sync like your other settings.
- Groupings (View > Groupings, or the playlist button in the controls; on a phone, Playlists and each ruleset are in the menu after Songs): your playlists, then the playlists each ruleset makes, By Album, By Artist and your own, picked along the top. Playlists moved here from their own tab: a playlist opens on its page to play and edit as before, + makes one, and dropping songs on them all makes a new one of them. Smart playlists have an engine in their tile's corner, and the engine is now smart playlists' icon everywhere (it was a sparkle). Show them as a grid of tiles or a list, search them, sort them by title, size or what's newest, or play them all. Open one for its page: its art, its artist (an album's links to them), play (it plays through to its end, whatever Autoplay is set to), shuffle, star them all, find in it, and its songs under their discs or albums. Each ruleset remembers where you were (search, sort, grid or list, what's open, how far you scrolled) until you close synththing.
- Album art from the picture in the songs' files, read in the background. Anything without art gets a tile in a color of its own with its first letters: one to four of them (Preferences > Songs; four make a square).
- A playlist remembers the song it was on: the fast-forward button beside Play continues from there.
- Spellings of a name are one group: albums, artists and the Songs list's groups see past case, spacing, punctuation and symbols (and full-width letters), so "<OST>" and "OST", or "Justcore Extravaganza" and "JUSTCORE EXTRAVAGANZA", are one, shown as the spelling with mixed case (else the one most of its songs have). Preferences > Songs can name more characters to pass over.
- An album is its name's songs, whoever each is by: a soundtrack with songs by several artists, or with its album artist spelled two ways, is one album (its artist line says Various Artists when its songs' artists differ and no album artist is set), its songs in track order.
- Genres are lists: a song tagged "Avant-Garde, Electronic, Rock" (commas or semicolons) is in each of those genres, in By Genre rulesets, the Songs list's genre groups and genre rules. Genres tagged by their old ID3 numbers ("17", "(17)") show as their names (Rock).
- More Songs list columns: Disc and Size. Song names can use every field ({disc}, {size}, {imported}...), and smart playlist rules can go by file type.

### Changed
- "Imported" is "Date added" everywhere, and the File column shows the name without its extension (Type has that).

## 1.1.0 (2026-10-20)

### Added
- Starred songs: click the alien at the left of a song, in Songs or a playlist, to star it (again to unstar it). Its neon green is part of the theme.
- Only play starred songs (Preferences > Playback): shuffle, autoplay and playlists playing on go only to starred songs. A song you pick yourself still plays.
- Starred songs as a sync choice, beside songs in playlists and every song: a device can fetch your starred songs from your others too.
- Every song's details (title, artist, album, album artist, composer, genre, track, disc, year, length and size, and when it was first imported) sync with the library, so a device knows them for songs it doesn't have yet: a playlist shows their names, for one.
- On a phone, anything drawn past the side of the screen is noted in the log (Help > Log): on which page, what (text or a control) and where, so a report with its log shows what didn't fit.

### Fixed
- On a phone, Credits & licenses fits the screen (long license names in the library list are cut short instead of widening the page), and the Profiler scrolls sideways instead of growing past it.
- On a phone, more fits the screen: long group names in Songs, playlist entries, soundfonts and the song name at the bottom scroll instead of running off or being cut short (in lists, on the playing row and the one you touch); the Visualizers page, the Branch builds list, notifications and a notification's window fit the screen's width.

### Changed
- Synced data is compressed before it's sealed, so a library of several thousand songs fits.
- Playlists and Soundfonts look like the Songs list: on a phone, each row is the name over a small line of details (a playlist entry's number, length and soundfont; a soundfont's size and whether it's in use), and the soundfont in use is outlined like the playing song. Their X buttons are gone: remove from the right-click (or long-press) menu, or with Delete.
- Songs that aren't MIDI start playing almost at once: playback begins as soon as the first moment is decoded, and the rest decodes while it plays. Before, the whole file was decoded first, which took a noticeable moment on a phone.
- Song names (and column values) too long for the Songs list scroll sideways instead of staying cut off, on the playing song and the one under the pointer or your finger.
- Toasts too long for the window scroll sideways instead of running off its edges, and stay up until they have scrolled through.

## 1.0.2 (2026-10-20)

### Added
- Your devices, under the sync switch in Preferences > Online Library: the devices your songs move between, when each last synced, and Remove for one you don't use any more (a test install, a lost phone), so songs stop being sent for it. One that doesn't sync for 14 days is removed by itself; one removed comes back if it syncs again.

## 1.0.1 (2026-10-20)

### Added
- Songs go straight to your other devices that are open: streamed through the server as they're sent, kept nowhere, and of any size. Only for a device that isn't open do they wait on the server.
- Send feedback can include how smoothly the app ran over the last minute (on unless you untick it): how many frames took too long, and how long each part of the app took on average and at worst. It helps track down stutters.
- A profiler overlay: Help > Profiler (Profiler in the menu on a phone) shows how long each part of every frame takes, live, as a flame graph. It only measures while it's open.
- Frames that take too long to draw (over 16 ms, or one part over 8 ms) are noted in the log (Help > Log), with how long each part of the app took, to track down stutters.
- Songs can be grouped by tag: right-click a column header (or use the sort button on a phone) > Group by > Tag. A song with several tags shows in each of their groups, and songs without tags get an Untagged group at the bottom.

### Fixed
- Syncing a big library to a slower device no longer loses songs on the way: a device leaves songs on the server only while it has room for them, then waits for your other devices to download what's there. Before, it kept sending, and the server dropped songs nobody had downloaded yet to make room, only for them to be sent again.
- A song too big for the server waits until the device that wants it is open, instead of never moving.

### Changed
- The visualizer never holds up the rest of the app: the app no longer waits for each picture, and turns it into an image off the main thread. A slow script just draws fewer frames, so the 30 fps fallback and "Try 60 fps again" are gone. Each picture shows about a frame after the audio it was drawn from.
- Server: songs stream between devices (`/api/v1/relay/inbox/{inbox}`, `/api/v1/relay/{id}/send/{inbox}`, `/api/v1/relay/listening`), and `/api/v1/relay/room` says how much more of an identity's songs the relay holds. Relay requests are signed without their body (songs are sealed in chunks now). Apps and server need to be updated together, server first. Behind nginx, `/api/v1/relay/` needs `client_max_body_size 0`, `proxy_request_buffering off` and `proxy_buffering off` (docs/DEPLOY.md); Caddy needs nothing.

## 1.0.0 (2026-10-20)

### Added
- Tags for songs: your own labels, each with a color, shown on songs in Songs and in playlists. Right-click songs > Tags to tag them or make a new one; Tags > Edit tags... renames, recolors and deletes them. The search box finds songs by their tags too. With sync on, tags sync with your songs.
- You can see songs moving between your devices: a spinner and a count (done, of how many) above Songs, in the status bar and under the sync switch, then "Downloaded 12 songs from your other devices." once they're in. With "Show what each sync changed" on, each song shows as it arrives or goes.
- Songs shows columns: Artist, Album and Length to start with. Click a column's header to sort by it (again to turn it around), right-click a header to pick columns (album artist, composer, genre, track, year, BPM, notes, type, when it was imported, file name), or group the songs by one of them (artist, album, genre, year, type, the month imported...); click a group to fold it. Groups ignore case (mid and MID are one). Sorting by a column you don't show adds it while you do. On a phone, the sort button holds the same, and the value sorted by shows under each song's name. Columns, sorting and grouping sync with your settings.

### Changed
- Song details moved from the little i on each row to Song info, in a song's right-click menu (and in a playlist entry's), and to the i button above Songs for the selected song. They show the file's original name too.

### Fixed
- Settings sync failing with "invalid type: null, expected f32" on a device with a layout that had never been shown.

## 0.11.1 (2026-10-20)

### Fixed
- Songs move between your devices again when one of them is too big for the server: it's named in the sync error and stays where it is, and the rest still move (before, every other song waited behind it). A failed song no longer stops the others either.
- Sync turned on before updating starts syncing playlists and songs, without turning it off and on again.
- A song fetched from another device shows its details straight away, not as unreadable until a restart.
- A server too old to move songs says so, instead of the songs just never arriving.

### Changed
- Server: `relay_song_mb` is 250 by default (was 100); raise nginx's `client_max_body_size` to match. Past `relay_mb_per_identity`, the oldest songs waiting go to make room (they're sent again when wanted) instead of new ones being refused. `/api/v1/info` says it as `relay_song_mb` (0 without sync).

## 0.11.0 (2026-10-20)

### Added
- Songs come over from your other devices, through the server (encrypted on your device first, so it can't tell which songs they are): with sync on, a song in a playlist that isn't on this device is fetched from one that has it. Every song, under Songs in What this device syncs, fetches every song in your library instead, wherever it was imported. Removing a song deletes it on this device only: the others keep theirs, and this one doesn't fetch it again (Songs removed from this device, under the switch, lists them, to fetch one again). Server: `relay_song_mb` (100) and `relay_mb_per_identity` (2048) in `server.json`, and `POST /api/v1/relay/{id}`.
- Sync playlists between your devices, with your settings: the switch is now Sync settings and playlists between my devices (Playlists, under Songs in What this device syncs, turns it off on one device). A playlist renamed or changed on one device changes on the others; changed on two at once, the newer change wins. A song a device doesn't have yet shows as unavailable there, by its name. Which soundfont an entry uses stays on each device. Sync turns itself off once after this update: turn it back on.

## 0.10.2 (2026-10-20)

### Changed
- Songs is your song library now, on the desktop as on Android: synththing keeps its own copy of each song (named by its contents, so the same song is the same everywhere), and playlists name songs in it, so moving or renaming the originals never breaks one. + imports songs and the folder button a whole folder; Library downloads, the starter songs and files dropped onto a playlist go into it too. Right-click a song for Save a copy as... and Remove from library (a playlist keeps a removed song, shown as unavailable). The first launch brings in the songs from your Songs folder and changes your playlists over, leaving the folder as it was. Songs no longer browses folders on disk; a file dropped on the window still plays from where it is. Song names lose `{file_path}`, and `{file_name}` is the name the file had when it was imported.

### Removed
- Songs in the Online Library: its Songs side, downloading and publishing songs, their previews and the Downloaded songs tab. The Library is for scripts again; songs are to sync between your own devices instead. Server: `/api/v1/songs` and the `songs` setting are gone (songs already stored stay in the database, unused), and so is `synththing preview --song-start`.

## 0.10.1 (2026-10-20)

### Added
- Sync settings between your devices: Preferences > Online Library > Sync settings between my devices, on each device signed in as you. A setting changed on one changes on the others, even if one was offline at the time (if both changed it, the newer change wins). What belongs to one device stays on it: folders, layouts, the audio buffer, the mobile layout, the UI scale and updates. Settings are encrypted on your device before they're sent, with a key only your devices have. Turning it on takes the settings your other devices synced. Your own themes go along with the theme setting. What this device syncs, under it, chooses which settings each device takes part in, by category or one by one; last volume and speed can sync too, but are off by default. Show what each sync changed, under it, is for testing. Server: `"sync": true` in `server.json`, and `POST /api/v1/sync/{name}`.

### Changed
- Exported identities and sign-in codes carry the key settings sync uses, so a device signed in either way can sync.
- Your own themes are kept with the rest of the settings (`config.json`) instead of in a `themes` folder, so settings sync carries them. Themes in the old folder aren't brought over.

## 0.10.0 (2026-10-20)

### Added
- An Android app, experimental: `synththing-android-arm64.apk` on each release. A layout made for a phone, with the visualizer on top and the controls under it; songs and soundfonts are imported from the phone; playback goes on in the background, with controls in the notification and on the lock screen; and leaving the app while it plays shrinks the visualizer into a floating window (Preferences > Visualizers turns it off or picks its shape). Scripts' previews come from the Online Library, whose list opens a script's details on a page of their own, with back (or Escape) returning to the list. Updates, the changelog, the log and the credits are pages of the menu, and an update found or installed at launch opens its page. It reads the Online Library without an account: install and update scripts, but no publishing or encores. See docs/ANDROID.md.
- The mobile layout on desktop too (View > Mobile layout), to try it, or for a narrow window.
- Sign in another device as you, without moving files around: Preferences > Online Library > Sign in another device... shows a QR code and a link, and on the other computer, Sign in with a link... takes the link. The code works once, for 10 minutes, and the server passing it along only ever holds it sealed. The device that showed it says which device signed in (each one's name is set beside the buttons). Server: `POST /api/v1/pair` and the endpoints under it.
- The Android app can sign in: scan the code from Sign in another device... with the phone's camera (or use Sign in with a link... in the app), and it's you there too, with encores, reports, deleting your uploads and notifications. Publishing stays on the desktop. Server: `GET /pair` (where a link lands outside the app) and `/.well-known/assetlinks.json` (`android_fingerprints` in `server.json`).
- Moving your identity to a new key (New key..., or recovering it by email) signs out every other device signed in as you, and says so first. The others notice the next time they talk to a server, forget the old key and say they're signed out; Sign in another device... signs them in again. Server: a replaced key is refused with a 410 that names it (`replaced`).

### Changed
- The toolbars are slimmer: Restart, Fullscreen, Settings, Controls and Debug above the visualizer, the playlist's Play, New, Rename, Export and Delete, the Songs browser's Browse, Set as default and hidden-files toggle, and the editor's Format, Find and History are icon buttons now (hover one to see what it does), and each toolbar is one row that wraps when its tab is narrow.
- New script, Rename..., Restore default and Publish... moved into the script picker: New script lists the templates and examples, the sidebar's Make a copy starts a new script from any script, and the running script's card has its file's buttons and its full path.
- The editor's Apply, Wrap and text size are under its cog.
- Preferences are arranged by what they're for: Appearance (with the theme and the UI scale, which moved there from the Themes window), Playback, Songs, Visualizers, Online Library, Updates, App and Recording. In the mobile layout they're a page of their own, each section opening in its place, and back (or Escape) steps out again. Themes are edited from Appearance too (Edit themes..., in place of the synththing menu's Themes...); in the mobile layout that's a level under it, with the theme and its buttons along the top and the preview along the bottom.
- A canary build updates to canary builds unless Preferences > Updates says otherwise; before, it waited for the next release unless Canary updates was turned on.
- Song names are set under Preferences > Appearance. On Android they show on the now-playing screen and in the notification too, and {file_path} is the folder inside the song library.

### Fixed
- On Android, the keyboard works in text fields as it does in other apps: autocorrect, predictions and gesture typing.
- With a very round theme, the seek bar's filled part no longer runs past its handle.

## 0.9.0 (2026-10-19)

### Added
- Select several songs at once in Songs, the way a playlist already allows (Ctrl and Shift, Ctrl+A), and drag them onto a playlist together. The Soundfonts list selects the same way, so several can be removed at once.
- Songs are named by their tags. Preferences > Songs > Song names holds a list of patterns such as "{title} by {artist}"; the first one a song has every field of names it, and a song without them keeps its file name. Besides tags, a pattern can use {file_name}, {file_extension}, {file_path} (the folder, inside the Songs list's default folder), {length}, and for MIDI files {bpm} and {notes}. The name shows in the Songs list, playlists, the now-playing bar and the tray. Search matches both the name and the file name, and a Sort button switches the list between the two. Tags are remembered between launches.
- Text in every script shows as written instead of as boxes: Japanese, Korean and Chinese (with punctuation like 『 』), and through the fonts installed on your computer, everything else they cover (Thai, Hebrew, symbols and more). This covers song names, tags and every other list. Japanese and Korean come with the app, in Noto Sans to match the rest of the text.

### Changed
- Songs, Soundfonts and the playlist are one kind of list now and work alike: click selects, double-click plays a song, opens a folder or uses a soundfont.
- A name too long for its list is cut short with "...", and hovering it shows all of it.
- Feedback asking for something new is filed as a feature request for the developers to decide on; only bug reports go on to automatic fixes.

### Fixed
- A long script path on the Visualizer page no longer stops the page from getting narrower; it's cut short with "...", and hovering it shows all of it.
- On Linux, the Songs list stopped re-reading its folder over and over just because files in it were read.
- Right-clicking the tray icon closes the mini player as it brings the main window back, so the two aren't on screen together.
- The main window and the mini player are never on screen together: left-clicking the tray icon puts the main window away in the tray as the mini player comes up, and bringing the main window back from the taskbar closes the mini player.

## 0.8.1 (2026-10-18)

### Changed
- Feedback is filed once an hour, a batch at a time, so notes about the same thing end up in one issue instead of several.

## 0.8.0 (2026-10-18)

### Changed
- Feedback you send is sorted into synththing's issue tracker on GitHub by Claude, so it turns into a fix sooner. The issue describes it in Claude's own words, without your log or anything that identifies you; the privacy policy says so.

## 0.7.1 (2026-10-18)

### Added
- Canary updates (Preferences > General): the newest build from master, minutes after every change, and releases once they're out. For trying new things early; it can be broken. Test builds name themselves in the title bar, and their icon and wordmark have a color of their own: yellow for canary, another for each branch build.
- Branch builds (Help > Check for updates > Branch builds): try a feature before it's merged. Each installs separately, starting with a copy of your settings (take a fresh one from its Help > Check for updates), and follows its branch; your usual synththing isn't touched. Their online features are off.
- Send feedback (Help > Send feedback..., or the speech bubble at the top right): one box to say what's working, what isn't and what you wish synththing did. No account needed; the app's version and your OS go along, and its log if you tick the box. It works from branch builds too, so you can say how a feature you're trying works for you.
- More audio files play: Opus (`.opus`, and `.ogg` files holding Opus), Apple Lossless `.m4a`, AIFF, CAF, Matroska audio (`.mka`), MP2, WMA, AC3 and ADPCM WAVs. Anything synththing can't decode itself goes through ffmpeg, the same one recording uses. Their length, tags and codec show in the song info too.

### Changed
- Opening synththing while it's already running brings the running one to the front (out of the tray too) instead of starting a second copy, which would save over the first one's settings. A branch build and your usual synththing still run side by side.

### Fixed
- `.m4a` files (AAC) play; they failed with "The format of the data has not been recognized".
- Settings saved by a different build that this one can't read are no longer replaced by the defaults. Whatever can still be read is kept (a layout with a tab this build doesn't have costs just the layout), the whole file is saved beside it as `soundfonts.json.unreadable-...`, and synththing says so when it starts. The library identity's key and its rotation records are kept the same way; the key used to be replaced by a new identity.

## 0.7.0 (2026-10-09)

### Added
- A mini player (Windows): click the tray icon for a little window that stays on top, with playback, the volume, and your playlists and soundfonts. Drag it anywhere by its header; it opens where you left it. Its expand button grows it to the left with the visualizer, as tall as the window (drag its left edge for anything from tall 9:16 to wide 16:9), and a picker for switching visualizers over a softened view of the one that's running (the mouse wheel scrolls through them). Right-clicking the tray icon now brings the main window back (it opened a panel in 0.6.2: that's the mini player unpinned now).
- The bundled visualizers fit the mini player: fewer labels in FFT and Spectrogram, just the tempo in Pulse, no titles or legend in Dashboard (whose panels stack in a column when it's taller than wide), and no help line in Keyboard.
- Scripts can tell when they're in the mini player: `display_mode()` is `"mini"` there, so they can leave out detail too small to see.
- `shift_frame(dx, dy)` scrolls the kept frame, so a scrolling display draws only what's new. The Spectrogram works that way now, several times faster.
- Outlines for scripts: `rect_outline` and `circle_outline` draw just the edge, any width, blending evenly all the way round. The Disco ball's rim light is one now.
- Note helpers for scripts: `note_name(key)` gives `"C#4"`, `note_freq(key)` a key's pitch in Hz, and `freq_note(hz)` the key at a pitch.

## 0.6.2 (2026-10-08)

### Added
- synththing can keep running in the tray (Windows): closing the window hides it there, music and all, if you like. The first time you close it, it asks. Click the tray icon to bring synththing back; right-click it for a little panel with what's playing, play/pause and skip, the volume, and your playlists and soundfonts, which goes away like a menu when you click elsewhere and opens again where you left it. Preferences > General changes your answer, and Quit (synththing menu, Ctrl+Q, or the panel) always closes it completely.
- Closing while a recording or render is running asks what to do with it first: save it, discard it, or keep going (a render can also close once it's done).

## 0.6.1 (2026-10-08)

### Added
- The status line keeps a history: the button at its start shows the recent messages, newest first, with how long ago and colored like the line (red for what failed). A red dot on the button means something failed that you haven't looked at yet. "Open the full log..." goes to Help > Log, showing just these.
- synththing has an icon: in the title bar, the taskbar and on the exe itself. The menu bar's "synththing" is the new wordmark, with a thin outline on themes where its purple would be hard to see.
- synththing starts the way it was left: the same volume, soundfont and visualizer script (and loop and shuffle, as before). Each can be turned off, and the speed turned on, in Preferences > Playback.

### Changed
- Every channel can be turned off at once, for silence: the last one playing used to stay on. "All" (or turning any one back on) brings them back. Scripts' `set_channel_enabled` can now do the same.
- The audio buffer setting moved from the transport bar to Preferences > Playback, and it's remembered now (it went back to 150 ms every launch).

## 0.6.0 (2026-10-08)

### Added
- Scripts can look back at the last few seconds of sound: `history_left(seconds, [count])` and `history_right` give the samples played, oldest first, or `count` of them spread evenly, one per pixel say, so a scope or a slow trace no longer needs its own ring buffer.
- `line` takes a width: `line(x0, y0, x1, y1, color, 4)` draws a 4-pixel band with round ends, so a thick trace joins up cleanly.
- `peak_left()` and `peak_right()`: the loudest sample each frame, for peak meters and clip lights, next to `level_left`'s average loudness.
- Spectrum helpers for scripts: `fft_bin(hz)` finds a frequency's entry in a spectrum, `fft_freq` goes the other way, and `fft_band(spectrum, lo_hz, hi_hz)` gives the peak and average of a range (bass, mids, treble).
- `approach(current, target, rate)` for smoothing that's the same at any frame rate: meters and followers no longer run faster at 120 fps or slower in a 30 fps recording. The FFT, Pulse and Dashboard visualizers use it.
- Color helpers for scripts: `hsv(hue, saturation, value)` for colors around the color wheel (hue in degrees), and `mix(color1, color2, t)` for anything in between two colors.
- Scripts can tell which instrument each channel plays: `channel_program(c)` gives its program and General MIDI name (following changes partway through a song), and `channel_name(c)` the name of the track it's in. Dashboard's piano roll uses them for a legend.
- Scripts can move and clip their drawing: `translate(dx, dy)` shifts everything drawn after it, `clip(x0, y0, x1, y1)` keeps it inside a rectangle, and `push_view()` / `pop_view()` save and restore both, so a panel or widget can draw in its own coordinates without spilling out.
- A new visualizer, Dashboard: a scope, a spectrum, a piano roll and level meters in four panels, with a shake when a sound starts.
- Scripts can leave trails: `set_clear_color({r,g,b,a})` sets what each frame starts from, blended over the frame before. A translucent color fades the last frame a little each time, and `a = 0` keeps it as it was, so a script can draw only what changed.

### Changed
- Hovering a channel toggle above the visualizer says which instrument that channel is playing, and the name of its track in the file.
- The channel toggles can be colored chips that say what each channel plays ("3 · Violin"): click one to turn its channel off, and it goes grey. Preferences > Songs > "Channel toggles as chips with instrument names".
- The Waveform visualizer has a setting for how much sound it shows across the window, from a few milliseconds up to 4 seconds, a trail setting that lets old traces fade out slowly, like an old oscilloscope's screen, and a thickness setting.
- The FFT visualizer's meters hold the loudest sample, like a studio meter, instead of the highest recent level.
- The Disco visualizer's light beams are drawn as solid bands rather than bundles of thin lines: smoother, with no stripes.

### Fixed
- The FFT and Spectrogram visualizers show each frequency where it belongs: they were reading every bucket from about 43 Hz lower than its place on the scale.

## 0.5.3 (2026-10-08)

### Changed
- Scripts can play notes from the moment they start, whenever a soundfont's loaded, not only once a MIDI song is: before one is (or with an audio file), they play on General MIDI's defaults, a piano on every channel and drums on channel 9.

### Fixed
- Online Library entries can be clicked anywhere again: since 0.5.2 their text could be selected instead, which got in the way of picking them.

## 0.5.2 (2026-10-07)

### New
- Colored chips in the Online Library: each script's kind (visualizer, game, toy, example) and each song's source (original, video game, film or TV, anime, popular, classical, folk) has a color, shown as a chip in the list and the details, and as a dot in the dropdowns that pick them. They're from the same palette as the channel toggles.
- Notifications in color, by how much each asks of you: good news (encores, downloads, remixes) in pink, green and purple; things resolved in cyan and blue; heads-ups in yellow and coral; and what you need to act on (a takedown, a deletion) in red. The envelope's count takes the color of the most serious one unread, and ones you've dealt with turn grey.
- The status line is red when something failed and green when something's done.
- The playback controls are icons now (hover for what each does), with Play in green and Pause in orange.
- The seek bar is colored by the song's tempo: slow songs blue, through cyan, green, yellow and coral, to pink for fast ones, shifting as the tempo changes. While a song plays it flashes brighter on each beat; Preferences > Songs sets how far toward white (0% for not at all, 100% for white) and how long each flash lasts, a bar's first beat a little longer.

## 0.5.1 (2026-10-07)

### New
- Channel toggles in color: the toggles above the visualizer are filled with each channel's color while it's on, and outlined in it while it's off. They're the keyboard visualizer's colors (as in song previews), unless the script says which it draws each channel in (`channel_colors`).

## 0.5.0 (2026-10-07)

### Fixed
- On Windows, the app has room to read very large scripts (in the editor, or publishing one) without crashing: its main thread's stack is 16 MB, as on the threads that run scripts, instead of Windows' 1 MB.

## 0.4.9 (2026-10-07)

### Fixed
- The scripts that come with synththing (keyboard, disco, the games...) can be updated again on Windows: they were added with Windows line endings, so they never matched a version on the official server and stayed at "version 0" with nothing to update to. Copies added that way are put right by their next update check.

## 0.4.8 (2026-10-07)

### New
- Library servers can host songs (MIDI files), when their owner turns it on (`"songs": true` in `server.json`): each upload declares the poster's right to share it, says where it's from, and is checked by its notes, so the same song posted again by someone else isn't taken.
- Songs in the Online Library: a Songs side beside Scripts, for the servers that take them, with each song's credits, where it's from and the rights it's shared under. Download saves one in a Library folder beside your songs, and the new Downloaded songs tab (View menu) plays them, shows them in their folder, or deletes them.
- Publish song..., in the song browser's right-click menu: share a MIDI file on a library server that takes songs. Its title, composer and arranger are filled in from the file when it says.
- Song previews: the selected song plays its liveliest stretch on the keyboard visualizer, muted; click it to hear it with your soundfont.

### Changed
- The Script Library is the Online Library now, since it has songs too.
- The keyboard visualizer's keys keep a grand piano's shape (about 6.4 times as long as they're wide) whatever the window's, instead of always taking 28% of its height.

### Fixed
- A preview's "Loading..." (or "No preview.") space no longer overlaps what's below it in the Online Library.
- Notifications stop asking a library server that doesn't have them (one running an older version), instead of trying every half minute.

## 0.4.7 (2026-10-07)

### New
- Copyright notices on library servers: Report... has "It's my copyrighted work", a legal notice (as the US DMCA has them) that takes the script down at once. Its uploader is notified, and can dispute it (a counter-notice) or let it stand; moderators decide disputes (in Moderation's new Copyright tab), an upheld one keeping the script safe from further notices. Takedowns that stand can't simply be published again, and three of them get an identity banned.
- Notifications you haven't read stand out in the list, and once you've acted on one (disputed a notice, say), it says what you did instead of offering it again.
- Notifications from library servers: the envelope at the right of the header turns red with a count when there's news about your things (encores and installs adding up, a script hidden or back, a remix of yours, a report of yours dealt with; for moderators, new reports). Click it for the list, every server's together, and one for what can be done about it. Preferences > Library > Notifications turns it off.
- `--config-dir DIR` (or `SYNTHTHING_CONFIG_DIR`) keeps everything in a folder of its own, for trying a build without touching your real setup.

### Changed
- The Script Library searches again each time it's opened, so it's always current.
- Publishing ends with what was published and a button to open it in the Library, instead of the window just closing.

## 0.4.6 (2026-10-06)

### New
- Library servers can offer documents to read, like their EULA and privacy policy: icon buttons in the Script Library (hover for which) open them. The official server has both.
- Delete my data... (Preferences > Library) has a library server delete what it has of yours: your scripts, encores, admin rights and email. It's out of sight at once and gone for good after 30 days; until then, Restore my data puts it back. Where your identity has an email attached, a code sent to it confirms it.
- Library servers can be updated by their admins: the Moderation window's Server tab (and `synththing admin update`) says when a newer release is out and updates to it.

## 0.4.5 (2026-10-06)

### New
- Adding soundfonts takes several at once: pick as many as you like in the file dialog (or drop them on the window), and the status line says how many were added and what went wrong with any that weren't.
- The official scripts' publisher key can be replaced, should it ever get out (`synththing publisher-rotate`); the app learns the new one from the official server, so official scripts stay marked and updatable.

## 0.4.4 (2026-10-06)

### New
- A recovery email for your library identity (Preferences > Library): attach one, and if you lose your key, Recover... gets your identity back with a code sent to it. The server keeps only a hash of the address.
- New key... moves your identity to a fresh key, if the old one might have got out: your scripts, encores and admin rights follow it, and the old key stops working. Other servers catch up the next time the app talks to them.

## 0.4.3 (2026-10-05)

### New
- Encores, in the Script Library: say you liked a script (and take it back); they add up on its author's page.
- Report... in the Script Library: tell a server's moderators what's wrong with a script, pointing at the lines of its code and the sprites in it that are the problem.
- Remixes: Publish remix... (by Update, for a script installed from someone else) shares your take on it, credited to the original. A script lists its remixes, and a remix links back to what it came from (or says it was remixed from a deleted script), even on another server: following the link asks to add that server (or turn it on) if it isn't in use.
- Library servers refuse a near-copy of someone else's script (or of one that comes with synththing) unless it's published as a remix of it, and a remix with nothing changed. A copy of a bundled script is published as a remix of it by itself.
- Presets: `settings_preset(name, {...})` in a script offers named sets of setting values, picked at the top of the Settings window (with Defaults). Save your own there too, and publishing offers to add your settings to the script as a preset. highway.lua has Relaxed, Expert and Focus; settings_demo.lua shows how.
- Moderation, for a library server's admins (in the Script Library): reports with what they point at highlighted, hiding, deleting, bans (a key or an address, for a while or for good; a key's ban can follow it to the addresses it uses) and admins. `synththing admin` does the same on the server itself.

## 0.4.2 (2026-10-05)

### New
- Previews: the Script Library shows each script playing, a few looping seconds picked from the liveliest part of the starter songs (a game is seen playing itself). The server makes them after each upload.
- The script picker has a sidebar: the script under the mouse, playing, with where it's from, who made it and its description. Scripts without a preview from a library get one made here the first time they're looked at (and again after they're changed).
- Library servers refuse a script that doesn't run (it doesn't compile, or errors in its first seconds), saying why.
- `synththing preview` makes a script's preview from the command line (`--check` only checks it runs).

## 0.4.1 (2026-10-05)

### New
- Render, in the Record window: the video is made in the background, frame by frame, by its own copy of the script and the synth, with the visualizer showing its progress (and a glimpse of it) meanwhile. Every frame is drawn and the sound is exactly in step at any size, so frames never repeat, whatever the script or the encoder can manage live. A song or a whole playlist; Stop keeps what's done.
- A saved recording says how big the file is.
- The Script Library (View > Script Library): search scripts shared on library servers, install them, and publish your own. The official one is synththing.p51.nl; more can be added in Preferences > Library, and anyone can run one with `synththing serve`, directly or in Docker (`ghcr.io/dissssy/synththing`, docs/DEPLOY.md).
- Library identities: what you publish is signed with a key the app makes (no account, no email), shown as an ID like `#k3f9q2xa`. Publishing the same script again (by its slug) adds a new version, only you can delete your uploads, and clicking an author's ID lists everything they've posted. Export and import it (sealed with a passphrase) in Preferences > Library. Posting anonymously is still an option, except on servers that only take signed uploads.
- The visualizer's script row has Publish... (your scripts) or Update (scripts installed from a library): an update shows what the author changed and, separately, what you changed in your copy; changes that don't overlap can be kept. Restore original undoes your changes to an installed script.
- The bundled scripts are also on the official library, published from the repository whenever they change, so fixes and new versions arrive through Update without waiting for a release. Scripts get the newest version your app can run: each version records the oldest synththing it needs.

### Fixed
- A live recording's "frames repeating" warning blamed the script even when the encoder was the one behind, and didn't count frames skipped for the encoder at all. It now says which.

## 0.4.0 (2026-10-05)

### New
- Recording moved to synththing > Record..., a window to pick what to record (a song from the start, the one playing or any other, and for MIDI its soundfont; the playlist; or free), which script, the video's size, frame rate and quality, and for scripts that can play themselves, Auto or Manual. F9 still starts a free recording at once, and a recording's Pause and Stop stay above the visualizer.
- Scripts can take part: `recording()` says what's being recorded and whether to play themselves, and with `script_options({ record_prepare = true })` a script gets ready before each song (nothing recorded meanwhile), says `recording_ready()` to start the video and `recording_done()` to end each take. Playlists move on only once each take is over.
- `run-script --video` records prepared scripts the same way, and takes `--auto`.
- Vertical (720 x 1280, 1080 x 1920) and square recording sizes for phone-shaped videos, and Custom for any size (also `--video-size vertical`, `square` or `WIDTHxHEIGHT`).
- The Script Settings tab is gone, to leave room: Settings, Controls and Debug buttons above the visualizer each open a window laid out like Preferences. Scripts can sort their settings and controls into groups and explain them on hover: `{ group = "Movement", info = "..." }` as the last argument of `setting_*` and `input_register` (gamepad_demo and settings_demo show it).
- In Controls, each binding is a button: click to remove it, right-click to rebind it in place. + adds one and waits for its key.
- Debug has the script's variables, live: its top-level locals and its own globals, with tables opened one level at a time (50 entries, then more on request), plus its last `debug_locals()`, its log, and how long it takes to draw.
- Scripts get Lua's `coroutine` library, to spread big jobs over frames (the watchdog covers coroutines too).
- highway.lua: big songs are read and charted a bit per frame with a progress line, so the visualizer no longer freezes (a 280,000-note song that used to hit the watchdog charts in a few seconds); recordings start when the game does and end after a few seconds of results (or on leaving them), and in Auto it plays itself (the track last played, else PLAY ALL) without keeping the score, the take ending with the song. Its controls and settings are grouped.
- highway.lua: the track menu plays the song (scroll to seek, Space to pause) and SOLO (I) plays only the selected track, so you can tell which is which. GUITAR mode (G) plays it like Guitar Hero: five frets held and a strum (Up/Down, or a guitar controller's strum bar), with HOPOs and taps that can be fretted without strumming.

## 0.3.3 (2026-10-03)

### New
- An experimental Linux build on each release, `synththing-linux-x86_64`, which updates itself like the Windows one. It builds and passes every test on Ubuntu, but hasn't been played with yet: reports welcome (docs/LINUX.md).

## 0.3.2 (2026-10-03)

### New
- Mouse buttons can be bound to script actions (`"mouse_left"`, `"mouse_right"`, `"mouse_middle"`), and captured in the Controls list by clicking.
- `playback().recording`: whether the visualizer is being recorded, so a script can keep hints or overlays out of the video.
- snake.lua and keyboard.lua take a controller: the d-pad (and the left stick, in snake) steers, or moves the span and key.

## 0.3.1 (2026-10-03)

### New
- `input_value(id)` for scripts: how far an action is pressed, 0 to 1 (keys 0 or 1, triggers and stick directions how far), so analogue steering stays rebindable. gamepad_demo.lua shows it.
- Sprite Editor: line (L) and rectangle (U, Shift for filled) tools, and Flip H / Flip V, which mirror each frame of a sheet in place.

## 0.3.0 (2026-10-03)

### New
- A welcome window the first time synththing runs, offering a starter pack: two soundfonts (GeneralUser GS and TimGM6mb, downloaded in the background and ready to use) and four songs to try, arranged for synththing with each instrument on its own channel. Help > Welcome... brings it back.
- A guided tour (offered by the welcome window, and Help > Tour): it points at the real buttons one at a time, with everything else dimmed, and moves on as you try each thing: playing a song, the controls, soundfonts, visualizers, channels, playlists, a soundfont for one song, fullscreen, and a game.
- Themes (synththing > Themes..., a window of its own that stays on top while you look around the app): Dark (the new default), Light, Frutiger Aero and Programmer art (egui's own look). Duplicate one to make your own: every color, the corner roundness, borders and shadows, changed live. The UI scale is there too, and Ctrl + / Ctrl - / Ctrl 0 change it anywhere.
- A What's new window after an update, listing what changed since the version you had; Help > Changelog... shows every version.

### Changed
- The File menu is now the synththing menu (Preferences, Themes, Quit).
- Preferences is organized into categories (General, Songs, Loading, Scripts, Recording, Experimental), one line per setting; the longer explanations show on hover, wherever there's an (i).
- Only MIDI files are listed as songs by default; Preferences > Songs > "Show audio files" lists MP3, WAV and the rest too (notes-based features don't apply to them).
- The Listening layout (the first one you see) has the songs and soundfonts on the left and the visualizer on the right. Playlists are a View menu click away (the tour opens them).

## 0.2.2 (2026-10-03)

### New
- Visualizer scripts run on their own thread: a slow or stuck script no longer freezes the window, menus or playback. When a frame takes over a second, a notice in the visualizer's corner says so, with a Stop button.
- A watchdog stops a script whose code runs for over 10 seconds without finishing (an endless loop, say), with an error saying so. Restart (F5) or saving a fix runs it again.
- A warning before playing a very large MIDI file (over 100,000 notes), with "don't warn me again"; such files get a red ! in the song lists.
- Script logs can go to the app's log (Help > Log...): a Preferences option for all scripts, `script_options({ app_log = true })` for one script, or `log_app(message)` for single messages.
- `synththing --console` prints the app's log to the terminal as it runs.

### Changed
- Picking another script, applying an edit or restarting stops a script that's been busy for over a second first, instead of waiting for it.

## 0.2.1 (2026-10-03)

### New
- Game controllers: buttons, the d-pad, triggers and stick directions can be bound to a script's actions like keys, as defaults or in the Controls list. `pad_axis()` reads sticks and triggers directly, and `gamepads()` lists what's connected.
- gamepad_demo.lua (an example): a drawn controller lighting up as yours is used.
- An updated highway.lua.

### Changed
- The whole Script Settings tab scrolls, so a long Controls list no longer squeezes the rest.

## 0.2.0 (2026-10-03)

### New
- Scripts can drive playback through the current playlist: `playlist()`, `play_track`, `next_track`, `previous_track`, `set_speed`, `set_loop` and `set_shuffle`. Only songs already in the playlist; volume stays yours.
- `script_options({ start_paused = true })`: songs load paused while that script runs, for games with their own start button. `playback().song_loads` counts every load, the same song again included.
- highway.lua, a Guitar Hero style game charted from any song.
- player.lua template: a mouse-driven player drawn by a script.
- Bundled scripts are grouped into visualizers, games, templates and examples. New visualizers and games arrive with updates; ones you deleted stay deleted.

### Changed
- game.lua is now note_runner.lua.

### Fixed
- Muting or unmuting a channel no longer skips the song (and the visuals) forward.
- A script reading the new song's notes as it loaded could get the previous song's length.

## 0.1.9 (2026-10-03)

### New
- Record the visualizer to video (MP4 with sound): this song, this playlist, or until you stop, at 720p to 4K and 30 or 60 fps, in standard, sharp or lossless quality. F9 starts and stops, F10 pauses. ffmpeg is downloaded on first use. Also from the command line: `run-script --video`.
- The script editor got line numbers, auto-indent, bracket and quote pairing, comment toggling (Ctrl+/), find and replace (Ctrl+F, Ctrl+H), a choice of when edits apply, and a history of earlier versions to restore.
- The editor understands Lua: problems underlined as you type (unknown names, unused locals, accidental globals), completions, signature help, go to definition (F12), an outline, and Format (Shift+Alt+F).
- Color swatches in the editor, with a picker, and links from function names into the reference.
- A Sprite Editor tab: draw a script's sprites and sprite sheets with pencil, fill and picker, and edit their palettes; changes are written back into the script.
- An Insert menu of snippets: color tables, settings, controls, a text field, sequences, a new sprite, and more.
- Layouts: built-in ones (Listening, Watching, Script writing, Sprite editing) and your own saved by name, under View > Layout. A layout keeps your changes, and Reset puts a built-in one back.
- Sprites can be recolored per draw (`palette` swaps, `tint`), and `playback()` has `song_name`, `song_path` and `song_id`.
- The game example script (now note_runner.lua), a platformer whose level is the music.

### Changed
- Visuals line up with what you hear at any audio buffer size, and notes a script plays show up in the waveform, spectrum and levels.

### Fixed
- Audio right after a seek could be dropped, and a song was marked finished slightly before its end was heard.

## 0.1.8 (2026-10-02)

### New
- Sprites for scripts: palette-indexed images, scaled and flipped, and sprite sheets.
- Rebindable controls: scripts register actions with default keys, and the Controls list in Script Settings rebinds them.
- Typing spans for text input in scripts, and a terminal.lua template.
- Scripts can play notes on the song's instruments (`play_note`, `note_on`, sequences). keyboard.lua lets you play along with A to L.
- A render-time readout in Script Settings; a script too slow for 60 fps drops to 30 fps.
- `run-script --screenshot` saves the last frame as a PNG.
- The disco example visualizer.

### Changed
- snake, keyboard, fft and spectrogram use the new features, and pulse.lua is new.
- `active_notes()` includes notes the script plays, marked as such.

## 0.1.7 (2026-10-02)

### New
- Song lengths in the Songs browser and playlists, a playlist total, and an info window per song (tracks, tempo, channels, tags and more).
- Playlists: multi-select, dragging folders in, and .m3u import and export.
- A README, and synththing is licensed under the WTFPL.

## 0.1.6 (2026-10-02)

### New
- Musical timing for scripts: `beat()`, `bar()`, `tempo()`, `time_signature()` and `time_at_beat()`.
- Audio levels and onset (beat) detection for scripts.
- Filled shapes: `circle()`, `triangle()` and `polygon()`.

## 0.1.5 (2026-10-02)

### New
- Pixel text for scripts (the Monogram font): `text()` and `text_size()`.
- Help > Credits & licenses.
- `synththing run-script`: test a script without a window, from the command line.

## 0.1.4 (2026-10-02)

### New
- `notes_between()`: every note in a stretch of the song, with start and end times.
- Scripts can pause and seek, and save their own data (`store_get`, `store_set`).
- `FRAME` and `TIME`, and `playback().generation` to notice seeks and loops.

## 0.1.3 (2026-10-02)

### New
- Loop and Shuffle are remembered between runs.
- The line a script error points at is underlined, with a Go to line button.
- Rename scripts, settings included.
- The Songs and scripts folders refresh by themselves when files change, and a script edited elsewhere reloads.

## 0.1.2 (2026-10-02)

### New
- An app log (Help > Log...), also written to a file, so problems in release builds can be looked into.
- The scripting reference was rewritten and is easier to read.

## 0.1.1 (2026-10-02)

The first release: a MIDI and audio player with soundfonts, playlists, a dockable tab layout, Lua-scripted visualizers with a script editor, mouse and keyboard input for scripts, a dedicated fullscreen visualizer, background loading, and updates from GitHub.
