//! Sprites written in a script's code, for the sprite editor: finding each
//! `sprite_register({ image = {...}, palette = {...} })` call, reading its
//! image and palette (written in the call, or in a `local NAME = {...}`
//! it names), and writing edited ones back in place, one image row per
//! line. A comment just above the call, `-- sprite sheet: 4 frames of
//! 8x8`, says it's a sprite sheet (for the editor's frame lines and
//! preview; scripts pick frames with `sprite()`'s `src` either way).
//!
//! Positions are char indices into the text.

use crate::code_edit;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub rgb: [u8; 3],
    /// `a`, when the code gives one.
    pub alpha: Option<f32>,
}

/// A sprite sheet's frames: `count` frames of `width` x `height`, side by
/// side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sheet {
    pub count: usize,
    pub width: usize,
    pub height: usize,
}

/// A table literal in the code, and what it holds.
#[derive(Clone, Debug, PartialEq)]
pub struct Table<T> {
    /// Char range from its `{` to just past its `}`.
    pub span: (usize, usize),
    pub value: T,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpriteCall {
    /// Char index of `sprite_register`.
    pub at: usize,
    /// 1-based line of the call.
    pub line: usize,
    /// `local NAME = sprite_register(...)`: NAME.
    pub name: Option<String>,
    /// Rows of palette indices (0 = transparent); `None` when it isn't
    /// written out where the editor can reach it.
    pub image: Option<Table<Vec<Vec<u32>>>>,
    pub palette: Option<Table<Vec<Color>>>,
    pub sheet: Option<Sheet>,
    /// The sheet comment's line: char range including its newline.
    pub sheet_comment: Option<(usize, usize)>,
    /// Char index of the start of the line the call's statement starts on
    /// (where a sheet comment goes).
    pub statement_line_start: usize,
    /// Why it can't be edited, if it can't.
    pub problem: Option<String>,
}

impl SpriteCall {
    pub fn editable(&self) -> bool {
        self.image.is_some() && self.palette.is_some()
    }

    /// A short name for lists: `heart (line 12)`.
    pub fn label(&self) -> String {
        match &self.name {
            Some(name) => format!("{name} (line {})", self.line),
            None => format!("sprite at line {}", self.line),
        }
    }
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn skip_space(cs: &[char], mut i: usize) -> usize {
    while i < cs.len() && cs[i].is_whitespace() {
        i += 1;
    }
    i
}

/// The char index of the `}` matching the `{` at `open` (code only).
fn matching(cs: &[char], mask: &[bool], open: usize) -> Option<usize> {
    let mut depth = 0;
    for i in open..cs.len() {
        if !mask[i] {
            continue;
        }
        match cs[i] {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn line_start(cs: &[char], index: usize) -> usize {
    cs[..index.min(cs.len())].iter().rposition(|&c| c == '\n').map_or(0, |i| i + 1)
}

fn line_of(cs: &[char], index: usize) -> usize {
    cs[..index.min(cs.len())].iter().filter(|&&c| c == '\n').count() + 1
}

/// The top-level `key = <value>` fields of the table `{...}` from `open` to
/// `close`: (key, char index where the value starts).
fn fields(cs: &[char], mask: &[bool], open: usize, close: usize) -> Vec<(String, usize)> {
    let mut found = Vec::new();
    let mut depth = 0;
    let mut i = open + 1;
    let mut at_field_start = true;
    while i < close {
        if !mask[i] {
            i += 1;
            continue;
        }
        let c = cs[i];
        match c {
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' => depth -= 1,
            ',' | ';' if depth == 0 => at_field_start = true,
            c if depth == 0 && at_field_start && (c.is_alphabetic() || c == '_') => {
                let start = i;
                while i < close && is_ident(cs[i]) {
                    i += 1;
                }
                let key: String = cs[start..i].iter().collect();
                let eq = skip_space(cs, i);
                if cs.get(eq) == Some(&'=') && cs.get(eq + 1) != Some(&'=') {
                    found.push((key, skip_space(cs, eq + 1)));
                }
                at_field_start = false;
                continue;
            }
            c if !c.is_whitespace() => at_field_start = false,
            _ => {}
        }
        i += 1;
    }
    found
}

/// The table literal a value starting at `at` is: written right there, or
/// a name with a `local NAME = {...}` earlier in the text.
fn table_at(cs: &[char], mask: &[bool], at: usize) -> Result<(usize, usize), String> {
    if cs.get(at) == Some(&'{') {
        let close = matching(cs, mask, at).ok_or("an unclosed table")?;
        return Ok((at, close));
    }
    let mut end = at;
    while end < cs.len() && is_ident(cs[end]) {
        end += 1;
    }
    if end == at {
        return Err("not a table written in the code".into());
    }
    let name: String = cs[at..end].iter().collect();
    // Followed by more than the name (a call, an index): computed.
    let after = skip_space(cs, end);
    if !matches!(cs.get(after), Some(',' | ';' | '}')) {
        return Err(format!("`{name}...` is computed by the script"));
    }
    let text: String = cs.iter().collect();
    let pattern = format!("local {name} =");
    let mut search = 0;
    while let Some(found) = text[search..].find(&pattern) {
        let byte = search + found;
        let index = text[..byte].chars().count();
        if mask.get(index) == Some(&true) && (index == 0 || !is_ident(cs[index - 1])) {
            let value = skip_space(cs, index + pattern.chars().count());
            if cs.get(value) == Some(&'{') {
                let close = matching(cs, mask, value).ok_or("an unclosed table")?;
                return Ok((value, close));
            }
            return Err(format!("`{name}` is computed by the script"));
        }
        search = byte + pattern.len();
    }
    Err(format!("`{name}` isn't a table written in this script"))
}

/// The image rows of `{ {0, 1}, {1, 0} }`.
fn parse_image(cs: &[char], mask: &[bool], (open, close): (usize, usize)) -> Result<Vec<Vec<u32>>, String> {
    let mut rows = Vec::new();
    let mut i = open + 1;
    loop {
        i = skip_space(cs, i);
        if i >= close {
            break;
        }
        match cs[i] {
            ',' | ';' => i += 1,
            '{' => {
                let end = matching(cs, mask, i).ok_or("an unclosed row")?;
                let inner: String = cs[i + 1..end].iter().collect();
                let row = inner
                    .split([',', ';'])
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.parse::<u32>().map_err(|_| format!("`{s}` in the image isn't a palette index")))
                    .collect::<Result<Vec<u32>, String>>()?;
                rows.push(row);
                i = end + 1;
            }
            '-' if cs.get(i + 1) == Some(&'-') => {
                // A comment between rows.
                while i < close && cs[i] != '\n' {
                    i += 1;
                }
            }
            _ => return Err("the image has something besides rows of numbers".into()),
        }
    }
    Ok(rows)
}

/// The colors of `{ {r = .., g = .., b = ..}, ... }`.
fn parse_palette(text: &str, cs: &[char], (open, close): (usize, usize)) -> Result<Vec<Color>, String> {
    let literals: Vec<code_edit::ColorLiteral> =
        code_edit::color_literals(text).into_iter().filter(|l| l.start > open && l.end <= close).collect();
    // Every entry must be a color table: compare with how many top-level
    // entries there are.
    let mut entries = 0;
    let mut depth = 0;
    let mut in_entry = false;
    for &c in &cs[open + 1..close] {
        match c {
            '{' => {
                if depth == 0 {
                    entries += 1;
                    in_entry = true;
                }
                depth += 1;
            }
            '}' => depth -= 1,
            c if depth == 0 && !c.is_whitespace() && c != ',' && c != ';' && !in_entry => {
                return Err("its colors are kept in variables or worked out by the script".into());
            }
            _ => {}
        }
        if depth == 0 && c == ',' {
            in_entry = false;
        }
    }
    if entries != literals.len() {
        return Err("its colors are kept in variables or worked out by the script".into());
    }
    Ok(literals
        .iter()
        .map(|l| {
            let (rgb, alpha) = l.rgba();
            Color { rgb, alpha: l.has_alpha().then_some(alpha) }
        })
        .collect())
}

/// `-- sprite sheet: 4 frames of 8x8` on the line just above `line_start`:
/// the sheet and that line's char range.
fn sheet_comment(cs: &[char], line_start: usize) -> Option<(Sheet, (usize, usize))> {
    if line_start == 0 {
        return None;
    }
    let prev_start = self::line_start(cs, line_start - 1);
    let line: String = cs[prev_start..line_start - 1].iter().collect();
    let rest = line.trim().strip_prefix("--")?.trim().strip_prefix("sprite sheet:")?.trim();
    let (count, size) = rest.split_once("frames of").or_else(|| rest.split_once("frame of"))?;
    let (width, height) = size.trim().split_once('x')?;
    let sheet = Sheet {
        count: count.trim().parse().ok()?,
        width: width.trim().parse().ok()?,
        height: height.trim().parse().ok()?,
    };
    (sheet.count > 0 && sheet.width > 0 && sheet.height > 0).then_some((sheet, (prev_start, line_start)))
}

/// Every `sprite_register` call in `text`, in order.
pub fn find_sprites(text: &str) -> Vec<SpriteCall> {
    let cs: Vec<char> = text.chars().collect();
    let mask = code_edit::code_mask_of(&cs);
    const NAME: &str = "sprite_register";
    let name_chars: Vec<char> = NAME.chars().collect();
    let mut calls = Vec::new();
    let mut i = 0;
    while i + name_chars.len() <= cs.len() {
        let is_call = mask[i]
            && cs[i..i + name_chars.len()] == name_chars[..]
            && (i == 0 || !is_ident(cs[i - 1]))
            && cs.get(i + name_chars.len()).is_none_or(|&c| !is_ident(c));
        if !is_call {
            i += 1;
            continue;
        }
        let at = i;
        i += name_chars.len();
        let statement_line_start = line_start(&cs, at);
        // `local NAME = sprite_register(...)` / `NAME = ...` / `t.k = ...`
        let before: String = cs[statement_line_start..at].iter().collect();
        let name = before
            .trim_end()
            .strip_suffix('=')
            .map(|lhs| lhs.trim().trim_start_matches("local ").trim().to_string())
            .filter(|n| !n.is_empty());
        let (sheet, sheet_comment) = match sheet_comment(&cs, statement_line_start) {
            Some((sheet, span)) => (Some(sheet), Some(span)),
            None => (None, None),
        };
        let mut call = SpriteCall {
            at,
            line: line_of(&cs, at),
            name,
            image: None,
            palette: None,
            sheet,
            sheet_comment,
            statement_line_start,
            problem: None,
        };
        let paren = skip_space(&cs, i);
        let open = skip_space(&cs, paren + 1);
        if cs.get(paren) != Some(&'(') || cs.get(open) != Some(&'{') {
            call.problem = Some("its argument isn't a table written in the call".into());
            calls.push(call);
            continue;
        }
        let Some(close) = matching(&cs, &mask, open) else {
            call.problem = Some("the table isn't closed yet".into());
            calls.push(call);
            continue;
        };
        let fields = fields(&cs, &mask, open, close);
        let value_of = |key: &str| fields.iter().find(|(k, _)| k == key).map(|&(_, at)| at);
        let mut problems = Vec::new();
        match value_of("image").map(|at| table_at(&cs, &mask, at)) {
            Some(Ok(span)) => match parse_image(&cs, &mask, span) {
                Ok(rows) if rows.is_empty() => problems.push("image: it's empty here (filled in by the script?)".into()),
                Ok(rows) => call.image = Some(Table { span: (span.0, span.1 + 1), value: rows }),
                Err(e) => problems.push(format!("image: {e}")),
            },
            Some(Err(e)) => problems.push(format!("image: {e}")),
            None => problems.push("no `image` in it".into()),
        }
        match value_of("palette").map(|at| table_at(&cs, &mask, at)) {
            Some(Ok(span)) => match parse_palette(text, &cs, span) {
                Ok(colors) => call.palette = Some(Table { span: (span.0, span.1 + 1), value: colors }),
                Err(e) => problems.push(format!("palette: {e}")),
            },
            Some(Err(e)) => problems.push(format!("palette: {e}")),
            None => problems.push("no `palette` in it".into()),
        }
        if !problems.is_empty() {
            call.problem = Some(problems.join("; "));
        }
        calls.push(call);
        i = close;
    }
    calls
}

/// The indentation of the line holding char `index`.
fn indent_at(cs: &[char], index: usize) -> String {
    cs[line_start(cs, index)..].iter().take_while(|&&c| c == ' ' || c == '\t').collect()
}

/// An image table, one row per line, indented under `indent`.
pub fn image_text(rows: &[Vec<u32>], indent: &str) -> String {
    let mut out = String::from("{\n");
    for row in rows {
        let cells: Vec<String> = row.iter().map(u32::to_string).collect();
        out.push_str(&format!("{indent}    {{ {} }},\n", cells.join(", ")));
    }
    out.push_str(indent);
    out.push('}');
    out
}

fn color_text(color: &Color) -> String {
    let [r, g, b] = color.rgb;
    match color.alpha {
        Some(a) => {
            let a = format!("{a:.2}");
            let a = a.trim_end_matches('0').trim_end_matches('.');
            format!("{{ r = {r}, g = {g}, b = {b}, a = {} }}", if a.is_empty() { "0" } else { a })
        }
        None => format!("{{ r = {r}, g = {g}, b = {b} }}"),
    }
}

/// A palette table: on one line when it's short, else one color per line.
pub fn palette_text(colors: &[Color], indent: &str) -> String {
    let entries: Vec<String> = colors.iter().map(color_text).collect();
    let one_line = format!("{{ {} }}", entries.join(", "));
    if one_line.len() <= 90 {
        return one_line;
    }
    let mut out = String::from("{\n");
    for entry in entries {
        out.push_str(&format!("{indent}    {entry},\n"));
    }
    out.push_str(indent);
    out.push('}');
    out
}

fn sheet_comment_text(sheet: &Sheet, indent: &str) -> String {
    format!(
        "{indent}-- sprite sheet: {} frame{} of {}x{}\n",
        sheet.count,
        if sheet.count == 1 { "" } else { "s" },
        sheet.width,
        sheet.height
    )
}

fn replace(text: &mut String, (from, to): (usize, usize), with: &str) {
    code_edit::replace_range(text, (from, to), with);
}

/// Write an edited sprite back over `call` (found in this same `text`):
/// its image, palette and sheet comment. Returns false if it isn't
/// editable.
pub fn write_sprite(text: &mut String, call: &SpriteCall, rows: &[Vec<u32>], colors: &[Color], sheet: Option<Sheet>) -> bool {
    let (Some(image), Some(palette)) = (&call.image, &call.palette) else { return false };
    let cs: Vec<char> = text.chars().collect();
    let image_new = image_text(rows, &indent_at(&cs, image.span.0));
    let palette_new = palette_text(colors, &indent_at(&cs, palette.span.0));
    // Back to front, so the earlier spans stay where they are.
    let mut edits: Vec<((usize, usize), String)> = vec![(image.span, image_new), (palette.span, palette_new)];
    let statement_indent = indent_at(&cs, call.statement_line_start);
    match (call.sheet_comment, sheet) {
        (Some(span), Some(sheet)) => edits.push((span, sheet_comment_text(&sheet, &statement_indent))),
        (Some(span), None) => edits.push((span, String::new())),
        (None, Some(sheet)) => {
            let at = call.statement_line_start;
            edits.push(((at, at), sheet_comment_text(&sheet, &statement_indent)));
        }
        (None, None) => {}
    }
    edits.sort_by_key(|(span, _)| std::cmp::Reverse(span.0));
    for (span, with) in edits {
        replace(text, span, &with);
    }
    true
}

/// A new, blank sprite as code: `width` x `height` (times `frames` side by
/// side), one white color.
pub fn new_sprite_text(name: &str, width: usize, height: usize, frames: usize) -> String {
    let frames = frames.max(1);
    let mut out = String::new();
    if frames > 1 {
        out.push_str(&sheet_comment_text(&Sheet { count: frames, width, height }, ""));
    }
    let rows = vec![vec![0; width * frames]; height];
    let palette = palette_text(&[Color { rgb: [255, 255, 255], alpha: None }], "    ");
    out.push_str(&format!(
        "local {name} = sprite_register({{\n    palette = {palette},\n    image = {},\n}})\n",
        image_text(&rows, "    ")
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCRIPT: &str = "local A = {\n    { 0, 1 },\n    { 1, 0 },\n}\n\n-- sprite sheet: 2 frames of 1x2\nlocal heart = sprite_register({\n    palette = { { r = 230, g = 40, b = 70 }, {r=1,g=2,b=3,a=0.5} },\n    image = A,\n})\nlocal made = sprite_register({ image = build(), palette = { { r = 1, g = 1, b = 1 } } })\n-- sprite_register({ not = this })\n";

    #[test]
    fn finds_sprites_and_reads_them() {
        let sprites = find_sprites(SCRIPT);
        assert_eq!(sprites.len(), 2);
        let heart = &sprites[0];
        assert_eq!(heart.name.as_deref(), Some("heart"));
        assert_eq!(heart.line, 7);
        assert!(heart.editable(), "{:?}", heart.problem);
        assert_eq!(heart.image.as_ref().unwrap().value, [vec![0, 1], vec![1, 0]]);
        assert_eq!(
            heart.palette.as_ref().unwrap().value,
            [Color { rgb: [230, 40, 70], alpha: None }, Color { rgb: [1, 2, 3], alpha: Some(0.5) }]
        );
        assert_eq!(heart.sheet, Some(Sheet { count: 2, width: 1, height: 2 }));
        let made = &sprites[1];
        assert!(!made.editable());
        assert!(made.problem.as_deref().unwrap().contains("computed"), "{:?}", made.problem);
    }

    #[test]
    fn writes_sprites_back_in_place() {
        let mut text = SCRIPT.to_string();
        let heart = find_sprites(&text).remove(0);
        let rows = vec![vec![2, 2, 0], vec![0, 1, 1]];
        let colors = vec![Color { rgb: [9, 9, 9], alpha: None }, Color { rgb: [1, 2, 3], alpha: Some(0.25) }];
        assert!(write_sprite(&mut text, &heart, &rows, &colors, Some(Sheet { count: 3, width: 1, height: 2 })));
        assert!(text.starts_with("local A = {\n    { 2, 2, 0 },\n    { 0, 1, 1 },\n}\n"), "{text}");
        assert!(text.contains("-- sprite sheet: 3 frames of 1x2\nlocal heart"), "{text}");
        assert!(text.contains("palette = { { r = 9, g = 9, b = 9 }, { r = 1, g = 2, b = 3, a = 0.25 } },"), "{text}");
        // And it reads back the same.
        let again = find_sprites(&text).remove(0);
        assert_eq!(again.image.unwrap().value, rows);
        assert_eq!(again.palette.unwrap().value, colors);
        // Dropping the sheet removes the comment.
        let heart = find_sprites(&text).remove(0);
        let rows = heart.image.as_ref().unwrap().value.clone();
        let colors = heart.palette.as_ref().unwrap().value.clone();
        write_sprite(&mut text, &heart, &rows, &colors, None);
        assert!(!text.contains("sprite sheet"), "{text}");
    }

    #[test]
    fn new_sprites_are_valid_and_editable() {
        let code = new_sprite_text("player", 3, 2, 2);
        assert!(code.starts_with("-- sprite sheet: 2 frames of 3x2\n"), "{code}");
        let sprites = find_sprites(&code);
        assert_eq!(sprites.len(), 1);
        assert!(sprites[0].editable(), "{:?}", sprites[0].problem);
        assert_eq!(sprites[0].image.as_ref().unwrap().value, vec![vec![0; 6]; 2]);
        assert_eq!(sprites[0].sheet, Some(Sheet { count: 2, width: 3, height: 2 }));
        assert!(full_moon::parse(&code).is_ok(), "{code}");
    }
}
