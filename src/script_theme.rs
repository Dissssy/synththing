//! The app theme's colors, for scripts (`theme()`), so a visualizer can
//! match the look the user picked. Previews and command-line runs get the
//! default theme, so they come out the same everywhere.

use std::cell::RefCell;
use std::rc::Rc;

use mlua::{Lua, Table};

use crate::theme::{Hex, Theme};

pub fn register(lua: &Lua, theme: &Rc<RefCell<Theme>>) -> mlua::Result<()> {
    let theme = Rc::clone(theme);
    lua.globals().set("theme", lua.create_function(move |lua, ()| theme_table(lua, &theme.borrow()))?)
}

fn theme_table(lua: &Lua, theme: &Theme) -> mlua::Result<Table> {
    let p = &theme.palette;
    let colors: [(&str, Hex); 21] = [
        ("background", p.background),
        ("window", p.window),
        ("faint", p.faint),
        ("deep", p.deep),
        ("text", p.text),
        ("widget_text", p.widget_text),
        ("strong_text", p.strong_text),
        ("weak_text", p.weak_text),
        ("selection", p.selection),
        ("accent", p.accent),
        ("link", p.link),
        ("warning", p.warning),
        ("error", p.error),
        ("code", p.code),
        ("widget", p.widget),
        ("widget_hovered", p.widget_hovered),
        ("widget_pressed", p.widget_pressed),
        ("widget_outline", p.widget_outline),
        ("border", p.border),
        ("playing", p.playing),
        ("star", p.star),
    ];
    let t = lua.create_table_with_capacity(0, colors.len() + 2)?;
    t.raw_set("name", theme.name.as_str())?;
    t.raw_set("dark", theme.dark)?;
    for (key, Hex(color)) in colors {
        // Themes keep colors premultiplied (Hex); scripts take them straight.
        let [r, g, b, a] = color.to_srgba_unmultiplied();
        let c = lua.create_table_with_capacity(0, 4)?;
        c.raw_set("r", r)?;
        c.raw_set("g", g)?;
        c.raw_set("b", b)?;
        c.raw_set("a", (f64::from(a) / 255.0 * 1000.0).round() / 1000.0)?;
        t.raw_set(key, c)?;
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gives_the_theme_colors() {
        let lua = Lua::new();
        let theme = Rc::new(RefCell::new(crate::theme::default_theme()));
        register(&lua, &theme).unwrap();
        let (name, dark, r, a): (String, bool, u8, f64) =
            lua.load("local t = theme() return t.name, t.dark, t.accent.r, t.background.a").eval().unwrap();
        assert_eq!((name.as_str(), dark, a), ("Dark", true, 1.0));
        assert_eq!(r, theme.borrow().palette.accent.0.r());
    }
}
