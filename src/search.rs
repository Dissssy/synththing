//! The Songs search's filters: `key:value` terms in the search box
//! (`type:mid`, `artist:"daft punk"`, `year:1990..1999`, `-is:starred`)
//! become rules of the rules engine (`smart.rs`), all of which a song has
//! to match; the rest of the query is the fuzzy search. A term that isn't
//! a filter (an unknown key, a value that doesn't parse, one still being
//! typed) is searched for as text.

use std::collections::BTreeMap;

use crate::fields::{Field, Kind};
use crate::smart::{Op, Rule, Rules};
use crate::song_store::Tag;

/// The filters, for the search box's hover text.
pub const HELP: &str = "Filter with key:value (- in front to exclude, quotes around spaces):\n\
    type:mid  (also filetype:, ext:)\n\
    title: artist: album: albumartist: composer: genre: file:  (contains)\n\
    year: track: disc: length: size:  (5, >5, <5, 1..9; length as m:ss, size in MB)\n\
    added:30d  added:>2026-01-01  added:<2026-01-01\n\
    tag:name  is:starred  has:album  (has: any text or number key)";

/// A query split into its filters and the text left to search for.
#[derive(Debug, PartialEq)]
pub struct Search {
    pub rules: Rules,
    pub text: String,
}

/// Split `query` into filters and text, `tags` naming the tags `tag:`
/// can pick.
pub fn parse(query: &str, tags: &BTreeMap<String, Tag>) -> Search {
    let mut rules = Vec::new();
    let mut text = Vec::new();
    for term in terms(query) {
        match filter(term, tags) {
            Some(rule) => rules.push(rule),
            None => text.push(term),
        }
    }
    Search { rules: Rules { rules, seed: 0, ..Rules::default() }, text: text.join(" ") }
}

/// `query`'s terms, split at spaces outside quotes.
fn terms(query: &str) -> Vec<&str> {
    let mut terms = Vec::new();
    let mut start = None;
    let mut quoted = false;
    for (i, c) in query.char_indices() {
        if c == '"' {
            quoted = !quoted;
        }
        if c.is_whitespace() && !quoted {
            terms.extend(start.take().map(|s| &query[s..i]));
        } else if start.is_none() {
            start = Some(i);
        }
    }
    terms.extend(start.map(|s| &query[s..]));
    terms
}

/// The field a filter's key names.
fn field(key: &str) -> Option<Field> {
    Some(match key {
        "title" => Field::Title,
        "artist" => Field::Artist,
        "albumartist" | "album_artist" => Field::AlbumArtist,
        "album" => Field::Album,
        "composer" => Field::Composer,
        "genre" => Field::Genre,
        "file" => Field::FileName,
        "type" | "filetype" | "ext" => Field::FileExtension,
        "year" => Field::Year,
        "track" => Field::Track,
        "disc" => Field::Disc,
        "length" => Field::Length,
        "size" => Field::Size,
        "added" => Field::Imported,
        "tag" => Field::Tag,
        _ => return None,
    })
}

/// The rule a term is, if it's a filter.
fn filter(term: &str, tags: &BTreeMap<String, Tag>) -> Option<Rule> {
    let (not, term) = term.strip_prefix('-').map_or((false, term), |t| (true, t));
    let (key, value) = term.split_once(':')?;
    let key = key.to_lowercase();
    let value = value.trim_matches('"').trim();
    if value.is_empty() {
        return None;
    }
    let rule = |field, op, value: &str| Some(Rule { field, op, value: value.to_string(), to: String::new() });
    match key.as_str() {
        "is" if value.eq_ignore_ascii_case("starred") => rule(Field::Starred, if not { Op::IsFalse } else { Op::IsTrue }, ""),
        "has" => {
            let field = field(&value.to_lowercase()).filter(|f| matches!(f.kind(), Kind::Text | Kind::Number))?;
            rule(field, if not { Op::IsEmpty } else { Op::IsNotEmpty }, "")
        }
        key => {
            let field = field(key)?;
            match field.kind() {
                Kind::Text if field == Field::FileExtension => rule(field, if not { Op::IsNot } else { Op::Is }, value.trim_start_matches('.')),
                Kind::Text => rule(field, if not { Op::NotContains } else { Op::Contains }, value),
                Kind::Number => number(field, not, value),
                Kind::Date => date(not, value),
                Kind::Tag => {
                    let id = tags.iter().find(|(_, t)| t.name.eq_ignore_ascii_case(value)).map(|(id, _)| id.as_str())?;
                    rule(field, if not { Op::HasNot } else { Op::Has }, id)
                }
                Kind::Flag => None,
            }
        }
    }
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

    #[test]
    fn filters_become_rules_and_the_rest_is_text() {
        let search = parse(r#"type:mid  overworld -artist:"koji kondo" year:1990..1999 is:starred tag:chill zelda"#, &tags());
        assert_eq!(search.text, "overworld zelda");
        assert_eq!(
            search.rules.rules,
            [
                rule(Field::FileExtension, Op::Is, "mid", ""),
                rule(Field::Artist, Op::NotContains, "koji kondo", ""),
                rule(Field::Year, Op::InRange, "1990", "1999"),
                rule(Field::Starred, Op::IsTrue, "", ""),
                rule(Field::Tag, Op::Has, "t1", ""),
            ]
        );
        assert!(!search.rules.any);
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
        for query in ["artist:", "re:zero", "year:soon", "-year:>2000", "tag:nope", "is:fun", "has:tag", "added:yesterday"] {
            let search = parse(query, &tags());
            assert!(search.rules.rules.is_empty(), "{query}");
            assert_eq!(search.text, query);
        }
    }
}
