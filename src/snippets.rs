//! Ready-made blocks of script for the editor's Insert menu. A snippet's
//! `$0` marks where the text cursor goes after inserting it; its lines
//! are indented to match where it lands.
//!
//! Some belong at the top of the script (registering things, which should
//! happen once), so they go just above `function render` wherever the
//! cursor is; the rest go at the cursor.

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// At the text cursor.
    Cursor,
    /// At the top level, above `function render` (or at the end).
    TopLevel,
}

pub struct Snippet {
    pub name: &'static str,
    pub description: &'static str,
    pub place: Place,
    pub body: &'static str,
}

pub const SNIPPETS: &[Snippet] = &[
    Snippet {
        name: "Color",
        description: "An empty color table (click its swatch to pick the color)",
        place: Place::Cursor,
        body: "{ r = 255, g = 255, b = 255 }$0",
    },
    Snippet {
        name: "Color with transparency",
        description: "A color table with alpha",
        place: Place::Cursor,
        body: "{ r = 255, g = 255, b = 255, a = 0.5 }$0",
    },
    Snippet {
        name: "Settings",
        description: "A few user-editable settings, read every frame (put them in render)",
        place: Place::Cursor,
        body: "local color = setting_color(\"color\", { r = 90, g = 200, b = 255 })\n\
               local size = setting_float(\"size\", 1.0, 0.1, 4.0)\n\
               local count = setting_int(\"count\", 8, 1, 64)\n\
               local enabled = setting_bool(\"enabled\", true)\n$0",
    },
    Snippet {
        name: "Controls",
        description: "Rebindable keyboard actions, and reading them in render",
        place: Place::TopLevel,
        body: "-- Rebindable with the Controls button above the visualizer.\n\
               local LEFT = input_register(\"left\", { \"left\", \"a\", \"pad_dpad_left\", \"pad_lstick_left\" })\n\
               local RIGHT = input_register(\"right\", { \"right\", \"d\", \"pad_dpad_right\", \"pad_lstick_right\" })\n\
               local JUMP = input_register(\"jump\", { \"space\", \"pad_a\" })\n\
               -- In render: if input_down(LEFT) then ... end\n\
               --            if input(JUMP) == \"pressed\" then ... end\n$0",
    },
    Snippet {
        name: "Text field",
        description: "A typing span: the app edits, the script draws (put it in render)",
        place: Place::Cursor,
        body: "local typing = typing_state()\n\
               if not typing.active and has_focus() then\n    typing_begin(\"\", { max_length = 32 })\n\
               end\n\
               if typing.done then\n    $0-- typing.text is what was typed\n\
               end\n\
               text(8, 8, typing.text, { r = 255, g = 255, b = 255 }, FONT_HEIGHT * 2)\n",
    },
    Snippet {
        name: "Note sequence",
        description: "A little melody to play on the song's instruments",
        place: Place::TopLevel,
        body: "-- Keys and lengths in beats; sequence_play(MELODY) plays it at the song's tempo.\n\
               local MELODY = sequence_register({\n    \
               { key = 60, length = 0.5 }, { key = 64, length = 0.5 }, { key = 67, length = 0.5 },\n    \
               { key = 72, length = 1.5 },$0\n\
               })\n",
    },
    Snippet {
        name: "Notes loop",
        description: "Go through the notes held right now (put it in render)",
        place: Place::Cursor,
        body: "for _, note in ipairs(active_notes()) do\n    $0-- note.channel, note.key, note.velocity, note.source\nend\n",
    },
    Snippet {
        name: "Render function",
        description: "An empty render function",
        place: Place::Cursor,
        body: "function render(width, height, left, right)\n    clear({ r = 16, g = 16, b = 24 })\n    $0\nend\n",
    },
];

/// Indent every line after the first of `body` by `indent`, and find the
/// cursor marker: the text to insert, and the cursor's char offset in it.
pub fn expand(body: &str, indent: &str) -> (String, usize) {
    let mut text = String::new();
    for (n, line) in body.split('\n').enumerate() {
        if n > 0 {
            text.push('\n');
            if !line.is_empty() {
                text.push_str(indent);
            }
        }
        text.push_str(line);
    }
    let cursor = text.find("$0").map_or(text.chars().count(), |b| text[..b].chars().count());
    (text.replacen("$0", "", 1), cursor)
}

/// Where a top-level snippet goes in `text`: the char index of the start
/// of the `function render` line, or the end.
pub fn top_level_spot(text: &str) -> usize {
    if text.starts_with("function render") {
        return 0;
    }
    text.find("\nfunction render").map_or(text.chars().count(), |b| text[..b + 1].chars().count())
}

/// Insert `snippet` into `text` with the text cursor at char `cursor`;
/// returns where the cursor goes.
pub fn insert(text: &mut String, cursor: usize, snippet: &Snippet) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let (at, indent, before, after) = match snippet.place {
        Place::Cursor => {
            let cursor = cursor.min(chars.len());
            let line_start = chars[..cursor].iter().rposition(|&c| c == '\n').map_or(0, |i| i + 1);
            let indent: String = chars[line_start..].iter().take_while(|&&c| c == ' ' || c == '\t').collect();
            (cursor, indent, "", "")
        }
        Place::TopLevel => {
            let at = top_level_spot(text);
            let needs_newline = at > 0 && chars.get(at - 1) != Some(&'\n');
            (at, String::new(), if needs_newline { "\n" } else { "" }, "\n")
        }
    };
    let (body, offset) = expand(snippet.body, &indent);
    let full = format!("{before}{body}{after}");
    crate::code_edit::replace_range(text, (at, at), &full);
    at + before.chars().count() + offset
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snippet(name: &str) -> &'static Snippet {
        SNIPPETS.iter().find(|s| s.name == name).unwrap()
    }

    #[test]
    fn cursor_snippets_follow_the_lines_indentation() {
        let mut text = "function render()\n    \nend\n".to_string();
        let cursor = insert(&mut text, 22, snippet("Notes loop"));
        assert_eq!(
            text,
            "function render()\n    for _, note in ipairs(active_notes()) do\n        -- note.channel, note.key, note.velocity, note.source\n    end\n\nend\n"
        );
        let after: String = text.chars().skip(cursor).take(2).collect();
        assert_eq!(after, "--");
    }

    #[test]
    fn top_level_snippets_go_above_render() {
        let mut text = "local x = 1\nfunction render()\n    clear(x)\nend\n".to_string();
        let cursor = insert(&mut text, 40, snippet("Controls"));
        assert!(text.starts_with("local x = 1\n-- Rebindable"), "{text}");
        assert!(text.contains("--            if input(JUMP) == \"pressed\" then ... end\n\nfunction render()"), "{text}");
        assert!(cursor < text.find("function render").unwrap());
    }

    /// Every snippet, put where it's meant to go, leaves a script that
    /// parses.
    #[test]
    fn every_snippet_parses() {
        let body = "local a = 1

function render(width, height, left, right)
    
end
";
        for s in SNIPPETS {
            let (mut text, cursor) = match s.name {
                "Render function" => (String::new(), 0),
                n if n.starts_with("Color") => ("local c = 
".to_string(), 10),
                _ => (body.to_string(), 61),
            };
            insert(&mut text, cursor, s);
            assert!(full_moon::parse(&text).is_ok(), "{}:
{text}", s.name);
            assert!(!expand(s.body, "").0.contains("$0"), "{}", s.name);
        }
    }
}
