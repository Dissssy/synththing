//! The script editor's editing smarts, as plain text operations: auto-indent
//! on Enter, Tab / Shift+Tab, toggling comments, auto-closing brackets and
//! quotes, `end` lining itself up, bracket matching, and find. The editor
//! (`app/editor.rs`) turns key presses into these.
//!
//! Positions are char indices; a selection is `(anchor, cursor)`, the
//! cursor being the end that moves, equal for no selection. Everything
//! works on a `Vec<char>` copy, simple and plenty fast for a script.

/// One level of indentation.
pub const INDENT: &str = "    ";

pub type Selection = (usize, usize);

fn chars(text: &str) -> Vec<char> {
    text.chars().collect()
}

fn ordered((a, b): Selection) -> (usize, usize) {
    if a <= b { (a, b) } else { (b, a) }
}

/// Char index where the line holding `index` starts.
fn line_start(chars: &[char], index: usize) -> usize {
    chars[..index.min(chars.len())].iter().rposition(|&c| c == '\n').map_or(0, |i| i + 1)
}

/// Char index of the end of the line holding `index` (its `\n`, or the end).
fn line_end(chars: &[char], index: usize) -> usize {
    chars[index.min(chars.len())..].iter().position(|&c| c == '\n').map_or(chars.len(), |i| index + i)
}

/// The leading spaces/tabs of the line starting at `start`.
fn indentation(chars: &[char], start: usize) -> String {
    chars[start..].iter().take_while(|&&c| c == ' ' || c == '\t').collect()
}

/// Replace the selection with `insert`; the cursor lands after it.
fn replace(text: &mut String, sel: Selection, insert: &str) -> usize {
    let (from, to) = ordered(sel);
    let mut cs = chars(text);
    let to = to.min(cs.len());
    let from = from.min(to);
    cs.splice(from..to, insert.chars());
    *text = cs.into_iter().collect();
    from + insert.chars().count()
}

/// Whether `before` (a line up to the cursor) opens a block, so the next
/// line goes one level deeper.
fn opens_block(before: &str) -> bool {
    let code = strip_comment(before).trim_end();
    if code.ends_with(['{', '(', '[']) {
        return true;
    }
    let last_word = code.rsplit(|c: char| !(c.is_alphanumeric() || c == '_')).next().unwrap_or("");
    if matches!(last_word, "then" | "do" | "else" | "repeat") {
        return !closes_on_same_line(code);
    }
    // `function name(args)` or `local f = function(args)`, not already
    // closed on the same line.
    if code.ends_with(')') && contains_word(code, "function") {
        return !closes_on_same_line(code);
    }
    false
}

/// A one-liner like `if x then y() end` or `function() return 1 end`.
fn closes_on_same_line(code: &str) -> bool {
    contains_word(code, "end")
}

fn contains_word(code: &str, word: &str) -> bool {
    code.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|w| w == word)
}

/// `line` without a trailing `--` comment (ignoring `--` inside strings).
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut quote = None;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match quote {
            Some(_) if c == b'\\' => i += 1,
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == b'"' || c == b'\'' => quote = Some(c),
            None if c == b'-' && bytes.get(i + 1) == Some(&b'-') => return &line[..i],
            None => {}
        }
        i += 1;
    }
    line
}

/// Enter: a new line indented like this one, one level deeper after a
/// block opener. Between a bracket pair (`{|}`), the closer goes on a line
/// of its own below.
pub fn newline(text: &mut String, sel: Selection) -> Selection {
    let (from, _) = ordered(sel);
    let cs = chars(text);
    let start = line_start(&cs, from);
    let indent = indentation(&cs, start);
    let before: String = cs[start..from.min(cs.len())].iter().collect();
    let deeper = opens_block(&before);
    let (_, to) = ordered(sel);
    let after = cs.get(to).copied();
    let opener = before.trim_end().chars().last();
    let split_pair = matches!((opener, after), (Some('{'), Some('}')) | (Some('('), Some(')')) | (Some('['), Some(']')))
        && before.trim_end().len() == before.len();
    if split_pair {
        let inner = format!("\n{indent}{INDENT}");
        let cursor = replace(text, sel, &format!("{inner}\n{indent}"));
        let cursor = cursor - indent.chars().count() - 1;
        return (cursor, cursor);
    }
    let insert = if deeper { format!("\n{indent}{INDENT}") } else { format!("\n{indent}") };
    let cursor = replace(text, sel, &insert);
    (cursor, cursor)
}

/// The lines a selection touches: (first line's start, last line's end).
/// A selection ending right at a line's start doesn't take that line.
fn touched_lines(cs: &[char], sel: Selection) -> (usize, usize) {
    let (from, mut to) = ordered(sel);
    if to > from && to > 0 && cs.get(to - 1) == Some(&'\n') {
        to -= 1;
    }
    (line_start(cs, from), line_end(cs, to))
}

/// Run `edit` on each line the selection touches; returns the new selection
/// covering the same lines (or the moved cursor, when nothing was selected).
fn edit_lines(text: &mut String, sel: Selection, mut edit: impl FnMut(&str) -> String) -> Selection {
    let cs = chars(text);
    let (start, end) = touched_lines(&cs, sel);
    let block: String = cs[start..end].iter().collect();
    let edited: Vec<String> = block.split('\n').map(&mut edit).collect();
    let edited = edited.join("\n");
    let mut new: Vec<char> = cs[..start].to_vec();
    new.extend(edited.chars());
    new.extend(&cs[end..]);
    *text = new.into_iter().collect();
    let new_end = start + edited.chars().count();
    if sel.0 == sel.1 {
        // Keep the cursor at the same spot within its (single) line.
        let shift = new_end as isize - end as isize;
        let cursor = (sel.1 as isize + shift).max(start as isize) as usize;
        (cursor, cursor)
    } else if sel.0 <= sel.1 {
        (start, new_end)
    } else {
        (new_end, start)
    }
}

/// Tab: indent the selected lines, or insert spaces up to the next
/// indentation stop.
pub fn tab(text: &mut String, sel: Selection) -> Selection {
    let cs = chars(text);
    let (from, to) = ordered(sel);
    if cs[from.min(cs.len())..to.min(cs.len())].contains(&'\n') {
        return edit_lines(text, sel, |line| if line.trim().is_empty() { line.to_string() } else { format!("{INDENT}{line}") });
    }
    let column = from - line_start(&cs, from);
    let width = INDENT.len() - column % INDENT.len();
    let cursor = replace(text, sel, &" ".repeat(width));
    (cursor, cursor)
}

/// Shift+Tab: take one level of indentation off the selected lines (or the
/// cursor's line).
pub fn outdent(text: &mut String, sel: Selection) -> Selection {
    edit_lines(text, sel, |line| {
        let spaces = line.chars().take(INDENT.len()).take_while(|&c| c == ' ').count();
        if spaces > 0 {
            line[spaces..].to_string()
        } else {
            line.strip_prefix('\t').unwrap_or(line).to_string()
        }
    })
}

/// Ctrl+/: comment out the selected lines with `-- `, or uncomment them if
/// they all are already.
pub fn toggle_comment(text: &mut String, sel: Selection) -> Selection {
    let cs = chars(text);
    let (start, end) = touched_lines(&cs, sel);
    let block: String = cs[start..end].iter().collect();
    let lines: Vec<&str> = block.split('\n').collect();
    let code_lines = || lines.iter().filter(|l| !l.trim().is_empty());
    let all_commented = code_lines().count() > 0 && code_lines().all(|l| l.trim_start().starts_with("--"));
    let min_indent = code_lines().map(|l| l.len() - l.trim_start().len()).min().unwrap_or(0);
    edit_lines(text, sel, |line| {
        if line.trim().is_empty() {
            return line.to_string();
        }
        if all_commented {
            let indent = line.len() - line.trim_start().len();
            let rest = &line[indent + 2..];
            format!("{}{}", &line[..indent], rest.strip_prefix(' ').unwrap_or(rest))
        } else {
            format!("{}-- {}", &line[..min_indent], &line[min_indent..])
        }
    })
}

fn closer_for(c: char) -> Option<char> {
    match c {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '"' => Some('"'),
        '\'' => Some('\''),
        _ => None,
    }
}

/// Typing `c`: brackets and quotes come in pairs (around the selection, if
/// any), and typing a closer that's already next steps over it. `None`:
/// nothing special, type it normally.
pub fn type_char(text: &mut String, sel: Selection, c: char) -> Option<Selection> {
    let cs = chars(text);
    let (from, to) = ordered(sel);
    let next = cs.get(to).copied();
    let prev = from.checked_sub(1).and_then(|i| cs.get(i)).copied();
    let is_word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    let in_comment_or_string = in_comment_or_string(&cs, from);

    // Step over a closer that's already there.
    if from == to && next == Some(c) && matches!(c, ')' | ']' | '}' | '"' | '\'') {
        return Some((from + 1, from + 1));
    }
    let closer = closer_for(c)?;
    if from != to {
        let selected: String = cs[from..to].iter().collect();
        replace(text, sel, &format!("{c}{selected}{closer}"));
        return Some((from + 1, to + 1));
    }
    if in_comment_or_string {
        return None;
    }
    let quote = c == '"' || c == '\'';
    // Pair only where it helps: not right before a word, and a quote not
    // right after one (`don't`).
    if is_word(next) || (quote && is_word(prev)) {
        return None;
    }
    let cursor = replace(text, sel, &format!("{c}{closer}")) - 1;
    Some((cursor, cursor))
}

/// Backspace right between an empty pair (`(|)`, `"|"`) deletes both.
pub fn backspace_pair(text: &mut String, sel: Selection) -> Option<Selection> {
    if sel.0 != sel.1 || sel.1 == 0 {
        return None;
    }
    let cs = chars(text);
    let at = sel.1;
    let (prev, next) = (cs.get(at - 1).copied()?, cs.get(at).copied()?);
    if closer_for(prev) != Some(next) {
        return None;
    }
    replace(text, (at - 1, at + 1), "");
    Some((at - 1, at - 1))
}

/// After a key was typed at `cursor`: if that finished a line reading just
/// `end` / `else` / `elseif` / `until` / `}` / `)` / `]`, take it back one
/// level, to line up with the line that opened the block. Returns the
/// moved cursor.
pub fn dedent_closer(text: &mut String, cursor: usize) -> Option<usize> {
    let cs = chars(text);
    let start = line_start(&cs, cursor);
    let end = line_end(&cs, cursor);
    if cursor != end {
        return None;
    }
    let line: String = cs[start..end].iter().collect();
    let word = line.trim();
    if !matches!(word, "end" | "else" | "elseif" | "until" | "}" | ")" | "]") {
        return None;
    }
    let indent = line.len() - line.trim_start().len();
    if indent < INDENT.len() {
        return None;
    }
    // Only when it's still at the block's inner depth: the line before
    // (skipping blank ones) is at least as deep and doesn't itself open.
    let mut previous = None;
    let mut probe = start;
    while probe > 0 {
        let prev_start = line_start(&cs, probe - 1);
        let prev: String = cs[prev_start..probe - 1].iter().collect();
        if !prev.trim().is_empty() {
            previous = Some(prev);
            break;
        }
        probe = prev_start;
    }
    let previous = previous?;
    let prev_indent = previous.len() - previous.trim_start().len();
    if prev_indent < indent || (prev_indent == indent && opens_block(&previous)) {
        return None;
    }
    let cursor = edit_lines(text, (cursor, cursor), |l| l[INDENT.len()..].to_string());
    Some(cursor.1)
}

/// Whether `index` is inside a `--` comment or a quoted string on its line.
fn in_comment_or_string(cs: &[char], index: usize) -> bool {
    let start = line_start(cs, index);
    let mut quote = None;
    let mut i = start;
    while i < index.min(cs.len()) {
        let c = cs[i];
        match quote {
            Some(_) if c == '\\' => i += 1,
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '-' && cs.get(i + 1) == Some(&'-') => return true,
            None => {}
        }
        i += 1;
    }
    quote.is_some()
}

/// Which chars are code (not in a string or comment), for bracket matching.
fn code_mask(cs: &[char]) -> Vec<bool> {
    let mut mask = vec![true; cs.len()];
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        let long_open = |at: usize| -> Option<usize> {
            // [[, [=[, [==[ ... : returns the level.
            if cs.get(at) != Some(&'[') {
                return None;
            }
            let level = cs[at + 1..].iter().take_while(|&&c| c == '=').count();
            (cs.get(at + 1 + level) == Some(&'[')).then_some(level)
        };
        let long_close = |from: usize, level: usize| -> usize {
            let mut j = from;
            while j < cs.len() {
                if cs[j] == ']'
                    && cs[j + 1..].iter().take(level).all(|&c| c == '=')
                    && cs.get(j + 1 + level) == Some(&']')
                {
                    return j + 2 + level;
                }
                j += 1;
            }
            cs.len()
        };
        if c == '-' && cs.get(i + 1) == Some(&'-') {
            let end = match long_open(i + 2) {
                Some(level) => long_close(i + 2, level),
                None => line_end(cs, i),
            };
            mask[i..end].iter_mut().for_each(|m| *m = false);
            i = end;
        } else if let Some(level) = long_open(i) {
            let end = long_close(i, level);
            mask[i..end].iter_mut().for_each(|m| *m = false);
            i = end;
        } else if c == '"' || c == '\'' {
            let mut j = i + 1;
            while j < cs.len() && cs[j] != c && cs[j] != '\n' {
                if cs[j] == '\\' {
                    j += 1;
                }
                j += 1;
            }
            let end = (j + 1).min(cs.len());
            mask[i..end].iter_mut().for_each(|m| *m = false);
            i = end;
        } else {
            i += 1;
        }
    }
    mask
}

/// The bracket next to the cursor (just before it, else just after) and its
/// partner, ignoring strings and comments.
pub fn matching_bracket(text: &str, cursor: usize) -> Option<(usize, usize)> {
    let cs = chars(text);
    let mask = code_mask(&cs);
    let is_bracket = |i: usize| mask.get(i) == Some(&true) && matches!(cs[i], '(' | ')' | '[' | ']' | '{' | '}');
    let at = [cursor.checked_sub(1), Some(cursor)].into_iter().flatten().find(|&i| i < cs.len() && is_bracket(i))?;
    let (open, close, forward) = match cs[at] {
        '(' => ('(', ')', true),
        '[' => ('[', ']', true),
        '{' => ('{', '}', true),
        ')' => ('(', ')', false),
        ']' => ('[', ']', false),
        _ => ('{', '}', false),
    };
    let mut depth = 0i32;
    let indices: Box<dyn Iterator<Item = usize>> = if forward { Box::new(at..cs.len()) } else { Box::new((0..=at).rev()) };
    for i in indices {
        if !mask[i] {
            continue;
        }
        if cs[i] == open {
            depth += if forward { 1 } else { -1 };
        } else if cs[i] == close {
            depth += if forward { -1 } else { 1 };
        }
        if depth == 0 {
            return Some((at, i));
        }
    }
    None
}

/// Every match of `query` in `text`, as char ranges.
pub fn find_all(text: &str, query: &str, case_sensitive: bool) -> Vec<(usize, usize)> {
    if query.is_empty() {
        return Vec::new();
    }
    let fold = |s: &str| if case_sensitive { s.to_string() } else { s.to_lowercase() };
    let hay: Vec<char> = fold(text).chars().collect();
    let needle: Vec<char> = fold(query).chars().collect();
    // Lowercasing can change lengths for a few characters; only match when
    // it didn't, so indices stay valid.
    if hay.len() != text.chars().count() {
        return Vec::new();
    }
    let mut matches = Vec::new();
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if hay[i..i + needle.len()] == needle[..] {
            matches.push((i, i + needle.len()));
            i += needle.len();
        } else {
            i += 1;
        }
    }
    matches
}

/// Replace the char range `range` with `with`.
pub fn replace_range(text: &mut String, range: (usize, usize), with: &str) -> usize {
    replace(text, range, with)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `|` marks the cursor (or `[` `]` a selection, anchor first) in
    /// `before`; returns the text after `op` with the result marked the
    /// same way.
    fn run(before: &str, op: impl FnOnce(&mut String, Selection) -> Selection) -> String {
        let (text, sel) = parse(before);
        let mut text = text;
        let sel = op(&mut text, sel);
        mark(&text, sel)
    }

    fn parse(marked: &str) -> (String, Selection) {
        let mut text = String::new();
        let (mut anchor, mut cursor) = (None, None);
        for c in marked.chars() {
            match c {
                '|' => cursor = Some(text.chars().count()),
                '«' => anchor = Some(text.chars().count()),
                '»' => cursor = Some(text.chars().count()),
                c => text.push(c),
            }
        }
        let cursor = cursor.unwrap();
        (text, (anchor.unwrap_or(cursor), cursor))
    }

    fn mark(text: &str, (anchor, cursor): Selection) -> String {
        let mut out = String::new();
        for (i, c) in text.chars().chain(std::iter::once('\0')).enumerate() {
            if anchor != cursor && i == anchor {
                out.push('«');
            }
            if i == cursor {
                out.push(if anchor == cursor { '|' } else { '»' });
            }
            if c != '\0' {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn enter_keeps_and_deepens_indentation() {
        assert_eq!(run("    x = 1|", newline), "    x = 1\n    |");
        assert_eq!(run("if a then|", newline), "if a then\n    |");
        assert_eq!(run("  for i = 1, 3 do|", newline), "  for i = 1, 3 do\n      |");
        assert_eq!(run("function render(w, h)|", newline), "function render(w, h)\n    |");
        assert_eq!(run("local f = function()|", newline), "local f = function()\n    |");
        assert_eq!(run("if a then b() end|", newline), "if a then b() end\n|");
        assert_eq!(run("x = 1 -- then|", newline), "x = 1 -- then\n|");
        assert_eq!(run("local t = {|}", newline), "local t = {\n    |\n}");
        assert_eq!(run("    f(|)", newline), "    f(\n        |\n    )");
    }

    #[test]
    fn tab_and_shift_tab() {
        assert_eq!(run("ab|", tab), "ab  |");
        assert_eq!(run("«a\nb»\nc", tab), "«    a\n    b»\nc");
        assert_eq!(run("    «a\n    b»", outdent), "«a\nb»");
        assert_eq!(run("      x|", outdent), "  x|");
    }

    #[test]
    fn comments_toggle() {
        assert_eq!(run("    «a()\n    b()»", toggle_comment), "«    -- a()\n    -- b()»");
        assert_eq!(run("«    -- a()\n    -- b()»", toggle_comment), "«    a()\n    b()»");
        assert_eq!(run("x|", toggle_comment), "-- x|");
    }

    #[test]
    fn brackets_and_quotes_pair_up() {
        let typed = |marked: &str, c: char| {
            let (mut text, sel) = parse(marked);
            match type_char(&mut text, sel, c) {
                Some(sel) => mark(&text, sel),
                None => "plain".to_string(),
            }
        };
        assert_eq!(typed("f|", '('), "f(|)");
        assert_eq!(typed("f(|)", ')'), "f()|");
        assert_eq!(typed("x = |", '"'), "x = \"|\"");
        assert_eq!(typed("don|", '\''), "plain");
        assert_eq!(typed("|word", '('), "plain");
        assert_eq!(typed("«a + b»", '('), "(«a + b»)");
        assert_eq!(typed("-- note |", '('), "plain");
        let (mut text, sel) = parse("f(|)");
        assert_eq!(backspace_pair(&mut text, sel).map(|s| mark(&text, s)), Some("f|".to_string()));
    }

    #[test]
    fn end_lines_up_with_its_block() {
        let dedent = |marked: &str| {
            let (mut text, sel) = parse(marked);
            match dedent_closer(&mut text, sel.1) {
                Some(cursor) => mark(&text, (cursor, cursor)),
                None => "unchanged".to_string(),
            }
        };
        assert_eq!(dedent("if a then\n    b()\n    end|"), "if a then\n    b()\nend|");
        assert_eq!(dedent("if a then\n    b()\n    else|"), "if a then\n    b()\nelse|");
        // Already lined up (an empty block): leave it.
        assert_eq!(dedent("if a then\n    end|"), "unchanged");
        assert_eq!(dedent("    x = end_value|"), "unchanged");
    }

    #[test]
    fn brackets_match_outside_strings_and_comments() {
        let text = "f(a, \")\", g(b)) -- (";
        assert_eq!(matching_bracket(text, 2), Some((1, 14)));
        assert_eq!(matching_bracket(text, 15), Some((14, 1)));
        assert_eq!(matching_bracket(text, 12), Some((11, 13)));
        assert_eq!(matching_bracket("x [[ ( ]] y", 6), None);
    }

    #[test]
    fn find_is_case_insensitive_by_default() {
        assert_eq!(find_all("Foo foo FOO", "foo", false), [(0, 3), (4, 7), (8, 11)]);
        assert_eq!(find_all("Foo foo FOO", "foo", true), [(4, 7)]);
        assert!(find_all("abc", "", false).is_empty());
    }
}
