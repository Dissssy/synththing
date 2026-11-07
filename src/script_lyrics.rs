//! The playing song's lyrics, for scripts (`lyrics()`): what the Lyrics
//! panel shows (`crate::lyrics`), with the song's lyrics offset already in
//! the times, so they compare with `playback().position` as they are.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use mlua::{Lua, RegistryKey, Table, Value};

use crate::lyrics::{Lyrics, Source};

/// The lyrics the app sent, and the offset (seconds) to add to their
/// times.
pub type ScriptLyrics = Option<(Arc<Lyrics>, f64)>;

/// The visualizer's copy of the song's lyrics, and their table once a
/// script asks (made once per change, not per call).
#[derive(Default)]
pub struct SongLyrics {
    lyrics: ScriptLyrics,
    table: Option<RegistryKey>,
}

impl SongLyrics {
    pub fn set(&mut self, lyrics: ScriptLyrics) {
        self.lyrics = lyrics;
        self.table = None;
    }
}

fn source_name(source: Source) -> &'static str {
    match source {
        Source::File => "file",
        Source::Midi => "midi",
        Source::Tags => "tags",
        Source::Lrclib => "lrclib",
    }
}

fn table(lua: &Lua, lyrics: &Lyrics, offset: f64) -> mlua::Result<Table> {
    let lines = lua.create_table_with_capacity(lyrics.lines.len(), 0)?;
    for line in &lyrics.lines {
        let t = lua.create_table_with_capacity(0, 3)?;
        t.raw_set("text", line.text.as_str())?;
        t.raw_set("start", line.at.map(|at| at + offset))?;
        let words = lua.create_table_with_capacity(line.words.len(), 0)?;
        for (i, &(at, place)) in line.words.iter().enumerate() {
            let end = line.words.get(i + 1).map_or(line.text.len(), |&(_, next)| next);
            let word = lua.create_table_with_capacity(0, 2)?;
            word.raw_set("start", at + offset)?;
            word.raw_set("text", line.text.get(place..end).unwrap_or_default())?;
            words.raw_push(word)?;
        }
        t.raw_set("words", words)?;
        lines.raw_push(t)?;
    }
    let t = lua.create_table_with_capacity(0, 3)?;
    t.raw_set("source", source_name(lyrics.source))?;
    t.raw_set("timed", lyrics.timed)?;
    t.raw_set("lines", lines)?;
    Ok(t)
}

pub fn register(lua: &Lua, shared: &Rc<RefCell<SongLyrics>>) -> mlua::Result<()> {
    // (A table made in the Lua this replaces isn't this one's.)
    shared.borrow_mut().table = None;
    let shared = Rc::clone(shared);
    lua.globals().set(
        "lyrics",
        lua.create_function(move |lua, ()| {
            let mut shared = shared.borrow_mut();
            if let Some(key) = &shared.table {
                return lua.registry_value::<Value>(key);
            }
            let Some((lyrics, offset)) = &shared.lyrics else { return Ok(Value::Nil) };
            let t = table(lua, lyrics, *offset)?;
            shared.table = Some(lua.create_registry_value(t.clone())?);
            Ok(Value::Table(t))
        })?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gives_lines_and_words_with_the_offset() {
        let lua = Lua::new();
        let shared = Rc::new(RefCell::new(SongLyrics::default()));
        register(&lua, &shared).unwrap();
        assert_eq!(lua.load("return lyrics()").eval::<Value>().unwrap(), Value::Nil);
        let lyrics = Lyrics::parse("[00:01.00]<00:01.00>Hello <00:02.00>world\n[00:03.00]Again", Source::File).unwrap();
        shared.borrow_mut().set(Some((Arc::new(lyrics), 0.5)));
        let check = r#"
            local l = lyrics()
            assert(l == lyrics(), "the same table each call")
            assert(l.source == "file" and l.timed)
            assert(#l.lines == 2)
            assert(l.lines[1].text == "Hello world" and l.lines[1].start == 1.5)
            assert(#l.lines[1].words == 2)
            assert(l.lines[1].words[1].text == "Hello " and l.lines[1].words[1].start == 1.5)
            assert(l.lines[1].words[2].text == "world" and l.lines[1].words[2].start == 2.5)
            assert(l.lines[2].start == 3.5 and #l.lines[2].words == 0)
        "#;
        lua.load(check).exec().unwrap();
        shared.borrow_mut().set(Some((Arc::new(Lyrics::parse("Just words", Source::Tags).unwrap()), 0.0)));
        lua.load(r#"local l = lyrics(); assert(not l.timed and l.source == "tags" and l.lines[1].start == nil)"#).exec().unwrap();
        shared.borrow_mut().set(None);
        assert_eq!(lua.load("return lyrics()").eval::<Value>().unwrap(), Value::Nil);
    }
}
