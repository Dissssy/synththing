//! Copy detection (docs/SERVER.md): a script's fingerprint, to tell an
//! upload that's (nearly) another script from one that only looks alike.
//!
//! The source is cut into Lua tokens (comments and spacing dropped, so
//! reformatting or rewording comments changes nothing), runs of `SHINGLE`
//! tokens are hashed, and a MinHash of `HASHES` values is kept: the share
//! of values two fingerprints have in common estimates how much of their
//! code they share (Jaccard similarity).

/// Tokens per shingle.
const SHINGLE: usize = 5;
/// Values in a fingerprint.
pub const HASHES: usize = 128;
/// Similar from here on: refused as a copy unless it's a remix.
pub const COPY: f64 = 0.8;

/// A script's fingerprint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fingerprint(pub Vec<u64>);

/// FNV-1a, so fingerprints are the same on every build and machine.
fn fnv(bytes: &[u8], seed: u64) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325 ^ seed.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// A 64-bit mix (splitmix64's finisher), for the MinHash's many hashes.
fn mix(mut x: u64) -> u64 {
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// Lua's tokens (`full_moon`'s lexer, as the editor uses), without
/// comments or spacing: names, numbers, strings (quotes and all), and
/// operators. Something that doesn't lex at all falls back to words.
pub fn tokens(source: &str) -> Vec<String> {
    use full_moon::tokenizer::{Lexer, LexerResult, TokenType};
    let lexed = match Lexer::new(source, full_moon::LuaVersion::lua54()).collect() {
        LexerResult::Ok(tokens) | LexerResult::Recovered(tokens, _) => tokens,
        LexerResult::Fatal(_) => return source.split_whitespace().map(str::to_string).collect(),
    };
    lexed
        .iter()
        .filter(|t| !t.token_type().is_trivia() && !matches!(t.token_type(), TokenType::Eof))
        .map(ToString::to_string)
        .collect()
}

impl Fingerprint {
    pub fn of(source: &str) -> Self {
        let tokens = tokens(source);
        let token_hashes: Vec<u64> = tokens.iter().map(|t| fnv(t.as_bytes(), 0)).collect();
        let shingles: Vec<u64> = if token_hashes.len() < SHINGLE {
            vec![token_hashes.iter().fold(0, |acc, &h| mix(acc ^ h))]
        } else {
            token_hashes.windows(SHINGLE).map(|w| w.iter().fold(0x5bd1_e995, |acc, &h| mix(acc ^ h))).collect()
        };
        let mins = (0..HASHES as u64)
            .map(|k| {
                let salt = mix(k.wrapping_add(0x1234_5678));
                shingles.iter().map(|&s| mix(s ^ salt)).min().unwrap_or(u64::MAX)
            })
            .collect();
        Self(mins)
    }

    /// About how much of their code two scripts share, 0 to 1.
    pub fn similarity(&self, other: &Self) -> f64 {
        let same = self.0.iter().zip(&other.0).filter(|(a, b)| a == b).count();
        same as f64 / HASHES.max(1) as f64
    }

    /// As stored: hex.
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|v| format!("{v:016x}")).collect()
    }

    pub fn from_hex(text: &str) -> Option<Self> {
        if text.len() != HASHES * 16 {
            return None;
        }
        (0..HASHES).map(|i| u64::from_str_radix(&text[i * 16..i * 16 + 16], 16).ok()).collect::<Option<_>>().map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_skip_comments_and_spacing() {
        assert_eq!(tokens("local x = 1.5 -- hi\n--[[ long\n]] y = 'a b' .. [[c]]"), [
            "local", "x", "=", "1.5", "y", "=", "'a b'", "..", "[[c]]"
        ]);
        assert_eq!(tokens("a~=b...c"), ["a", "~=", "b", "...", "c"]);
    }

    #[test]
    fn copies_and_lookalikes() {
        let disco = crate::lua_visualizer::bundled_default("disco.lua").unwrap();
        let waveform = crate::lua_visualizer::bundled_default("waveform.lua").unwrap();
        let fp = Fingerprint::of(disco);
        assert_eq!(Fingerprint::from_hex(&fp.to_hex()), Some(fp.clone()));
        // Reformatted, comments rewritten: the same.
        let reworded = disco.replace("--", "-- (edited)").replace("    ", "  ");
        assert!(fp.similarity(&Fingerprint::of(&reworded)) > 0.95);
        // A few lines changed: still a copy.
        let tweaked = disco.replacen("255", "254", 3);
        let s = fp.similarity(&Fingerprint::of(&tweaked));
        assert!(s >= COPY, "{s}");
        // Another script: not.
        let s = fp.similarity(&Fingerprint::of(waveform));
        assert!(s < 0.3, "{s}");
        // Every pair of bundled scripts is far apart.
        let all = crate::lua_visualizer::BUNDLED_SCRIPTS;
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                let s = Fingerprint::of(a.2).similarity(&Fingerprint::of(b.2));
                assert!(s < COPY, "{} and {}: {s}", a.1, b.1);
            }
        }
    }
}
