//! Matching text as the search does it, three ways: fuzzily (a word's
//! letters in order, not necessarily together: "zocv" finds "Zelda
//! Ocarina Overworld"), closely (the same words, past case, spacing and
//! punctuation, as grouping goes: `fields::group_key`), and exactly.
//! Matches say which characters matched, to highlight them.

use unicode_normalization::UnicodeNormalization;

/// How a search term (or a rule's value) is matched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Fuzzy,
    Close,
    Exact,
}

/// A word, or a quoted phrase, of the text searched for.
#[derive(Clone, Debug, PartialEq)]
pub struct Term {
    pub text: String,
    pub mode: Mode,
}

impl Term {
    /// How well `hay` has it (higher is better) and the characters (their
    /// indices) that matched. A fuzzy term is matched word by word; a close
    /// or exact one is found as a phrase anywhere in `hay`.
    pub fn find(&self, hay: &str) -> Option<(i32, Vec<usize>)> {
        match self.mode {
            Mode::Fuzzy => fuzzy(hay, &self.text),
            Mode::Close => {
                let (norm, at) = normalized(hay);
                let want = normalized(&self.text).0;
                let start = find_chars(&norm, &want)?;
                Some((20 + 2 * want.len() as i32, at[start..start + want.len()].to_vec()))
            }
            Mode::Exact => {
                let start = hay.find(&self.text)?;
                let from = hay[..start].chars().count();
                let len = self.text.chars().count();
                Some((30 + 2 * len as i32, (from..from + len).collect()))
            }
        }
    }
}

/// Whether `a` and `b` are the same past case, spacing and punctuation.
pub fn close(a: &str, b: &str) -> bool {
    let (a, b) = (normalized(a).0, normalized(b).0);
    !a.is_empty() && a == b
}

/// `text`'s letters and digits in lowercase (NFKC), each with the index of
/// the character of `text` it came from.
fn normalized(text: &str) -> (Vec<char>, Vec<usize>) {
    let mut chars = Vec::new();
    let mut at = Vec::new();
    for (i, c) in text.chars().enumerate() {
        for n in std::iter::once(c).nfkc().filter(|n| n.is_alphanumeric()).flat_map(char::to_lowercase) {
            chars.push(n);
            at.push(i);
        }
    }
    (chars, at)
}

fn find_chars(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Case-insensitive fuzzy match of every word of `needle` in `hay` (each
/// on its own, so "chrono corridor" finds "ChronoTrigger/Corridor"): a
/// score where higher is better, and the characters that matched.
pub fn fuzzy(hay: &str, needle: &str) -> Option<(i32, Vec<usize>)> {
    let mut score = 0;
    let mut hits = Vec::new();
    for word in needle.split_whitespace() {
        let (s, h) = fuzzy_word(hay, word)?;
        score += s;
        hits.extend(h);
    }
    hits.sort_unstable();
    hits.dedup();
    Some((score, hits))
}

/// One word's subsequence match: every character of `needle` appears in
/// `hay` in order. Runs of characters and matches at word starts score
/// higher, so "zocv" ranks "overworld.mid" above a scattered match.
fn fuzzy_word(hay: &str, needle: &str) -> Option<(i32, Vec<usize>)> {
    let chars: Vec<char> = hay.chars().collect();
    let lower: Vec<char> = chars.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();
    let mut score = 0i32;
    let mut from = 0usize;
    let mut previous: Option<usize> = None;
    let mut hits = Vec::new();
    for c in needle.to_lowercase().chars() {
        let found = lower[from..].iter().position(|&h| h == c).map(|i| i + from)?;
        score += 1;
        if previous == Some(found.wrapping_sub(1)) {
            score += 5;
        }
        if found == 0 {
            score += 8;
        } else if matches!(chars[found - 1], ' ' | '_' | '-' | '.' | '/' | '\\') {
            score += 4;
        }
        hits.push(found);
        previous = Some(found);
        from = found + 1;
    }
    // (Tighter matches first.)
    score -= (chars.len() as i32) / 20;
    Some((score, hits))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(text: &str, mode: Mode) -> Term {
        Term { text: text.into(), mode }
    }

    #[test]
    fn three_ways_to_match() {
        let (_, hits) = term("dfpk", Mode::Fuzzy).find("Daft Punk").unwrap();
        assert_eq!(hits, [0, 2, 5, 8]);
        assert!(term("ofpk", Mode::Fuzzy).find("Daft Punk").is_none());
        let (_, hits) = term("chrono trigger", Mode::Close).find("Chrono-Trigger: Corridor").unwrap();
        assert_eq!(hits, (0..14).filter(|&i| i != 6).collect::<Vec<_>>());
        assert!(term("chrono trigger", Mode::Close).find("Chrono Cross Trigger").is_none());
        assert!(term("Daft", Mode::Exact).find("Daft Punk").is_some());
        assert!(term("daft", Mode::Exact).find("Daft Punk").is_none());
        assert!(close("Daft-Punk!", "daft punk"));
        assert!(!close("Daft Punk", "Daft Punks"));
    }
}
