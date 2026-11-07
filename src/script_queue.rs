//! What plays after the song playing, for scripts (`up_next()`): the
//! queue's songs, then what the playlist, album or Autoplay picked to
//! carry on with.

use std::cell::RefCell;
use std::rc::Rc;

use mlua::Lua;

#[derive(Clone, Debug, PartialEq)]
pub struct UpNextSong {
    /// As the app shows it (the song name patterns).
    pub name: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// Seconds.
    pub length: Option<f64>,
    /// In the queue, rather than picked by what's playing.
    pub queued: bool,
}

pub fn register(lua: &Lua, up_next: &Rc<RefCell<Vec<UpNextSong>>>) -> mlua::Result<()> {
    let up_next = Rc::clone(up_next);
    lua.globals().set(
        "up_next",
        lua.create_function(move |lua, count: Option<usize>| {
            let up_next = up_next.borrow();
            let songs = &up_next[..count.unwrap_or(usize::MAX).min(up_next.len())];
            let list = lua.create_table_with_capacity(songs.len(), 0)?;
            for song in songs {
                let t = lua.create_table_with_capacity(0, 6)?;
                t.raw_set("name", song.name.as_str())?;
                t.raw_set("title", song.title.as_deref())?;
                t.raw_set("artist", song.artist.as_deref())?;
                t.raw_set("album", song.album.as_deref())?;
                t.raw_set("length", song.length)?;
                t.raw_set("queued", song.queued)?;
                list.raw_push(t)?;
            }
            Ok(list)
        })?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_up_to_count() {
        let lua = Lua::new();
        let song = |name: &str, queued| UpNextSong { name: name.into(), title: None, artist: None, album: None, length: Some(60.0), queued };
        let up_next = Rc::new(RefCell::new(vec![song("a", true), song("b", false)]));
        register(&lua, &up_next).unwrap();
        let (all, one, first_queued, second): (usize, usize, bool, String) =
            lua.load("return #up_next(), #up_next(1), up_next()[1].queued, up_next()[2].name").eval().unwrap();
        assert_eq!((all, one, first_queued, second.as_str()), (2, 1, true, "b"));
    }
}
