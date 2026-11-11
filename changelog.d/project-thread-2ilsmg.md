### Changed
- Hub: it always keeps 1 GB of its disk free for its other data and stops wanting songs once it's full, so your devices keep their new songs instead of sending them around it through the relay. Its log says when it fills up and when it has room again.
- Local API: adding or staging a song that would leave under 1 GB free on the library's disk answers 507 instead of failing partway.

### Fixed
- A disk that runs out of space no longer empties the settings, playlists, wishlist or sync state being saved at that moment.
