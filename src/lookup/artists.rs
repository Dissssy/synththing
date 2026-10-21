//! Who's who at MusicBrainz. A name in the library is tied to a
//! MusicBrainz artist where matched songs credit that artist under it (by
//! name, or in its place among the credits), by the artist's own name and
//! aliases (each artist's asked for once), and by the names Clean up's
//! offline Same name rows find spelled like a tied one. Any other name is
//! loose. A tied name spelled otherwise than its artist's own name is
//! offered as that name (a Same name row), never written by itself.

use std::collections::{BTreeMap, BTreeSet};

use super::matching::{Credit, Release, Track};
use super::musicbrainz::VARIOUS_ARTISTS;
use crate::cleanup::{Kind, NAME_FIELDS, Suggestion, What};
use crate::fields::{self, Facts, Field};
use crate::song_store::{Song, SongStore};

/// An artist at MusicBrainz: their id, their own name, and the other
/// names they go by.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Artist {
    pub id: String,
    pub name: String,
    pub aliases: Vec<String>,
}

/// Names (by group key), each with the artist (by id) it's tied to; and
/// each of those artists' own names.
#[derive(Debug, Default, PartialEq)]
pub struct Tied {
    pub names: BTreeMap<String, String>,
    pub own: BTreeMap<String, String>,
}

impl Tied {
    /// From matched songs, each with its release and track (if it's lined
    /// up with one): each name to the artist most of its songs credit
    /// under it.
    pub fn of<'a>(matched: impl IntoIterator<Item = (&'a Song, &'a Release, Option<&'a Track>)>, ignore: &str) -> Self {
        let mut votes: BTreeMap<String, BTreeMap<&str, usize>> = BTreeMap::new();
        let mut own = BTreeMap::new();
        for (song, release, track) in matched {
            let facts = Facts::of(song);
            let mut credited = |field: Field, credits: &'a [Credit]| {
                let items = facts.items(field);
                let credits: Vec<&Credit> = credits.iter().filter(|c| c.id != VARIOUS_ARTISTS).collect();
                let named: Vec<Option<&Credit>> = items
                    .iter()
                    .map(|item| {
                        let key = fields::group_key(item, ignore);
                        credits.iter().copied().find(|c| [&c.credited, &c.name].iter().any(|n| fields::group_key(n, ignore) == key))
                    })
                    .collect();
                for (i, item) in items.iter().enumerate() {
                    // (Else the one in its place, if there are as many and
                    // no other name's that one.)
                    let placed = || (items.len() == credits.len() && !named.iter().flatten().any(|c| c.id == credits[i].id)).then(|| credits[i]);
                    let Some(credit) = named[i].or_else(placed) else { continue };
                    own.insert(credit.id.clone(), credit.name.clone());
                    *votes.entry(fields::group_key(item, ignore)).or_default().entry(&credit.id).or_default() += 1;
                }
            };
            if let Some(track) = track {
                credited(Field::Artist, if track.credits.is_empty() { &release.credits } else { &track.credits });
            }
            credited(Field::AlbumArtist, &release.credits);
        }
        let names = votes.into_iter().filter_map(|(key, ids)| Some((key, ids.into_iter().max_by_key(|(_, n)| *n)?.0.to_string()))).collect();
        Self { names, own }
    }

    /// Tie each of `artists`' own names and aliases to them, those not
    /// tied already and that no other artist goes by.
    pub fn add_aliases<'a>(&mut self, artists: impl IntoIterator<Item = &'a Artist>, ignore: &str) {
        let mut claimed: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
        for artist in artists.into_iter().filter(|a| self.own.contains_key(&a.id)) {
            for name in std::iter::once(&artist.name).chain(&artist.aliases) {
                claimed.entry(fields::group_key(name, ignore)).or_default().insert(&artist.id);
            }
        }
        for (key, ids) in claimed {
            if let [id] = Vec::from_iter(ids).as_slice() {
                self.names.entry(key).or_insert_with(|| id.to_string());
            }
        }
    }

    /// Clean up's Same name rows for `store`'s names, given the offline
    /// ones (`cleanup::suggest`) in `suggestions`: a name spelled like only
    /// one artist's is tied to them too, and that artist's row (as their
    /// own name) takes the place of the offline ones it overlaps.
    pub fn offer(mut self, store: &SongStore, suggestions: &mut Vec<Suggestion>, ignore: &str) {
        let keys = |spellings: &[String]| spellings.iter().map(|s| fields::group_key(s, ignore)).collect::<BTreeSet<_>>();
        for suggestion in suggestions.iter() {
            let What::Name { spellings, artist: None } = &suggestion.what else { continue };
            let keys = keys(spellings);
            let ids: BTreeSet<&String> = keys.iter().filter_map(|k| self.names.get(k)).collect();
            if let [id] = Vec::from_iter(ids).as_slice() {
                let id = id.to_string();
                for key in keys {
                    self.names.entry(key).or_insert_with(|| id.clone());
                }
            }
        }
        let mut found: BTreeMap<&str, (BTreeSet<String>, BTreeSet<String>)> = BTreeMap::new();
        for (key, song) in store.known() {
            let facts = Facts::of(song);
            for field in NAME_FIELDS {
                for item in facts.items(field) {
                    let Some(id) = self.names.get(&fields::group_key(&item, ignore)) else { continue };
                    if self.own.get(id).is_some_and(|own| *own != item) {
                        let (spellings, keys) = found.entry(id).or_default();
                        spellings.insert(item.into_owned());
                        keys.insert(key.to_string());
                    }
                }
            }
        }
        suggestions.retain(|s| !matches!(&s.what, What::Name { spellings, artist: None } if keys(spellings).iter().any(|k| self.names.contains_key(k))));
        suggestions.extend(found.into_iter().map(|(id, (spellings, keys))| Suggestion {
            kind: Kind::Same,
            what: What::Name { spellings: spellings.into_iter().collect(), artist: Some(id.to_string()) },
            to: self.own[id].clone(),
            keys: keys.into_iter().collect(),
        }));
        suggestions.sort_by_key(|s| s.kind);
    }
}

/// The artists (by id) `releases` credit, past Various Artists.
pub fn credited<'a>(releases: impl IntoIterator<Item = &'a Release>) -> BTreeSet<&'a str> {
    releases
        .into_iter()
        .flat_map(|r| r.credits.iter().chain(r.tracks.iter().flat_map(|t| &t.credits)))
        .map(|c| c.id.as_str())
        .filter(|id| *id != VARIOUS_ARTISTS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::song_store::Meta;

    fn credit(id: &str, credited: &str, name: &str) -> Credit {
        Credit { id: id.into(), credited: credited.into(), name: name.into(), sort: name.into() }
    }

    fn song(artist: &str) -> Song {
        Song { name: "a.flac".into(), meta: Some(Meta { title: Some("Days".into()), artist: Some(artist.into()), ..Meta::default() }), ..Song::default() }
    }

    #[test]
    fn names_tie_by_name_or_place() {
        let release = Release { credits: vec![credit("kan", "Kan R. Gao", "Kan Gao")], ..Release::default() };
        let track = Track { credits: vec![credit("kan", "Kan R. Gao", "Kan Gao"), credit("laura", "Laura Shigihara", "Laura Shigihara")], ..Track::default() };
        let (by_name, by_place) = (song("Kan R. Gao; Laura Shigihara"), song("Kan (Reives) Gao; Laura Shigihara"));
        let tied = Tied::of([(&by_name, &release, Some(&track)), (&by_place, &release, Some(&track))], "");
        let id = |name: &str| tied.names.get(&fields::group_key(name, "")).map(String::as_str);
        assert_eq!((id("Kan R. Gao"), id("Kan (Reives) Gao"), id("Laura Shigihara")), (Some("kan"), Some("kan"), Some("laura")));
        assert_eq!(tied.own["kan"], "Kan Gao");
    }

    #[test]
    fn aliases_tie_unless_shared() {
        let mut tied = Tied { own: [("a".into(), "Reol".into()), ("b".into(), "Giga".into())].into(), ..Tied::default() };
        let artists = [
            Artist { id: "a".into(), name: "Reol".into(), aliases: vec!["れをる".into(), "REOL".into(), "Rei".into()] },
            Artist { id: "b".into(), name: "Giga".into(), aliases: vec!["Rei".into()] },
        ];
        tied.add_aliases(&artists, "");
        assert_eq!(tied.names.get(&fields::group_key("れをる", "")).map(String::as_str), Some("a"));
        assert_eq!(tied.names.get(&fields::group_key("Rei", "")), None, "two artists go by it");
    }

    #[test]
    fn tied_names_are_offered_as_their_artists() {
        let dir = std::env::temp_dir().join(format!("synththing-artists-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = SongStore::open(dir);
        let names = ["Kan R. Gao", "Kan (Reives) Gao", "Kan (Reives) Gao", "Kan Gao", "Nujabes"];
        store.learn(names.iter().enumerate().map(|(i, artist)| (format!("{:064x}.ogg", i + 1), song(artist))));
        let mut suggestions = crate::cleanup::suggest(&store, "");
        assert!(suggestions.iter().any(|s| matches!(&s.what, What::Name { artist: None, .. })), "{suggestions:?}");
        let tied = Tied { names: [(fields::group_key("Kan R. Gao", ""), "kan".to_string())].into(), own: [("kan".into(), "Kan Gao".into())].into() };
        tied.offer(&store, &mut suggestions, "");
        let same: Vec<_> = suggestions.iter().filter(|s| s.kind == Kind::Same).collect();
        assert_eq!(same.len(), 1, "{same:?}");
        assert_eq!(same[0].what, What::Name { spellings: vec!["Kan (Reives) Gao".into(), "Kan R. Gao".into()], artist: Some("kan".into()) });
        assert_eq!((same[0].to.as_str(), same[0].keys.len()), ("Kan Gao", 3));
    }
}
