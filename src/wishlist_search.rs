//! The Wishlist's search: `key:value` filters (`kind:album`, `got:no`,
//! `type:ep`, `artist:"jyocho"`, `year:2010..2019`, `added:30d`,
//! `-note:willy`), all of which a wish has to match, and the rest of the
//! query the text searched for in its title, artist and note. Values and
//! text are matched as the Songs search matches them (`search.rs`): fuzzy
//! as typed, closely in double quotes, exactly after a caret.

use std::ops::Range;

use crate::matching::{Mode, Term};
use crate::search::{self, Completion, Problem};
use crate::wishlist::{Kind, State, Wish, KINDS};

/// The filters, for the search box's hover text.
pub const HELP: &str = "Filter your wishes by words (fuzzy), \"a phrase\" or ^\"Exact Text\" in their title, artist or note.\n\
    Filter with key:value, - in front to exclude:\n\
    kind:album|song|artist|text  got:no|partly|yes  type:ep|single|compilation|live|other\n\
    title: artist: note: addedby:  (fuzzy, \"close\" or ^\"exact\")\n\
    year:2014  year:2010..2019  year:>2015\n\
    added:30d (12h, 2w, 6mo, 1y), added:2026-10 (that month)\n\
    Tab completes, Alt switches it between fuzzy, close and exact.";

/// The keys filters go by.
const KEYS: &[&str] = &["kind", "got", "type", "title", "artist", "note", "addedby", "year", "added"];

/// What `got:` can say, and the state it is.
const GOT: &[(&str, State)] = &[("no", State::Wanted), ("partly", State::Partly), ("yes", State::Got)];

/// What `kind:` can say, and the kind it is.
const WISH_KINDS: &[(&str, Kind)] = &[("album", Kind::Album), ("song", Kind::Song), ("artist", Kind::Artist), ("text", Kind::Text)];

/// A wish's text a filter looks in.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Text {
    Title,
    Artist,
    Note,
    AddedBy,
}

#[derive(Debug, PartialEq)]
enum Test {
    Kind(Kind),
    Got(State),
    /// Its release type (an album's), lowercase.
    Type(String),
    Text(Text, Term),
    /// From and to, both in.
    Year(f64, f64),
    /// Added in the last this many seconds.
    AddedSince(f64),
    /// Added from and to (Unix seconds, the end not in).
    AddedIn(i64, i64),
}

/// A query: its filters (each, and whether it's excluded), the text left
/// to search for, and the terms that look like filters but aren't.
#[derive(Debug, Default, PartialEq)]
pub struct Filter {
    tests: Vec<(bool, Test)>,
    text: Vec<Term>,
    pub problems: Vec<Problem>,
}

impl Filter {
    pub fn parse(query: &str) -> Filter {
        let mut filter = Filter::default();
        for (at, term) in search::terms(query) {
            match test(term) {
                Ok(test) => filter.tests.push(test),
                // (A filter still being typed, `kind:`: nothing yet.)
                Err(Some(why)) if why.is_empty() => {}
                Err(why) => {
                    if let Some(why) = why {
                        filter.problems.push(Problem { at: at..at + term.len(), why });
                    }
                    let (mode, text) = search::value(term);
                    if !text.is_empty() {
                        filter.text.push(Term { text: text.to_string(), mode });
                    }
                }
            }
        }
        filter
    }

    pub fn is_empty(&self) -> bool {
        self.tests.is_empty() && self.text.is_empty()
    }

    /// Whether `wish`, `state` as it is here, is kept, `now` (Unix
    /// seconds) saying when "the last 30 days" are.
    pub fn keeps(&self, wish: &Wish, state: State, now: i64) -> bool {
        let text = |which: Text| match which {
            Text::Title => Some(wish.title.as_str()),
            // (An artist's own name is theirs, its detail beside it.)
            Text::Artist if wish.kind == Kind::Artist => Some(wish.title.as_str()),
            Text::Artist => wish.artist.as_deref(),
            Text::Note => Some(wish.note.as_str()).filter(|n| !n.is_empty()),
            Text::AddedBy => wish.added_by.as_deref(),
        };
        let passes = |test: &Test| match test {
            Test::Kind(kind) => wish.kind == *kind,
            Test::Got(got) => state == *got,
            Test::Type(kind) => wish.release_type.as_deref().is_some_and(|t| t.eq_ignore_ascii_case(kind)),
            Test::Text(which, term) => text(*which).is_some_and(|t| term.find(t).is_some()),
            Test::Year(from, to) => wish.year.as_deref().and_then(|y| y.parse::<f64>().ok()).is_some_and(|y| (*from..=*to).contains(&y)),
            Test::AddedSince(age) => wish.added as f64 >= now as f64 - age,
            Test::AddedIn(start, end) => (*start..*end).contains(&wish.added),
        };
        let words = [Text::Title, Text::Artist, Text::Note];
        self.tests.iter().all(|(not, test)| passes(test) != *not)
            && self.text.iter().all(|term| words.iter().any(|w| text(*w).is_some_and(|t| term.find(t).is_some())))
    }
}

/// The test a term is: `Err(None)` if it isn't a filter, `Err(why)` if
/// it looks like one but isn't (why empty: it has no value yet).
fn test(term: &str) -> Result<(bool, Test), Option<String>> {
    let (not, term) = term.strip_prefix('-').map_or((false, term), |t| (true, t));
    let Some((key, raw)) = term.split_once(':') else { return Err(None) };
    let key = key.to_lowercase();
    if !KEYS.contains(&key.as_str()) {
        return Err(None);
    }
    let (mode, value) = search::value(raw);
    if value.is_empty() {
        return Err(Some(String::new()));
    }
    let bad = |why: &str| Err(Some(why.to_string()));
    let test = match key.as_str() {
        "kind" => match WISH_KINDS.iter().find(|(v, _)| v.eq_ignore_ascii_case(value)) {
            Some((_, kind)) => Test::Kind(*kind),
            None => return bad("kind: can be album, song, artist or text"),
        },
        "got" => match GOT.iter().find(|(v, _)| v.eq_ignore_ascii_case(value)) {
            Some((_, state)) => Test::Got(*state),
            None => return bad("got: can be no, partly or yes"),
        },
        "type" => match KINDS.iter().find(|k| k.eq_ignore_ascii_case(value)) {
            Some(kind) => Test::Type(kind.to_lowercase()),
            None => return bad("type: can be album, ep, single, compilation, live or other"),
        },
        "title" => Test::Text(Text::Title, Term { text: value.to_string(), mode }),
        "artist" => Test::Text(Text::Artist, Term { text: value.to_string(), mode }),
        "note" => Test::Text(Text::Note, Term { text: value.to_string(), mode }),
        "addedby" => Test::Text(Text::AddedBy, Term { text: value.to_string(), mode }),
        "year" => match years(value) {
            Some((from, to)) => Test::Year(from, to),
            None => return bad("year: takes 2014, a range (2010..2019, 2015.., ..2009) or >2015, <2015, >=2015, <=2015"),
        },
        "added" => match (crate::smart::parse_period(value), crate::smart::parse_age(value)) {
            (Some((start, end)), _) => Test::AddedIn(start, end),
            (None, Some(age)) => Test::AddedSince(age),
            _ => return bad("added: takes 30d, 2w, 6mo, 1y, or a year, month or day: 2026, 2026-10, 2026-10-20"),
        },
        _ => unreachable!("a key"),
    };
    Ok((not, test))
}

/// The years `value` says, from and to: `2014`, `2010..2019`, `2015..`,
/// `..2009`, `>2015`, `<2015`, `>=2015`, `<=2015`.
fn years(value: &str) -> Option<(f64, f64)> {
    let year = |v: &str| v.trim().parse::<f64>().ok();
    let (min, max) = (f64::NEG_INFINITY, f64::INFINITY);
    if let Some((from, to)) = value.split_once("..") {
        let from = if from.is_empty() { min } else { year(from)? };
        let to = if to.is_empty() { max } else { year(to)? };
        return Some((from.min(to), from.max(to)));
    }
    for (op, at) in [(">=", 0.0), ("<=", 0.0), (">", 1.0), ("<", -1.0)] {
        if let Some(rest) = value.strip_prefix(op) {
            let y = year(rest)? + at;
            return Some(if op.starts_with('>') { (y, max) } else { (min, y) });
        }
    }
    year(value).map(|y| (y, y))
}

/// The completion for the term of `query` that ends at `cursor` (a byte
/// index): a filter's key, or its value (`artists` and `programs` for
/// `artist:` and `addedby:`), written as `mode` says.
pub fn complete(query: &str, cursor: usize, mode: Mode, artists: &[String], programs: &[String]) -> Option<Completion> {
    let (start, term) = search::terms(query).into_iter().find(|(at, term)| *at < cursor && cursor == at + term.len())?;
    let at: Range<usize> = start..cursor;
    let (neg, term) = term.strip_prefix('-').map_or(("", term), |t| ("-", t));
    let options: Vec<(String, String)> = match term.split_once(':') {
        None => {
            let typed = term.to_lowercase();
            KEYS.iter().map(|k| format!("{k}:")).filter(|k| k.starts_with(&typed) && *k != typed).map(|k| (k.clone(), format!("{neg}{k}"))).collect()
        }
        Some((key, raw)) => {
            let key = key.to_lowercase();
            let (_, typed) = search::value(raw);
            let fixed = |options: Vec<String>| -> Vec<(String, String)> {
                options.into_iter().filter(|o| typed.is_empty() || crate::matching::fuzzy(o, typed).is_some()).map(|o| (o.clone(), format!("{neg}{key}:{o}"))).collect()
            };
            match key.as_str() {
                "kind" => fixed(WISH_KINDS.iter().map(|(k, _)| k.to_string()).collect()),
                "got" => fixed(GOT.iter().map(|(g, _)| g.to_string()).collect()),
                "type" => fixed(KINDS.iter().map(|k| k.to_lowercase()).collect()),
                "artist" | "addedby" => {
                    let names = if key == "artist" { artists } else { programs };
                    let mut scored: Vec<(i32, &String)> =
                        names.iter().filter_map(|n| Some((if typed.is_empty() { 0 } else { crate::matching::fuzzy(n, typed)?.0 }, n))).collect();
                    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())));
                    scored.into_iter().take(8).map(|(_, n)| (n.clone(), format!("{neg}{key}:{}", search::quoted(n, mode)))).collect()
                }
                _ => Vec::new(),
            }
        }
    };
    let options: Vec<(String, String)> = options.into_iter().filter(|(_, insert)| query[at.clone()] != *insert).collect();
    (!options.is_empty()).then_some(Completion { at, options })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wish(kind: Kind, title: &str, artist: Option<&str>, year: Option<&str>) -> Wish {
        Wish { artist: artist.map(Into::into), year: year.map(Into::into), release_type: (kind == Kind::Album).then(|| "EP".to_string()), added: 1000, ..Wish::new(kind, title) }
    }

    #[test]
    fn wishes_are_kept_by_their_filters() {
        let ep = wish(Kind::Album, "互いの宇宙", Some("JYOCHO"), Some("2018"));
        let song = Wish { note: "from Willy".into(), ..wish(Kind::Song, "Feather", Some("Nujabes"), Some("2005")) };
        let artist = wish(Kind::Artist, "JYOCHO", Some("Japanese band"), None);
        let keeps = |query: &str, wish: &Wish, state| {
            let filter = Filter::parse(query);
            assert!(filter.problems.is_empty(), "{query}: {:?}", filter.problems);
            filter.keeps(wish, state, 1000 + 86_400)
        };
        assert!(keeps("kind:album type:ep got:no", &ep, State::Wanted));
        assert!(!keeps("got:yes", &ep, State::Wanted));
        assert!(keeps("-got:yes artist:jyocho", &ep, State::Partly));
        assert!(keeps("artist:jyocho", &artist, State::Wanted), "an artist's name is their artist");
        assert!(keeps("year:2010..2019", &ep, State::Wanted) && !keeps("year:>2018", &ep, State::Wanted) && keeps("year:<=2018", &ep, State::Wanted));
        assert!(keeps("willy", &song, State::Got), "text is looked for in the note too");
        assert!(keeps(r#"note:"from willy" added:2d"#, &song, State::Got));
        assert!(!keeps("added:12h", &song, State::Got), "added a day ago");
        assert!(!keeps("-kind:song", &song, State::Got));
        for bad in ["kind:playlist", "got:maybe", "type:boxset", "year:soon", "added:yesterday"] {
            assert_eq!(Filter::parse(bad).problems.len(), 1, "{bad}");
        }
        assert!(Filter::parse("kind:").is_empty(), "still being typed");
    }

    #[test]
    fn keys_and_values_complete() {
        let options = |query: &str| complete(query, query.len(), Mode::Fuzzy, &["JYOCHO".to_string()], &[]).map(|c| c.options.into_iter().map(|(_, i)| i).collect::<Vec<_>>());
        assert_eq!(options("go"), Some(vec!["got:".to_string()]));
        assert_eq!(options("-got:pa"), Some(vec!["-got:partly".to_string()]));
        assert_eq!(options("artist:jyo"), Some(vec!["artist:JYOCHO".to_string()]));
        assert_eq!(options("kind:album"), None, "already in");
    }
}
