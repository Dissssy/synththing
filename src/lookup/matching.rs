//! Telling whether a release at MusicBrainz is one of the library's
//! albums: its title and artist alike, and every one of the album's songs
//! lined up with one of its tracks (by disc and track number, title and
//! length). Only a release that fits every song, clearly better than any
//! other edition-of-another-album that fits, is taken without asking.

use strsim::normalized_levenshtein;

/// One of the library's albums, as its songs say.
#[derive(Clone, Debug, PartialEq)]
pub struct Album {
    /// What it's known by (`lookup::album_id`).
    pub id: String,
    pub name: String,
    /// Who it's by: its album artist, else its songs' one artist (`None`:
    /// several, a compilation).
    pub artist: Option<String>,
    pub songs: Vec<Song>,
    /// Releases it's been matched to and isn't (Wrong match).
    pub rejected: Vec<String>,
}

/// One of an album's songs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Song {
    pub key: String,
    pub title: String,
    pub artist: Option<String>,
    pub track: Option<u32>,
    pub disc: Option<u32>,
    /// Seconds.
    pub length: Option<f64>,
}

/// A release at MusicBrainz, with its tracks.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Release {
    pub id: String,
    pub group: Option<String>,
    pub title: String,
    /// Its artist credit, as it reads ("Uyama Hiroto & Nujabes").
    pub artist: String,
    pub official: bool,
    /// "2004-07-21", or as much of it as is known.
    pub date: Option<String>,
    /// The Cover Art Archive has its front cover.
    pub front: bool,
    pub tracks: Vec<Track>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Track {
    pub disc: u32,
    pub position: u32,
    pub title: String,
    /// Seconds.
    pub length: Option<f64>,
    pub recording: Option<String>,
}

/// How well a release fits an album.
#[derive(Clone, Debug, PartialEq)]
pub struct Fit {
    /// 0 to 1.
    pub score: f64,
    /// Each song (by index) and the track it's lined up with, if any.
    pub tracks: Vec<Option<usize>>,
}

impl Fit {
    /// Every song is on it.
    pub fn complete(&self) -> bool {
        self.tracks.iter().all(Option::is_some)
    }

    fn lined_up(&self) -> f64 {
        self.tracks.iter().filter(|t| t.is_some()).count() as f64 / self.tracks.len().max(1) as f64
    }
}

/// What a lookup makes of an album.
#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    /// This release (by index), each song's track.
    Match(usize, Vec<Option<usize>>),
    /// One or more might be it, but none surely: a person decides.
    Unsure(Vec<usize>),
    NotFound,
}

/// How alike two names are (0 to 1), past case, spacing, punctuation and
/// a bracketed part one has ("Song (feat. X)" is "Song").
pub fn similar(a: &str, b: &str) -> f64 {
    let (a, b) = (key(a), key(b));
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let whole = normalized_levenshtein(&a, &b);
    let (short, long) = if a.chars().count() <= b.chars().count() { (&a, &b) } else { (&b, &a) };
    // "Days" and "Days (Piano Vers.)" stay apart; "Hollow" in "Hollow - EP" doesn't.
    let within = if short.chars().count() >= 4 && long.starts_with(short.as_str()) { 0.9 } else { 0.0 };
    whole.max(within)
}

/// A name for comparing: without bracketed parts, lowercase, letters and
/// digits only.
fn key(text: &str) -> String {
    let mut depth = 0usize;
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '(' | '[' | '【' | '「' => depth += 1,
            ')' | ']' | '】' | '」' => depth = depth.saturating_sub(1),
            _ if depth == 0 && c.is_alphanumeric() => out.extend(c.to_lowercase()),
            _ => {}
        }
    }
    // A name that's all brackets ("[Untitled]") keeps them.
    if out.is_empty() { text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect() } else { out }
}

/// Two lengths are the same song's: within 3 seconds, or 2% of a long one.
fn same_length(a: f64, b: f64) -> bool {
    (a - b).abs() <= 3.0_f64.max(b * 0.02)
}

/// Whether `release`'s credit is `album`'s artist: alike, or one of its
/// names; any credit does for a compilation.
fn same_artist(album: &Album, release: &Release) -> bool {
    let Some(artist) = &album.artist else {
        return true;
    };
    similar(artist, &release.artist) >= 0.8
        || release
            .artist
            .split([',', '&', ';'])
            .chain(artist.split([',', '&', ';']))
            .any(|name| similar(name, artist) >= 0.9 && similar(name, &release.artist) >= 0.4)
        || key(&release.artist) == "variousartists"
}

/// How well `release` fits `album`: its songs lined up with its tracks,
/// each with the track it's most like that no other song has.
pub fn fit(album: &Album, release: &Release) -> Fit {
    let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
    for (s, song) in album.songs.iter().enumerate() {
        for (t, track) in release.tracks.iter().enumerate() {
            let title = similar(&song.title, &track.title);
            let placed = song.track == Some(track.position) && song.disc.unwrap_or(1) == track.disc;
            let length = match (song.length, track.length) {
                (Some(a), Some(b)) => Some(same_length(a, b)),
                _ => None,
            };
            // A fade or mastering apart (within 15%) is still the song if
            // it's surely named and placed so; further apart, it isn't.
            let near = matches!((song.length, track.length), (Some(a), Some(b)) if (a - b).abs() <= b * 0.15);
            let ok = match length {
                Some(false) => near && title >= 0.95 && placed,
                _ => title >= 0.8 || (placed && title >= 0.5 && length == Some(true)),
            };
            if ok {
                let score = title + if placed { 0.3 } else { 0.0 } + if length == Some(true) { 0.2 } else { 0.0 };
                pairs.push((score, s, t));
            }
        }
    }
    pairs.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut tracks = vec![None; album.songs.len()];
    let mut taken = vec![false; release.tracks.len()];
    for (_, s, t) in pairs {
        if tracks[s].is_none() && !taken[t] {
            tracks[s] = Some(t);
            taken[t] = true;
        }
    }
    let title = similar(&album.name, &release.title);
    let artist = if same_artist(album, release) { 1.0 } else { 0.0 };
    let mut fit = Fit { score: 0.0, tracks };
    fit.score = title * 0.3 + artist * 0.2 + fit.lined_up() * 0.5;
    fit
}

/// Which of `releases` is `album`, if one surely is. Editions of one
/// album (a release group) that fit alike are one answer: the official
/// one with a cover, then the earliest.
pub fn decide(album: &Album, releases: &[Release]) -> Verdict {
    let fits: Vec<Fit> = releases.iter().map(|r| fit(album, r)).collect();
    let good = |i: usize| {
        let r = &releases[i];
        !album.rejected.contains(&r.id) && fits[i].complete() && similar(&album.name, &r.title) >= 0.85 && same_artist(album, r)
    };
    let mut candidates: Vec<usize> = (0..releases.len()).filter(|&i| good(i)).collect();
    if candidates.is_empty() {
        let maybe: Vec<usize> = (0..releases.len()).filter(|&i| !album.rejected.contains(&releases[i].id) && fits[i].lined_up() >= 0.5).collect();
        return if maybe.is_empty() { Verdict::NotFound } else { Verdict::Unsure(maybe) };
    }
    candidates.sort_by(|&a, &b| {
        fits[b].score.total_cmp(&fits[a].score).then_with(|| {
            let rank = |r: &Release| (!r.official, !r.front, r.date.clone().unwrap_or_else(|| "9999".into()));
            rank(&releases[a]).cmp(&rank(&releases[b]))
        })
    });
    let best = candidates[0];
    let group = |i: usize| releases[i].group.as_deref().unwrap_or(&releases[i].id);
    let rivals: Vec<usize> = candidates.iter().copied().filter(|&i| group(i) != group(best) && fits[i].score >= fits[best].score - 0.05).collect();
    if rivals.is_empty() { Verdict::Match(best, fits[best].tracks.clone()) } else { Verdict::Unsure(std::iter::once(best).chain(rivals).collect()) }
}

/// An album's name to search by: without edition words in brackets at
/// its end ("(Deluxe Edition)", "[Remastered 2011]").
pub fn search_name(name: &str) -> &str {
    const EDITIONS: &[&str] = &["deluxe", "edition", "remaster", "expanded", "anniversary", "bonus"];
    let trimmed = name.trim_end();
    if let Some(close) = trimmed.chars().last().filter(|c| matches!(c, ')' | ']'))
        && let Some(open) = trimmed.rfind(if close == ')' { '(' } else { '[' })
        && open > 0
    {
        let inside = trimmed[open..].to_lowercase();
        if EDITIONS.iter().any(|w| inside.contains(w)) {
            return trimmed[..open].trim_end();
        }
    }
    trimmed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(title: &str, track: u32, length: f64) -> Song {
        Song { key: title.into(), title: title.into(), track: Some(track), length: Some(length), ..Song::default() }
    }

    fn release(id: &str, group: &str, title: &str, artist: &str, tracks: &[(&str, f64)]) -> Release {
        Release {
            id: id.into(),
            group: Some(group.into()),
            title: title.into(),
            artist: artist.into(),
            official: true,
            front: true,
            tracks: tracks
                .iter()
                .enumerate()
                .map(|(i, (t, l))| Track { disc: 1, position: i as u32 + 1, title: t.to_string(), length: Some(*l), recording: Some(format!("rec-{t}")) })
                .collect(),
            ..Release::default()
        }
    }

    fn album(songs: Vec<Song>) -> Album {
        Album { id: "a".into(), name: "Modal Soul".into(), artist: Some("Nujabes".into()), songs, rejected: Vec::new() }
    }

    #[test]
    fn names_compare_past_brackets_and_case() {
        assert!(similar("Feather (feat. Cise Starr)", "feather") > 0.99);
        assert!(similar("Modal Soul", "Modal Soul [Deluxe]") > 0.99);
        assert!(similar("Days", "Days (Piano Vers.)") > 0.99, "a bracketed part is set aside");
        assert!(similar("Luv(sic.) Part 3", "Luv (sic) pt. 3") > 0.7);
        assert!(similar("Feather", "Reflection Eternal") < 0.5);
        assert_eq!(search_name("Modal Soul (Deluxe Edition)"), "Modal Soul");
        assert_eq!(search_name("Secrets of Grindea OST [Story Mode]"), "Secrets of Grindea OST [Story Mode]");
    }

    #[test]
    fn a_partial_album_matches_its_release() {
        let ours = album(vec![song("Feather", 1, 175.0), song("Reflection Eternal", 3, 213.0)]);
        let releases = [release(
            "r1",
            "g1",
            "Modal Soul",
            "Nujabes",
            &[("Feather (feat. Cise Starr & Akin)", 176.0), ("Ordinary Joe", 300.0), ("Reflection Eternal", 212.5)],
        )];
        assert_eq!(decide(&ours, &releases), Verdict::Match(0, vec![Some(0), Some(2)]));
    }

    #[test]
    fn editions_of_one_album_are_one_answer_but_two_albums_are_unsure() {
        let ours = album(vec![song("Feather", 1, 175.0)]);
        let tracks = [("Feather", 175.0)];
        let mut reissue = release("r2", "g1", "Modal Soul", "Nujabes", &tracks);
        reissue.date = Some("2010".into());
        let mut first = release("r1", "g1", "Modal Soul", "Nujabes", &tracks);
        first.date = Some("2005".into());
        assert_eq!(decide(&ours, &[reissue.clone(), first.clone()]), Verdict::Match(1, vec![Some(0)]), "the earliest edition");
        let other = release("r3", "g2", "Modal Soul", "Nujabes", &tracks);
        assert_eq!(decide(&ours, &[first.clone(), other]), Verdict::Unsure(vec![0, 1]));
        let rejected = Album { rejected: vec!["r1".into()], ..ours.clone() };
        assert_eq!(decide(&rejected, &[first]), Verdict::NotFound);
    }

    #[test]
    fn a_wrong_length_or_missing_song_isnt_a_match() {
        let ours = album(vec![song("Feather", 1, 175.0), song("Bonus Beat", 9, 100.0)]);
        let releases = [release("r1", "g1", "Modal Soul", "Nujabes", &[("Feather", 175.0)])];
        assert_eq!(decide(&ours, &releases), Verdict::Unsure(vec![0]), "half of it fits");
        let long = album(vec![song("Feather", 1, 400.0)]);
        assert_eq!(decide(&long, &releases), Verdict::NotFound, "same title, another song");
        let elsewhere = Album { name: "Spiritual State".into(), ..album(vec![song("Feather", 1, 175.0)]) };
        assert_eq!(decide(&elsewhere, &releases), Verdict::Unsure(vec![0]), "another album's song");
    }
}
