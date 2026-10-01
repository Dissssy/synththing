//! Hand-rolled Lua syntax highlighting for the script editor, via
//! `TextEdit::layouter` — egui's own hook for exactly this. Lua's lexical
//! grammar is small enough that a dedicated tokenizer crate would be more
//! machinery than the problem needs; this one scans byte-at-a-time and
//! never has to understand anything above the token level (no parsing, no
//! scoping) to color comments/strings/numbers/keywords and, as a bonus,
//! anything matching a known host function name (see `lua_completion`).

use std::sync::Arc;

use eframe::egui;
use egui::text::LayoutJob;
use egui::{Color32, FontId, Galley, TextFormat, Ui};

use crate::lua_completion;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Keyword,
    HostApi,
    String,
    Number,
    Comment,
    Plain,
}

const KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if",
    "in", "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

fn color_for(kind: Kind, visuals: &egui::Visuals) -> Color32 {
    match kind {
        Kind::Keyword => Color32::from_rgb(198, 120, 221),
        Kind::HostApi => Color32::from_rgb(97, 175, 239),
        Kind::String => Color32::from_rgb(152, 195, 121),
        Kind::Number => Color32::from_rgb(209, 154, 102),
        Kind::Comment => Color32::from_rgb(130, 137, 151),
        Kind::Plain => visuals.text_color(),
    }
}

/// The `TextEdit::layouter` callback: tokenizes `source` and returns a
/// colored galley at `wrap_width`. Called every frame the editor is drawn
/// (egui caches nothing here, so this has to stay cheap — a single linear
/// scan plus one `HOST_API` lookup per identifier easily does).
pub fn layout(ui: &Ui, source: &str, wrap_width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap_width;
    let visuals = ui.visuals();
    let font_id = FontId::monospace(13.0);

    for (text, kind) in tokenize(source) {
        job.append(
            text,
            0.0,
            TextFormat { font_id: font_id.clone(), color: color_for(kind, visuals), ..Default::default() },
        );
    }

    ui.fonts_mut(|f| f.layout_job(job))
}

/// Splits `source` into `(slice, kind)` runs covering every byte exactly
/// once (including whitespace and punctuation, tagged `Plain`), in order.
fn tokenize(source: &str) -> Vec<(&str, Kind)> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        let start = i;
        let c = bytes[i];

        if c == b'-' && bytes.get(i + 1) == Some(&b'-') {
            if bytes.get(i + 2) == Some(&b'[') && bytes.get(i + 3) == Some(&b'[') {
                i += 4;
                while i < bytes.len() && !(bytes[i] == b']' && bytes.get(i + 1) == Some(&b']')) {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            } else {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            out.push((&source[start..i], Kind::Comment));
            continue;
        }

        if c == b'"' || c == b'\'' {
            let quote = c;
            i += 1;
            while i < bytes.len() && bytes[i] != quote {
                i += if bytes[i] == b'\\' { 2 } else { 1 };
            }
            i = (i + 1).min(bytes.len());
            out.push((&source[start..i], Kind::String));
            continue;
        }

        if c.is_ascii_digit() {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.') {
                i += 1;
            }
            out.push((&source[start..i], Kind::Number));
            continue;
        }

        if c.is_ascii_alphabetic() || c == b'_' {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &source[start..i];
            let kind = if KEYWORDS.contains(&word) {
                Kind::Keyword
            } else if lua_completion::lookup(word).is_some() {
                Kind::HostApi
            } else {
                Kind::Plain
            };
            out.push((word, kind));
            continue;
        }

        // Whitespace/punctuation/anything else: consume a run of bytes that
        // don't start one of the special cases above, as a single `Plain`
        // span rather than one per character. None of the "stop" checks
        // below ever match a UTF-8 continuation byte (they're all ASCII,
        // which continuation bytes never equal), so `i` always lands back
        // on a char boundary when the loop exits — safe to slice.
        while i < bytes.len() {
            let c = bytes[i];
            let starts_comment = c == b'-' && bytes.get(i + 1) == Some(&b'-');
            let starts_special =
                starts_comment || c == b'"' || c == b'\'' || c.is_ascii_digit() || c.is_ascii_alphabetic() || c == b'_';
            if starts_special {
                break;
            }
            i += 1;
        }
        if i == start {
            i += 1; // defensive: never spin on a byte nothing above claimed
        }
        out.push((&source[start..i], Kind::Plain));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_reconstructs_the_source_exactly() {
        let source = "-- comment\nlocal x = setting_int(\"n\", 1, 0, 10) -- trailing\nreturn x + 1.5\n";
        let rebuilt: String = tokenize(source).into_iter().map(|(text, _)| text).collect();
        assert_eq!(rebuilt, source);
    }

    #[test]
    fn keywords_and_host_functions_are_tagged() {
        let tokens = tokenize("local function render(width, height, left, right)\n  clear({r=1,g=1,b=1})\nend");
        let kind_of = |word: &str| tokens.iter().find(|(t, _)| *t == word).map(|(_, k)| *k);
        assert_eq!(kind_of("local"), Some(Kind::Keyword));
        assert_eq!(kind_of("function"), Some(Kind::Keyword));
        assert_eq!(kind_of("end"), Some(Kind::Keyword));
        assert_eq!(kind_of("clear"), Some(Kind::HostApi));
        assert_eq!(kind_of("width"), Some(Kind::Plain));
    }

    #[test]
    fn strings_and_comments_are_tagged() {
        let tokens = tokenize("log(\"hi\") -- note\nlocal s = 'ok'");
        assert!(tokens.contains(&("\"hi\"", Kind::String)));
        assert!(tokens.contains(&("-- note", Kind::Comment)));
        assert!(tokens.contains(&("'ok'", Kind::String)));
    }
}
