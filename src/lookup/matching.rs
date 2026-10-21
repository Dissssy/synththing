//! Telling whether a release at MusicBrainz is one of the library's
//! albums: its title and artist alike, and most of the album's songs
//! (`MOSTLY`) lined up with its tracks (by disc and track number, title
//! and length); the rest can be another edition's. Only a release that
//! fits clearly better than any edition of another album that fits is
//! taken without asking.

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
    /// It's a song on no album (its one song), looked up by its recording.
    pub loose: bool,
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

impl Album {
    /// How many songs it has, counting one that's in twice once.
    pub fn distinct_songs(&self) -> usize {
        (0..self.songs.len()).filter(|&s| !(0..s).any(|o| self.songs[o].same_as(&self.songs[s]))).count()
    }
}

impl Song {
    /// It's `other` again: named and as long alike.
    fn same_as(&self, other: &Song) -> bool {
        key(&self.title) == key(&other.title)
            && match (self.length, other.length) {
                (Some(a), Some(b)) => (a - b).abs() <= 1.0,
                (a, b) => a.is_none() && b.is_none(),
            }
    }
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
    /// Who it's credited to, each by name.
    pub credits: Vec<Credit>,
    /// When the first of its group came out: "1975", or a whole date.
    pub first_released: Option<String>,
    /// Its genres, those voted for most first.
    pub genres: Vec<String>,
    /// It's a compilation (or by Various Artists).
    pub compilation: bool,
    /// How many discs it's on.
    pub discs: u32,
}

/// An artist credited: who (their MusicBrainz id), the name they're
/// credited as there, their own name, and what it sorts as.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Credit {
    pub id: String,
    pub credited: String,
    pub name: String,
    pub sort: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Track {
    pub disc: u32,
    pub position: u32,
    pub title: String,
    /// Seconds.
    pub length: Option<f64>,
    pub recording: Option<String>,
    /// Who it's credited to, if not its release's credits.
    pub credits: Vec<Credit>,
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

/// Whether `release`'s credit is `album`'s artist, or one of its songs'
/// (an album artist can be a label: "Level-5"): alike, or one of the
/// names in it; any credit does for a compilation.
fn same_artist(album: &Album, release: &Release) -> bool {
    let Some(artist) = &album.artist else {
        return true;
    };
    let credited: Vec<&str> = release.artist.split([',', '&', ';', '/']).map(str::trim).filter(|n| !n.is_empty()).collect();
    let ours = std::iter::once(artist.as_str()).chain(album.songs.iter().filter_map(|s| s.artist.as_deref()));
    key(&release.artist) == "variousartists"
        || ours
            .flat_map(|a| std::iter::once(a).chain(a.split([',', '&', ';', '/'])))
            .any(|name| similar(name, &release.artist) >= 0.8 || credited.iter().any(|c| similar(name.trim(), c) >= 0.9))
}

/// A song's title as a track would have it: without a file's extension
/// ("Tempest.mp3"), a leading track number ("27. Luke") or the album's
/// name before it ("X-Morph Defense - 12 Dark Hour").
fn track_title<'t>(title: &'t str, album: &str) -> &'t str {
    let mut title = title.trim();
    for ext in [".mp3", ".flac", ".ogg", ".wav", ".m4a", ".opus"] {
        if title.len() > ext.len() && title.is_char_boundary(title.len() - ext.len()) && title[title.len() - ext.len()..].eq_ignore_ascii_case(ext) {
            title = &title[..title.len() - ext.len()];
        }
    }
    let album = search_name(album);
    let mut prefixed = false;
    if let Some(rest) = title.get(..album.len()).filter(|start| start.eq_ignore_ascii_case(&album)).map(|_| &title[album.len()..])
        && rest.starts_with([' ', '-', ':', '–'])
    {
        title = rest.trim_start_matches([' ', '-', ':', '–']);
        prefixed = true;
    }
    // ("12 Dark Hour" after the album's name; "4 PM" by itself is a title.)
    let digits = title.len() - title.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if (1..=3).contains(&digits) {
        let rest = &title[digits..];
        let marked = rest.trim_start().starts_with(['.', '-', '_', ')']);
        let unnumbered = rest.trim_start_matches(['.', ' ', '-', '_', ')']);
        if (marked || (prefixed && rest.starts_with(' '))) && !unnumbered.is_empty() {
            title = unnumbered;
        }
    }
    title
}

/// How well `release` fits `album`: its songs lined up with its tracks,
/// each with the track it's most like that no other song has.
pub fn fit(album: &Album, release: &Release) -> Fit {
    let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
    for (s, song) in album.songs.iter().enumerate() {
        for (t, track) in release.tracks.iter().enumerate() {
            let title = similar(&song.title, &track.title).max(similar(track_title(&song.title, &album.name), &track.title));
            // (A song that doesn't say its disc can be on any.)
            let placed = song.track == Some(track.position) && song.disc.is_none_or(|d| d == track.disc);
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
    // A song that's in the library twice is the same track both times.
    for s in 0..tracks.len() {
        if tracks[s].is_none() {
            tracks[s] = (0..tracks.len()).filter(|&o| o != s && album.songs[o].same_as(&album.songs[s])).find_map(|o| tracks[o]);
        }
    }
    let title = similar_album(&album.name, &release.title);
    let artist = if same_artist(album, release) { 1.0 } else { 0.0 };
    let mut fit = Fit { score: 0.0, tracks };
    fit.score = title * 0.3 + artist * 0.2 + fit.lined_up() * 0.5;
    fit
}

/// Whether `release` is surely `album` though its artist's credited
/// otherwise ("きくお" for "Kikuo"): named alike, and every one of five
/// songs or more on it.
fn named_otherwise(album: &Album, release: &Release, fit: &Fit) -> bool {
    album.songs.len() >= 5 && fit.complete() && similar_album(&album.name, &release.title) >= 0.95
}

/// How many of an album's songs (at least) must be on a release for it to
/// be the album.
const MOSTLY: f64 = 0.75;

/// Which of `releases` is `album`, if one surely is. Editions of one
/// album (a release group) that fit alike are one answer: the official
/// one with a cover, then the earliest.
pub fn decide(album: &Album, releases: &[Release]) -> Verdict {
    let fits: Vec<Fit> = releases.iter().map(|r| fit(album, r)).collect();
    // Most of its songs on it: the rest can be from another edition
    // (bonus tracks, a single version, a live one).
    let good = |i: usize| {
        let r = &releases[i];
        !album.rejected.contains(&r.id)
            && fits[i].lined_up() >= MOSTLY
            && similar_album(&album.name, &r.title) >= 0.85
            && (same_artist(album, r) || named_otherwise(album, r, &fits[i]))
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
    let rivals: Vec<usize> = candidates.iter().copied().filter(|&i| group(i) != group(best) && fits[i].score >= fits[best].score - 0.02).collect();
    if rivals.is_empty() { Verdict::Match(best, fits[best].tracks.clone()) } else { Verdict::Unsure(std::iter::once(best).chain(rivals).collect()) }
}

/// A recording a search for a song found, with the releases it's on.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hit {
    pub recording: String,
    pub title: String,
    /// Each artist credited.
    pub artists: Vec<String>,
    /// Seconds.
    pub length: Option<f64>,
    pub releases: Vec<HitRelease>,
}

/// A release a recording's on, as a search tells of it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HitRelease {
    pub id: String,
    pub official: bool,
    /// What it is, those to take first first: an album (0), a single (1),
    /// an EP (2), anything else (3), a compilation (4).
    pub kind: u8,
    pub date: Option<String>,
}

/// Words that tell one version of a song from another.
const VERSIONS: [&str; 10] = ["live", "remix", "mix", "acoustic", "instrumental", "demo", "edit", "cover", "karaoke", "version"];

fn versions(title: &str) -> Vec<&'static str> {
    let lower = title.to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).collect();
    VERSIONS.iter().copied().filter(|v| words.contains(v)).collect()
}

/// Which of `hits` a song on no album is, and the release to take it
/// from: one named the same (the same version), by its artist, as long
/// as it, on the first of its albums (else singles, EPs) that's
/// official. Stricter than an album's songs, which have each other.
pub fn pick_recording(song: &Song, hits: &[Hit]) -> Option<(usize, String)> {
    let artist = song.artist.as_deref()?;
    let ours: Vec<&str> = artist.split([',', '&', ';', '/']).map(str::trim).filter(|n| !n.is_empty()).collect();
    let fits = |hit: &Hit| {
        similar(&song.title, &hit.title) >= 0.95
            && versions(&song.title) == versions(&hit.title)
            && hit.artists.iter().any(|a| ours.iter().any(|o| similar(o, a) >= 0.9))
            && match (song.length, hit.length) {
                (Some(a), Some(b)) => same_length(a, b),
                _ => true,
            }
    };
    hits.iter()
        .enumerate()
        .filter(|(_, hit)| fits(hit))
        .flat_map(|(i, hit)| hit.releases.iter().map(move |r| (i, r)))
        .min_by_key(|(_, r)| (r.kind, !r.official, r.date.clone().unwrap_or_else(|| "9999".into())))
        .map(|(i, r)| (i, r.id.clone()))
}

/// An album's name to search by: without its bracketed parts ("(Deluxe
/// Edition)", "<OST>"), and what a soundtrack's called besides its game
/// or film ("Original Soundtrack"); "<A Bird Story> OST" is "A Bird
/// Story".
pub fn search_name(name: &str) -> String {
    let unbracketed = without_brackets(name);
    let trimmed = without_extras(&unbracketed);
    if trimmed.chars().filter(|c| c.is_alphanumeric()).count() >= 2 {
        return trimmed;
    }
    let opened: String = name.chars().map(|c| if is_bracket(c) { ' ' } else { c }).collect();
    let trimmed = without_extras(&opened);
    if trimmed.is_empty() { name.trim().to_string() } else { trimmed }
}

fn is_bracket(c: char) -> bool {
    matches!(c, '(' | ')' | '[' | ']' | '<' | '>' | '【' | '】' | '「' | '」' | '〈' | '〉')
}

/// `text` without its bracketed parts, its spacing tidied.
fn without_brackets(text: &str) -> String {
    let mut depth = 0usize;
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '(' | '[' | '<' | '【' | '「' | '〈' => depth += 1,
            ')' | ']' | '>' | '】' | '」' | '〉' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `name` without what follows a soundtrack's name ("- Original Game
/// Soundtrack", "OST") or a single's ("- Single").
fn without_extras(name: &str) -> String {
    const EXTRAS: &[&str] = &[
        "original game soundtrack",
        "original soundtrack",
        "original series soundtrack",
        "official soundtrack",
        "official sountrack",
        "game soundtrack",
        "soundtrack",
        "ost",
        "single",
        "ep",
    ];
    let separators: &[char] = &[' ', '-', '–', '—', ':', ',', '~'];
    let mut name = name.trim_matches(separators).to_string();
    loop {
        let lower = name.to_lowercase();
        let Some(extra) = EXTRAS.iter().find(|e| lower.ends_with(*e) && !lower[..lower.len() - e.len()].ends_with(|c: char| c.is_alphanumeric())) else {
            return name;
        };
        name = name[..name.len() - extra.len()].trim_end_matches(separators).to_string();
    }
}

/// How alike two albums' names are: as they are, or past their bracketed
/// parts and what a soundtrack's called besides ("Finding Paradise <OST>"
/// is "Finding Paradise (Original Soundtrack)").
pub fn similar_album(a: &str, b: &str) -> f64 {
    similar(a, b).max(similar(&search_name(a), &search_name(b)))
}

/// The first of several names ("Kazumi Totaka; Manaka Kataoka"), to search by.
pub fn first_name(names: &str) -> &str {
    names.split([';', ',', '&', '/']).map(str::trim).find(|n| !n.is_empty()).unwrap_or(names)
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
                .map(|(i, (t, l))| Track {
                    disc: 1,
                    position: i as u32 + 1,
                    title: t.to_string(),
                    length: Some(*l),
                    recording: Some(format!("rec-{t}")),
                    ..Track::default()
                })
                .collect(),
            ..Release::default()
        }
    }

    fn album(songs: Vec<Song>) -> Album {
        Album { id: "a".into(), name: "Modal Soul".into(), artist: Some("Nujabes".into()), songs, rejected: Vec::new(), loose: false }
    }

    #[test]
    fn names_compare_past_brackets_and_case() {
        assert!(similar("Feather (feat. Cise Starr)", "feather") > 0.99);
        assert!(similar("Modal Soul", "Modal Soul [Deluxe]") > 0.99);
        assert!(similar("Days", "Days (Piano Vers.)") > 0.99, "a bracketed part is set aside");
        assert!(similar("Luv(sic.) Part 3", "Luv (sic) pt. 3") > 0.7);
        assert!(similar("Feather", "Reflection Eternal") < 0.5);
        assert_eq!(search_name("Modal Soul (Deluxe Edition)"), "Modal Soul");
        assert_eq!(search_name("Secrets of Grindea Original Soundtrack [Story Mode]"), "Secrets of Grindea");
        assert_eq!(search_name("<A Bird Story> OST"), "A Bird Story");
        assert_eq!(search_name("Finding Paradise <OST>"), "Finding Paradise");
        assert_eq!(search_name("Paradise Lost - Official Sountrack"), "Paradise Lost");
        assert_eq!(search_name("No Wonder I - Single"), "No Wonder I");
        assert_eq!(search_name("Hydeout Productions (First Collection)"), "Hydeout Productions");
        assert_eq!(search_name("Ghost"), "Ghost", "not an \"ost\" at its end");
        assert_eq!(search_name("II"), "II");
        assert!(similar_album("To the Moon <OST>", "To the Moon (Original Soundtrack)") > 0.99);
        assert_eq!(first_name("Kazumi Totaka; Manaka Kataoka"), "Kazumi Totaka");
        assert_eq!(first_name("Tsutchie / Force of Nature"), "Tsutchie");
        assert_eq!(track_title("27. Luke", "Grindea"), "Luke");
        assert_eq!(track_title("29. Tempest.mp3", "A Bird Story"), "Tempest");
        assert_eq!(track_title("X-Morph Defense - 12 Dark Hour", "X-Morph: Defense OST"), "X-Morph Defense - 12 Dark Hour", "punctuated otherwise");
        assert_eq!(track_title("X-Morph Defense - 12 Dark Hour", "X-Morph Defense"), "Dark Hour");
        assert_eq!(track_title("1999", "Prince"), "1999", "a title that's a number");
        assert_eq!(track_title("4 PM", "Animal Crossing"), "4 PM");
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
    fn songs_without_discs_or_in_twice_still_fit() {
        let mut remix = song("Rhayader (NEW remixed 2023)", 2, 190.0);
        remix.track = Some(2);
        let ours = album(vec![song("Rhayader", 2, 182.0), song("Rhayader", 2, 182.0), remix]);
        let mut release = release("r1", "g1", "Modal Soul", "Nujabes", &[("The Great Marsh", 124.0), ("Rhayader", 182.5)]);
        release.tracks.push(Track { disc: 2, position: 2, title: "Rhayader (2023 remix)".into(), length: Some(190.5), recording: None, ..Track::default() });
        assert_eq!(decide(&ours, &[release]), Verdict::Match(0, vec![Some(1), Some(1), Some(2)]));
    }

    #[test]
    fn most_songs_on_it_and_a_song_artist_credited_do() {
        let mut ours = album((1..=4).map(|n| song(&format!("Song {n}"), n, 100.0 + f64::from(n) * 20.0)).collect());
        ours.artist = Some("Level-5".into());
        for s in &mut ours.songs {
            s.artist = Some("Tomohito Nishiura".into());
        }
        ours.songs.push(song("Bonus (live)", 9, 300.0));
        let tracks: Vec<(String, f64)> = (1..=4).map(|n| (format!("Song {n}"), 100.0 + f64::from(n) * 20.0)).collect();
        let tracks: Vec<(&str, f64)> = tracks.iter().map(|(t, l)| (t.as_str(), *l)).collect();
        let releases = [release("r1", "g1", "Modal Soul", "Tomohito Nishiura", &tracks)];
        assert_eq!(decide(&ours, &releases), Verdict::Match(0, vec![Some(0), Some(1), Some(2), Some(3), None]));
    }

    #[test]
    fn a_whole_album_credited_otherwise_is_it() {
        let names = ["夜想天鐘", "妖精の霧", "水の神殿", "迷子の廃園", "暗叫"];
        let mut ours = album(names.iter().zip(1..).map(|(t, n)| song(t, n, 100.0 + f64::from(n))).collect());
        ours.artist = Some("Kikuo Sound Works".into());
        let tracks: Vec<(&str, f64)> = names.iter().zip(1..).map(|(t, n)| (*t, 100.0 + f64::from(n))).collect();
        let releases = [release("r1", "g1", "Modal Soul", "きくお", &tracks)];
        assert!(matches!(decide(&ours, &releases), Verdict::Match(0, _)));
        ours.songs.truncate(2);
        assert!(matches!(decide(&ours, &releases), Verdict::Unsure(_)), "too few songs to be sure");
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

    #[test]
    fn a_loose_song_is_its_album_version() {
        let ours = Song { key: "k".into(), title: "Feather".into(), artist: Some("Nujabes; Cise Starr".into()), length: Some(175.0), ..Song::default() };
        let release = |id: &str, kind, date: &str| HitRelease { id: id.into(), official: true, kind, date: Some(date.into()) };
        let hit = |recording: &str, title: &str, length| Hit {
            recording: recording.into(),
            title: title.into(),
            artists: vec!["Nujabes".into(), "Cise Starr".into()],
            length: Some(length),
            releases: vec![release(&format!("{recording}-best-of"), 4, "2010"), release(&format!("{recording}-album"), 0, "2005")],
        };
        let hits = [hit("live", "Feather (live)", 176.0), hit("long", "Feather", 240.0), hit("c1", "Feather", 176.5)];
        assert_eq!(pick_recording(&ours, &hits), Some((2, "c1-album".to_string())));
        assert_eq!(pick_recording(&Song { artist: Some("Someone Else".into()), ..ours.clone() }, &hits), None);
        assert_eq!(pick_recording(&Song { artist: None, ..ours }, &hits), None, "no artist: too loose to tell");
    }
}
