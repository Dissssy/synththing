//! What the library knows of the playing song, for scripts
//! (`song_details()`): every song field the app has (`fields::FIELDS`), by
//! the same keys song name patterns use, with the user's edits, tags and
//! play counts.

use std::cell::RefCell;
use std::rc::Rc;

use mlua::{Lua, Value};

use crate::fields::{Facts, Field, Kind, FIELDS};
use crate::song_store::SongStore;

#[derive(Clone, Debug, PartialEq)]
pub enum Fact {
    Text(String),
    /// A list field's items (artists, genres) or the song's tag names.
    List(Vec<String>),
    Number(f64),
    Flag(bool),
}

/// A song's fields that have a value, by key.
pub type SongDetails = Vec<(&'static str, Fact)>;

/// Fields that need more than the song's own facts (the wishlist, other
/// songs' matches), left out.
const LEFT_OUT: &[Field] = &[Field::Wished, Field::Wish, Field::ArtistMatched];

/// The library's details of the song at `path`, if it's in the library.
pub fn details(songs: &SongStore, path: &std::path::Path) -> Option<SongDetails> {
    let key = songs.key(path)?;
    let song = songs.get_by_key(&key)?;
    let facts = Facts { stats: Some(songs.totals(&key)), ..Facts::of(song) };
    let tags = songs.tags();
    let details = FIELDS
        .iter()
        .filter(|def| !LEFT_OUT.contains(&def.field))
        .filter_map(|def| {
            let fact = match def.kind {
                Kind::Text if def.is_list() => {
                    let items = facts.items(def.field);
                    (!items.is_empty()).then(|| Fact::List(items.into_iter().map(|i| i.into_owned()).collect()))
                }
                Kind::Text => facts.text(def.field).map(|t| Fact::Text(t.into_owned())),
                Kind::Number | Kind::Date => facts.number(def.field).map(Fact::Number),
                Kind::Flag => Some(Fact::Flag(facts.flag(def.field))),
                Kind::Tag => Some(Fact::List(facts.tags.iter().filter_map(|id| tags.get(id)).map(|t| t.name.clone()).collect())),
            }?;
            Some((def.key, fact))
        })
        .collect();
    Some(details)
}

pub fn register(lua: &Lua, details: &Rc<RefCell<Option<SongDetails>>>) -> mlua::Result<()> {
    let details = Rc::clone(details);
    lua.globals().set(
        "song_details",
        lua.create_function(move |lua, ()| {
            let details = details.borrow();
            let Some(details) = details.as_ref() else { return Ok(Value::Nil) };
            let t = lua.create_table_with_capacity(0, details.len())?;
            for (key, fact) in details {
                match fact {
                    Fact::Text(text) => t.raw_set(*key, text.as_str())?,
                    Fact::List(items) => t.raw_set(*key, lua.create_sequence_from(items.iter().map(String::as_str))?)?,
                    Fact::Number(n) if n.fract() == 0.0 && n.abs() < 9e15 => t.raw_set(*key, *n as i64)?,
                    Fact::Number(n) => t.raw_set(*key, *n)?,
                    Fact::Flag(on) => t.raw_set(*key, *on)?,
                }
            }
            Ok(Value::Table(t))
        })?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gives_each_kind_of_fact() {
        let lua = Lua::new();
        let details: Rc<RefCell<Option<SongDetails>>> = Rc::default();
        register(&lua, &details).unwrap();
        let none: Value = lua.load("return song_details()").eval().unwrap();
        assert!(none.is_nil());
        *details.borrow_mut() = Some(vec![
            ("title", Fact::Text("Feather".into())),
            ("artist", Fact::List(vec!["Nujabes".into(), "Cise Starr".into()])),
            ("plays", Fact::Number(12.0)),
            ("loudness", Fact::Number(-9.5)),
            ("starred", Fact::Flag(true)),
        ]);
        let (title, second, plays, kind, loud, starred): (String, String, i64, String, f64, bool) = lua
            .load("local d = song_details() return d.title, d.artist[2], d.plays, math.type(d.plays), d.loudness, d.starred")
            .eval()
            .unwrap();
        assert_eq!((title.as_str(), second.as_str(), plays, kind.as_str(), loud, starred), ("Feather", "Cise Starr", 12, "integer", -9.5, true));
    }
}
