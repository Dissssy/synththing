//! What MusicBrainz says of a matched song, from its release (as the
//! lookup kept it): its fields as MusicBrainz has them, those the song
//! has blank to fill in, and those it has otherwise to offer in Clean up
//! (`cleanup::from_musicbrainz`). A song whose length is far from its
//! track's is flagged there too: likely another version, or a bad rip.

use std::collections::BTreeMap;

use super::matching::{Credit, Release, Track};
use crate::fields::{self, Facts, Field};
use crate::smart::VARIOUS_ARTISTS;
use crate::song_store::SongStore;

/// How far a song's length may be from its track's before it's flagged
/// (seconds).
pub const LENGTH_LEEWAY: f64 = 10.0;
/// How many of a release's genres (those voted for most) a song gets.
const GENRES: usize = 3;

/// The library's genres, by group key: MusicBrainz's are written as the
/// library writes them ("Hip-Hop" for "hip hop").
pub struct Genres(BTreeMap<String, String>);

impl Genres {
    pub fn of(store: &SongStore, ignore: &str) -> Self {
        let mut spellings: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (_, song) in store.known() {
            for genre in Facts::of(song).items(Field::Genre) {
                spellings.entry(fields::group_key(&genre, ignore)).or_default().push(genre.into_owned());
            }
        }
        Self(spellings.into_iter().filter_map(|(key, names)| Some((key, fields::best_spelling(names.iter().map(String::as_str))?.to_string()))).collect())
    }

    fn named(&self, genre: &str, ignore: &str) -> String {
        self.0.get(&fields::group_key(genre, ignore)).cloned().unwrap_or_else(|| genre.to_string())
    }
}

/// The track `recording` is on `release`, if it's there.
pub fn track<'r>(release: &'r Release, recording: Option<&str>) -> Option<&'r Track> {
    let recording = recording?;
    release.tracks.iter().find(|t| t.recording.as_deref() == Some(recording))
}

/// A song's fields as `release` has them (and its `track`, if the song's
/// lined up with one).
pub fn said(release: &Release, track: Option<&Track>, genres: &Genres, ignore: &str) -> Vec<(Field, String)> {
    let mut said = Vec::new();
    let mut credited = |credits: &[Credit], field: Field, sort: Field| {
        if credits.is_empty() || credits.iter().any(|c| c.name == VARIOUS_ARTISTS) {
            return;
        }
        let def = field.def();
        let names: Vec<&str> = credits.iter().map(|c| c.name.as_str()).collect();
        let sorts: Vec<&str> = credits.iter().map(|c| c.sort.as_str()).collect();
        said.push((field, def.join(&names)));
        if names != sorts && sorts.iter().all(|s| !s.is_empty()) {
            said.push((sort, def.join(&sorts)));
        }
    };
    if let Some(track) = track {
        let credits = if track.credits.is_empty() { &release.credits } else { &track.credits };
        credited(credits, Field::Artist, Field::SortArtist);
    }
    credited(&release.credits, Field::AlbumArtist, Field::SortAlbumArtist);
    if let Some(track) = track {
        said.push((Field::Title, track.title.clone()));
        said.push((Field::Track, track.position.to_string()));
        if release.discs > 1 {
            said.push((Field::Disc, track.disc.to_string()));
        }
    }
    said.push((Field::Album, release.title.clone()));
    let year = release.first_released.as_deref().or(release.date.as_deref()).and_then(|d| d.get(..4)).filter(|y| y.chars().all(|c| c.is_ascii_digit()));
    if let Some(year) = year {
        said.push((Field::Year, year.to_string()));
    }
    if !release.genres.is_empty() {
        let mut named: Vec<String> = Vec::new();
        for genre in release.genres.iter().take(GENRES) {
            let genre = genres.named(genre, ignore);
            if !named.contains(&genre) {
                named.push(genre);
            }
        }
        said.push((Field::Genre, Field::Genre.def().join(&named)));
    }
    if release.compilation {
        said.push((Field::Compilation, "1".to_string()));
    }
    said
}

/// What a song (`facts`) makes of what MusicBrainz `said`.
#[derive(Debug, Default, PartialEq)]
pub struct Compared {
    /// Fields it has blank, and what to fill them with.
    pub blank: Vec<(Field, String)>,
    /// Fields it has otherwise: what it has, and what MusicBrainz says
    /// (a genre's is what it has, with MusicBrainz's added).
    pub differs: Vec<(Field, String, String)>,
}

pub fn compare(facts: &Facts, said: &[(Field, String)], ignore: &str) -> Compared {
    let mut compared = Compared::default();
    for (field, to) in said {
        let field = *field;
        match field.kind() {
            fields::Kind::Number => match facts.number(field) {
                None => compared.blank.push((field, to.clone())),
                Some(n) if (n as i64).to_string() != *to => compared.differs.push((field, (n as i64).to_string(), to.clone())),
                Some(_) => {}
            },
            fields::Kind::Flag => {
                if !facts.flag(field) {
                    compared.blank.push((field, to.clone()));
                }
            }
            _ => {
                let Some(now) = facts.text(field) else {
                    compared.blank.push((field, to.clone()));
                    continue;
                };
                // (What a name sorts as is only filled in.)
                if matches!(field, Field::SortArtist | Field::SortAlbumArtist) {
                    continue;
                }
                let def = field.def();
                let keys = |text: &str| def.split(text).map(|i| fields::group_key(i, ignore)).collect::<Vec<_>>();
                let (has, says) = (keys(&now), keys(to));
                if field == Field::Genre {
                    let more: Vec<&str> = def.split(to).filter(|g| !has.contains(&fields::group_key(g, ignore))).collect();
                    if !more.is_empty() {
                        let mut all: Vec<&str> = def.split(&now).collect();
                        all.extend(more);
                        compared.differs.push((field, now.to_string(), def.join(&all)));
                    }
                } else if has.len() != says.len() || !says.iter().all(|k| has.contains(k)) {
                    compared.differs.push((field, now.to_string(), to.clone()));
                }
            }
        }
    }
    compared
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::song_store::{Meta, Song};

    fn release() -> Release {
        let credit = |name: &str, sort: &str| Credit { name: name.into(), sort: sort.into() };
        Release {
            id: "r1".into(),
            title: "Modal Soul".into(),
            credits: vec![credit("Nujabes", "Nujabes")],
            first_released: Some("2005-11-11".into()),
            genres: vec!["hip hop".into(), "jazz".into()],
            discs: 1,
            tracks: vec![Track {
                disc: 1,
                position: 1,
                title: "Feather".into(),
                length: Some(175.0),
                recording: Some("c1".into()),
                credits: vec![credit("Nujabes", "Nujabes"), credit("Cise Starr", "Starr, Cise")],
            }],
            ..Release::default()
        }
    }

    #[test]
    fn blanks_fill_and_others_are_offered() {
        let release = release();
        let genres = Genres([("hiphop".to_string(), "Hip-Hop".to_string())].into());
        let said = said(&release, track(&release, Some("c1")), &genres, "");
        let meta = Meta { title: Some("feather".into()), artist: Some("Nujabes".into()), year: Some(2006), genre: Some("Jazz".into()), ..Meta::default() };
        let song = Song { name: "Feather.flac".into(), meta: Some(meta), ..Song::default() };
        let compared = compare(&Facts::of(&song), &said, "");
        let fields = |list: &[(Field, String)]| list.iter().map(|(f, _)| *f).collect::<Vec<_>>();
        assert_eq!(fields(&compared.blank), [Field::SortArtist, Field::AlbumArtist, Field::Track, Field::Album]);
        assert!(compared.blank.contains(&(Field::SortArtist, "Nujabes; Starr, Cise".into())), "{:?}", compared.blank);
        assert_eq!(
            compared.differs,
            [
                (Field::Artist, "Nujabes".into(), "Nujabes; Cise Starr".into()),
                (Field::Year, "2006".into(), "2005".into()),
                (Field::Genre, "Jazz".into(), "Jazz, Hip-Hop".into()),
            ],
            "a title only cased otherwise isn't offered"
        );
    }
}
