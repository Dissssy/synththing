//! The Songs search: `key:value` filters (`type:mid`, `artist:"daft
//! punk"`, `year:1990..1999`, `-is:starred`) become rules of the rules
//! engine (`smart.rs`), all of which a song has to match, and the rest of
//! the query is the text searched for (`matching::Term`s).
//!
//! A value or a phrase is matched fuzzily as typed or in single quotes
//! (`artist:bonno`, `'daft punk'`), closely in double quotes (the same past
//! case, spacing and punctuation: `artist:"daft punk"`), and exactly after
//! a caret (`artist:^"Daft Punk"`). A term that isn't a filter is searched
//! for as text; one with a filter's key and a value that doesn't parse
//! (`year:soon`) is a [`Problem`] too, to point out.

use std::collections::BTreeMap;
use std::ops::Range;

use crate::fields::{Field, Kind};
use crate::matching::{Mode, Term};
use crate::smart::{Op, Rule, Rules};
use crate::song_store::Tag;

/// The filters, for the search box's hover text.
pub const HELP: &str = "Search for words (fuzzy), \"a phrase\" (past case and punctuation) or ^\"Exact Text\".\n\
    Filter with key:value, - in front to exclude:\n\
    type:mid  (also filetype:, ext:)\n\
    title: artist: album: albumartist: composer: genre: file:\n\
    \u{2003}artist:bonno or artist:'daft pk' (fuzzy), artist:\"daft punk\" (the whole name), artist:^\"Daft Punk\" (exactly)\n\
    year: track: disc: length: size:  (5, >5, <5, 1..9; length as m:ss, size in MB)\n\
    added:30d  added:>2026-01-01  added:<2026-01-01\n\
    tag:name  is:starred  has:album  (has: any text or number key)\n\
    Tab completes, Alt switches it between fuzzy, close and exact.";

/// The keys filters go by, and their fields.
pub const KEYS: &[(&str, Field)] = &[
    ("title", Field::Title),
    ("artist", Field::Artist),
    ("albumartist", Field::AlbumArtist),
    ("album", Field::Album),
    ("composer", Field::Composer),
    ("genre", Field::Genre),
    ("file", Field::FileName),
    ("type", Field::FileExtension),
    ("year", Field::Year),
    ("track", Field::Track),
    ("disc", Field::Disc),
    ("length", Field::Length),
    ("size", Field::Size),
    ("added", Field::Imported),
    ("tag", Field::Tag),
];

/// The keys that aren't a field's.
pub const OTHER_KEYS: &[&str] = &["is", "has"];

/// A query split into its filters and the text left to search for.
#[derive(Debug, Default, PartialEq)]
pub struct Search {
    pub rules: Rules,
    pub text: Vec<Term>,
    pub problems: Vec<Problem>,
}

/// A term that looks like a filter but isn't one (it's searched for as
/// text): where it is in the query, and why.
#[derive(Clone, Debug, PartialEq)]
pub struct Problem {
    pub at: Range<usize>,
    pub why: String,
}

/// Split `query` into filters and text, `tags` naming the tags `tag:`
/// can pick.
pub fn parse(query: &str, tags: &BTreeMap<String, Tag>) -> Search {
    let mut search = Search { rules: Rules { seed: 0, ..Rules::default() }, ..Search::default() };
    for (at, term) in terms(query) {
        match filter(term, tags) {
            Ok(rule) => search.rules.rules.push(rule),
            // (A filter still being typed, `artist:`: nothing yet.)
            Err(Some(why)) if why.is_empty() => {}
            Err(why) => {
                if let Some(why) = why {
                    search.problems.push(Problem { at: at..at + term.len(), why });
                }
                let (mode, text) = value(term);
                if !text.is_empty() {
                    search.text.push(Term { text: text.to_string(), mode });
                }
            }
        }
    }
    search
}

/// The terms of `query` that aren't filters (and so can't be rules), as
/// typed.
pub fn left_out<'q>(query: &'q str, tags: &BTreeMap<String, Tag>) -> Vec<&'q str> {
    let left = |term: &str| match filter(term, tags) {
        Ok(_) => false,
        // (`artist:`, nothing yet: not left out.)
        Err(Some(why)) => !why.is_empty(),
        Err(None) => true,
    };
    terms(query).into_iter().map(|(_, term)| term).filter(|term| left(term)).collect()
}

/// `query`'s terms and where each starts, split at spaces outside quotes
/// (a quote opens only at the start of a term or a value, so "don't"
/// isn't one).
pub fn terms(query: &str) -> Vec<(usize, &str)> {
    let mut terms = Vec::new();
    let mut start = None;
    let mut quote = None;
    let mut previous = ' ';
    for (i, c) in query.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            None if (c == '"' || c == '\'') && matches!(previous, ' ' | ':' | '^' | '-') => quote = Some(c),
            _ => {}
        }
        if c.is_whitespace() && quote.is_none() {
            terms.extend(start.take().map(|s| (s, &query[s..i])));
        } else if start.is_none() {
            start = Some(i);
        }
        previous = if c.is_whitespace() { ' ' } else { c };
    }
    terms.extend(start.map(|s| (s, &query[s..])));
    terms
}

/// How a value (or a phrase) is matched, and its text without the quotes.
pub fn value(text: &str) -> (Mode, &str) {
    let (exact, text) = text.strip_prefix('^').map_or((false, text), |t| (true, t));
    let mode = match text.chars().next() {
        _ if exact => Mode::Exact,
        Some('"') => Mode::Close,
        _ => Mode::Fuzzy,
    };
    let unquoted = match text.chars().next() {
        Some(q @ ('"' | '\'')) => text[1..].strip_suffix(q).unwrap_or(&text[1..]),
        _ => text,
    };
    (mode, unquoted.trim())
}

/// A value as `mode` writes it.
pub fn quoted(text: &str, mode: Mode) -> String {
    match mode {
        Mode::Fuzzy if !text.contains(char::is_whitespace) => text.to_string(),
        Mode::Fuzzy => format!("'{text}'"),
        Mode::Close => format!("\"{text}\""),
        Mode::Exact => format!("^\"{text}\""),
    }
}

/// The field a filter's key names.
pub fn field(key: &str) -> Option<Field> {
    match key {
        "albumartist" | "album_artist" => Some(Field::AlbumArtist),
        "filetype" | "ext" => Some(Field::FileExtension),
        key => KEYS.iter().find(|(k, _)| *k == key).map(|(_, f)| *f),
    }
}

/// The rule a term is: `Err(None)` if it isn't a filter, `Err(why)` if it
/// looks like one but isn't (why empty: it has no value yet).
fn filter(term: &str, tags: &BTreeMap<String, Tag>) -> Result<Rule, Option<String>> {
    let (not, term) = term.strip_prefix('-').map_or((false, term), |t| (true, t));
    let Some((key, raw)) = term.split_once(':') else { return Err(None) };
    let key = key.to_lowercase();
    // (Not a key: "re:zero", "12:30".)
    if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphabetic() || c == '_') {
        return Err(None);
    }
    let known = field(&key).is_some() || OTHER_KEYS.contains(&key.as_str());
    let (mode, value) = value(raw);
    if value.is_empty() {
        return Err(known.then(String::new));
    }
    let bad = |why: &str| Err(Some(why.to_string()));
    let rule = |field, op, value: &str| Ok(Rule { field, op, value: value.to_string(), to: String::new() });
    match key.as_str() {
        "is" if value.eq_ignore_ascii_case("starred") => rule(Field::Starred, if not { Op::IsFalse } else { Op::IsTrue }, ""),
        "is" => bad("is: can only be is:starred"),
        "has" => match field(&value.to_lowercase()).filter(|f| matches!(f.kind(), Kind::Text | Kind::Number)) {
            Some(field) => rule(field, if not { Op::IsEmpty } else { Op::IsNotEmpty }, ""),
            None => bad("has: takes a text or number key, like has:album"),
        },
        // (Not flagged: it may be a song's name, "re:zero".)
        _ if !known => Err(None),
        key => {
            let field = field(key).expect("known");
            match field.kind() {
                Kind::Text if field == Field::FileExtension && mode == Mode::Fuzzy => {
                    rule(field, if not { Op::IsNot } else { Op::Is }, value.trim_start_matches('.'))
                }
                Kind::Text => {
                    let op = match (mode, not) {
                        (Mode::Fuzzy, false) => Op::Like,
                        (Mode::Fuzzy, true) => Op::NotLike,
                        (Mode::Close, false) => Op::Is,
                        (Mode::Close, true) => Op::IsNot,
                        (Mode::Exact, false) => Op::Exactly,
                        (Mode::Exact, true) => Op::NotExactly,
                    };
                    rule(field, op, value)
                }
                Kind::Number => number(field, not, value).ok_or_else(|| {
                    Some(if not { format!("{key}: can only leave out one number, like -{key}:5") } else { format!("{key}: takes 5, >5, <5 or 1..9") })
                }),
                Kind::Date => date(not, value).ok_or_else(|| Some("added: takes 30d, >2026-01-01 or <2026-01-01".to_string())),
                Kind::Tag => match tags.iter().find(|(_, t)| t.name.eq_ignore_ascii_case(value)) {
                    Some((id, _)) => rule(field, if not { Op::HasNot } else { Op::Has }, id),
                    None => bad(&format!("There's no tag named {value}")),
                },
                Kind::Flag => Err(None),
            }
        }
    }
}

/// What the term being typed can be completed with: the part of the query
/// it replaces, and the options (what's shown, and what goes in).
#[derive(Debug, PartialEq)]
pub struct Completion {
    pub at: Range<usize>,
    pub options: Vec<(String, String)>,
}

/// How many options a completion offers.
const MAX_OPTIONS: usize = 8;

/// The completion for the term of `query` that ends at `cursor` (a byte
/// index): a filter's key, or its value, written as `mode` says
/// (`quoted`). `values` gives a field's values (the tags' names for
/// `Field::Tag`).
pub fn complete(query: &str, cursor: usize, mode: Mode, values: &mut dyn FnMut(Field) -> Vec<String>) -> Option<Completion> {
    let (start, term) = terms(query).into_iter().find(|(at, term)| *at < cursor && cursor == at + term.len())?;
    let at = start..cursor;
    let (neg, term) = term.strip_prefix('-').map_or(("", term), |t| ("-", t));
    let ranked = |options: Vec<String>, typed: &str| -> Vec<String> {
        let mut scored: Vec<(i32, String)> =
            options.into_iter().filter_map(|o| Some((if typed.is_empty() { 0 } else { crate::matching::fuzzy(&o, typed)?.0 }, o))).collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())));
        scored.into_iter().take(MAX_OPTIONS).map(|(_, o)| o).collect()
    };
    let options: Vec<(String, String)> = match term.split_once(':') {
        None => {
            let typed = term.to_lowercase();
            let keys = KEYS.iter().map(|(k, _)| format!("{k}:")).chain(["is:starred".to_string(), "has:".to_string()]);
            keys.filter(|k| k.starts_with(&typed) && *k != typed).take(MAX_OPTIONS).map(|k| (k.clone(), format!("{neg}{k}"))).collect()
        }
        Some((key, raw)) => {
            let key = key.to_lowercase();
            let (_, typed) = value(raw);
            let keyed = |v: &str| format!("{neg}{key}:{v}");
            match key.as_str() {
                "has" => {
                    let keys = KEYS.iter().filter(|(_, f)| matches!(f.kind(), Kind::Text | Kind::Number)).map(|(k, _)| k.to_string()).collect();
                    ranked(keys, typed).into_iter().map(|k| (k.clone(), keyed(&k))).collect()
                }
                "is" => vec![("starred".to_string(), keyed("starred"))],
                key => {
                    let field = field(key).filter(|f| matches!(f.kind(), Kind::Text | Kind::Tag))?;
                    let mode = if field == Field::Tag { Mode::Fuzzy } else { mode };
                    ranked(values(field), typed).into_iter().map(|v| (v.clone(), keyed(&quoted(&v, mode)))).collect()
                }
            }
        }
    };
    let options: Vec<(String, String)> = options.into_iter().filter(|(_, insert)| query[at.clone()] != *insert).collect();
    (!options.is_empty()).then_some(Completion { at, options })
}

/// A number filter: `5`, `>5`, `<5` or `1..9` (only `5` can be excluded).
fn number(field: Field, not: bool, value: &str) -> Option<Rule> {
    let parses = |v: &str| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.' || c == ':');
    let (op, value, to) = if let Some(v) = value.strip_prefix('>') {
        (Op::Greater, v, "")
    } else if let Some(v) = value.strip_prefix('<') {
        (Op::Less, v, "")
    } else if let Some((a, b)) = value.split_once("..") {
        if !parses(b) {
            return None;
        }
        (Op::InRange, a, b)
    } else {
        (if not { Op::IsNot } else { Op::Is }, value, "")
    };
    if !parses(value) || (not && op != Op::IsNot) {
        return None;
    }
    Some(Rule { field, op, value: value.to_string(), to: to.to_string() })
}

/// A date filter: `30d` (added in the last 30 days; excluded, not), or
/// `>2026-01-01` and `<2026-01-01` (after or before that day).
fn date(not: bool, value: &str) -> Option<Rule> {
    let rule = |op, value: &str| Some(Rule { field: Field::Imported, op, value: value.to_string(), to: String::new() });
    if let Some(days) = value.strip_suffix('d').filter(|d| d.parse::<f64>().is_ok()) {
        return rule(if not { Op::NotInLast } else { Op::InLast }, days);
    }
    let is_date = |v: &str| v.len() >= 7 && v.chars().all(|c| c.is_ascii_digit() || c == '-');
    match (not, value.split_at_checked(1)) {
        (false, Some((">", day))) if is_date(day) => rule(Op::After, day),
        (false, Some(("<", day))) if is_date(day) => rule(Op::Before, day),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags() -> BTreeMap<String, Tag> {
        BTreeMap::from([("t1".to_string(), Tag { name: "Chill".into(), color: [0; 3] })])
    }

    fn rule(field: Field, op: Op, value: &str, to: &str) -> Rule {
        Rule { field, op, value: value.into(), to: to.into() }
    }

    fn term(text: &str, mode: Mode) -> Term {
        Term { text: text.into(), mode }
    }

    #[test]
    fn filters_become_rules_and_the_rest_is_text() {
        let search = parse(r#"type:mid  overworld -artist:"koji kondo" year:1990..1999 is:starred tag:chill zelda"#, &tags());
        assert_eq!(search.text, [term("overworld", Mode::Fuzzy), term("zelda", Mode::Fuzzy)]);
        assert_eq!(
            search.rules.rules,
            [
                rule(Field::FileExtension, Op::Is, "mid", ""),
                rule(Field::Artist, Op::IsNot, "koji kondo", ""),
                rule(Field::Year, Op::InRange, "1990", "1999"),
                rule(Field::Starred, Op::IsTrue, "", ""),
                rule(Field::Tag, Op::Has, "t1", ""),
            ]
        );
        assert!(!search.rules.any);
        assert!(search.problems.is_empty());
    }

    #[test]
    fn quotes_say_how_values_match() {
        let rules = |q| parse(q, &tags()).rules.rules;
        assert_eq!(rules("artist:bonno"), [rule(Field::Artist, Op::Like, "bonno", "")]);
        assert_eq!(rules("artist:'daft punk'"), [rule(Field::Artist, Op::Like, "daft punk", "")]);
        assert_eq!(rules("artist:\"daft punk\""), [rule(Field::Artist, Op::Is, "daft punk", "")]);
        assert_eq!(rules("-artist:^\"Daft Punk\""), [rule(Field::Artist, Op::NotExactly, "Daft Punk", "")]);
        let text = |q| parse(q, &tags()).text;
        assert_eq!(text("\"chrono trigger\" don't"), [term("chrono trigger", Mode::Close), term("don't", Mode::Fuzzy)]);
        assert_eq!(text("^\"Chrono\""), [term("Chrono", Mode::Exact)]);
        assert_eq!(text("artist:\"daft"), [], "a value still being typed is a filter already");
    }

    #[test]
    fn numbers_dates_and_has() {
        let rules = |q| parse(q, &tags()).rules.rules;
        assert_eq!(rules("length:>3:30"), [rule(Field::Length, Op::Greater, "3:30", "")]);
        assert_eq!(rules("-track:1"), [rule(Field::Track, Op::IsNot, "1", "")]);
        assert_eq!(rules("added:30d"), [rule(Field::Imported, Op::InLast, "30", "")]);
        assert_eq!(rules("-added:7d"), [rule(Field::Imported, Op::NotInLast, "7", "")]);
        assert_eq!(rules("added:<2026-01-01"), [rule(Field::Imported, Op::Before, "2026-01-01", "")]);
        assert_eq!(rules("-has:album"), [rule(Field::Album, Op::IsEmpty, "", "")]);
        assert_eq!(rules("type:.MID"), [rule(Field::FileExtension, Op::Is, "MID", "")]);
    }

    #[test]
    fn what_isnt_a_filter_is_text() {
        for query in ["artist:", "re:zero", "12:30", "year:soon", "-year:>2000", "tag:nope", "is:fun", "has:tag", "added:yesterday", "colour:red"] {
            let search = parse(query, &tags());
            assert!(search.rules.rules.is_empty(), "{query}");
            assert_eq!(search.text.len(), usize::from(query != "artist:"), "{query}");
        }
        let problems = |q| parse(q, &tags()).problems.into_iter().map(|p| p.at).collect::<Vec<_>>();
        assert!(problems("re:zero 12:30 artist:").is_empty());
        let found = problems("zelda year:soon colour:red");
        assert_eq!((found.len(), found[0].clone()), (1, 6..15));
        assert_eq!(left_out("zelda year:soon type:mid artist:", &tags()), ["zelda", "year:soon"]);
    }

    #[test]
    fn completing_keys_and_values() {
        let mut values = |field: Field| match field {
            Field::Artist => vec!["Daft Punk".to_string(), "Nujabes".to_string()],
            _ => Vec::new(),
        };
        let options = |q: &str, mode, values: &mut dyn FnMut(Field) -> Vec<String>| {
            complete(q, q.len(), mode, values).map(|c| (c.at, c.options.into_iter().map(|(_, i)| i).collect::<Vec<_>>()))
        };
        assert_eq!(options("zelda ar", Mode::Fuzzy, &mut values), Some((6..8, vec!["artist:".to_string()])));
        assert_eq!(options("-artist:dp", Mode::Fuzzy, &mut values), Some((0..10, vec!["-artist:'Daft Punk'".to_string()])));
        assert_eq!(options("artist:dp", Mode::Close, &mut values).unwrap().1, ["artist:\"Daft Punk\""]);
        assert_eq!(options("artist:dp", Mode::Exact, &mut values).unwrap().1, ["artist:^\"Daft Punk\""]);
        assert_eq!(options("artist:", Mode::Fuzzy, &mut values).unwrap().1.len(), 2);
        assert_eq!(options("zelda ", Mode::Fuzzy, &mut values), None, "nothing typed");
        assert_eq!(options("artist:Nujabes", Mode::Fuzzy, &mut values), None, "done");
    }
}
