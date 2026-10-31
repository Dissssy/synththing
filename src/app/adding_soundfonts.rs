//! Adding soundfonts to the list without blocking the UI. Whether a file is
//! a SoundFont at all is decided on the spot from its header, and it joins
//! the list right away; the full parse (seconds on a big one) runs on the
//! `AssetCache` loader threads, and the status line says how it went once
//! every file added together has been parsed. The parsed soundfont stays in
//! the cache, so picking it straight after is instant.

use std::path::{Path, PathBuf};
use std::time::Instant;

use super::App;
use crate::config::nice_name;
use crate::loader::{self, AssetKind, LoadState, Priority};

/// Soundfonts added and still being parsed, plus what to report once
/// they're done. Adds made while some are still parsing join the same
/// report.
#[derive(Default)]
pub(super) struct AddingSoundfonts {
    /// Added to the list, still being parsed.
    parsing: Vec<PathBuf>,
    /// Added to the list (including those still parsing).
    added: Vec<PathBuf>,
    /// One line each: files refused, or added but not playable.
    problems: Vec<String>,
}

impl AddingSoundfonts {
    /// The files to keep loaded until they're parsed.
    pub(super) fn parsing(&self) -> &[PathBuf] {
        &self.parsing
    }

    /// Stop waiting on files that came off the list meanwhile. If that
    /// leaves nothing parsing, the add is over: the removal says so instead.
    pub(super) fn forget_removed(&mut self, list: &[PathBuf]) {
        self.parsing.retain(|p| list.contains(p));
        self.added.retain(|p| list.contains(p));
        if self.parsing.is_empty() {
            *self = Self::default();
        }
    }
}

impl App {
    pub(super) fn add_soundfont(&mut self, path: PathBuf) {
        self.add_soundfonts(vec![path]);
    }

    /// Add `paths` to the soundfont list, then parse them in the background
    /// and say how it went in one line.
    pub(super) fn add_soundfonts(&mut self, paths: Vec<PathBuf>) {
        let now = Instant::now();
        let mut any_added = false;
        for path in paths {
            match self.add_to_soundfont_list(&path) {
                Ok(()) => {
                    any_added = true;
                    self.assets.request(&path, AssetKind::SoundFont, Priority::Now, now);
                    self.adding_soundfonts.parsing.push(path.clone());
                    self.adding_soundfonts.added.push(path);
                }
                Err(problem) => self.adding_soundfonts.problems.push(problem),
            }
        }
        if any_added && let Err(e) = self.config.save() {
            self.adding_soundfonts.problems.push(format!("The soundfont list couldn't be saved: {e}"));
        }
        self.collect_parsed_soundfonts();
        self.show_adding_soundfonts();
    }

    /// Put one soundfont on the list (not saved yet), or say why not. Only
    /// files that aren't SoundFont containers at all are refused: one this
    /// build can't render is still added, with a note once it's parsed.
    fn add_to_soundfont_list(&mut self, path: &Path) -> Result<(), String> {
        let name = nice_name(path);
        match loader::is_soundfont_file(path) {
            Ok(true) => {}
            Ok(false) => return Err(format!("'{name}' is not a SoundFont file (no RIFF/sfbk header).")),
            Err(e) => return Err(format!("Failed to read '{name}': {e}")),
        }
        if !self.config.add_soundfont(path) {
            return Err(format!("'{name}' is already in the list."));
        }
        Ok(())
    }

    /// Every frame: note the parses that finished, and update the status
    /// line only when one did, so other messages aren't overwritten while a
    /// parse runs.
    pub(super) fn poll_adding_soundfonts(&mut self) {
        if self.collect_parsed_soundfonts() {
            self.show_adding_soundfonts();
        }
    }

    /// Note the parses that finished. Returns whether any did.
    fn collect_parsed_soundfonts(&mut self) -> bool {
        let adding = &mut self.adding_soundfonts;
        let before = adding.parsing.len();
        let now = Instant::now();
        let mut still_parsing = Vec::new();
        for path in std::mem::take(&mut adding.parsing) {
            match self.assets.get(&path) {
                Some(LoadState::Ready(_)) => {}
                Some(LoadState::Failed(e)) => {
                    adding.problems.push(format!("'{}' was added, but {e}", nice_name(&path)));
                }
                Some(LoadState::Loading) => still_parsing.push(path),
                // Unloaded before it finished: ask again.
                None => {
                    self.assets.request(&path, AssetKind::SoundFont, Priority::Now, now);
                    still_parsing.push(path);
                }
            }
        }
        adding.parsing = still_parsing;
        adding.parsing.len() != before
    }

    /// Put how the add is going (or, once nothing is left parsing, how it
    /// went) on the status line.
    fn show_adding_soundfonts(&mut self) {
        let adding = &mut self.adding_soundfonts;
        if adding.added.is_empty() && adding.problems.is_empty() {
            return;
        }
        self.status = match adding.parsing.len() {
            0 => report(&std::mem::take(adding)),
            1 => format!("Checking soundfont {}...", nice_name(&adding.parsing[0])),
            n => format!("Checking {n} soundfonts..."),
        };
    }
}

/// The status line for a finished add.
fn report(adding: &AddingSoundfonts) -> String {
    for problem in &adding.problems {
        log::info!("adding soundfonts: {problem}");
    }
    let added = adding.added.len();
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    match (adding.added.as_slice(), adding.problems.as_slice()) {
        ([path], []) => format!("Added soundfont: {}", nice_name(path)),
        (_, []) => format!("Added {added} soundfont{}.", plural(added)),
        ([] | [_], [one]) => one.clone(),
        (_, [one]) => format!("Added {added} soundfont{}. {one}", plural(added)),
        (_, more) => format!(
            "Added {added} soundfont{}; {} had problems (Help > Log has them all). {}",
            plural(added),
            more.len(),
            more[0]
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adding(added: &[&str], problems: &[&str]) -> AddingSoundfonts {
        AddingSoundfonts {
            parsing: Vec::new(),
            added: added.iter().map(PathBuf::from).collect(),
            problems: problems.iter().map(|p| p.to_string()).collect(),
        }
    }

    #[test]
    fn one_soundfont_added_names_it() {
        assert_eq!(report(&adding(&["a.sf2"], &[])), "Added soundfont: a");
    }

    #[test]
    fn a_single_problem_is_the_whole_line() {
        let line = "'a' was added, but rustysynth rejected it (bad).";
        assert_eq!(report(&adding(&["a.sf2"], &[line])), line);
        assert_eq!(report(&adding(&[], &["'b' is already in the list."])), "'b' is already in the list.");
    }

    #[test]
    fn several_with_problems_are_counted() {
        assert_eq!(report(&adding(&["a.sf2", "b.sf2"], &[])), "Added 2 soundfonts.");
        assert_eq!(report(&adding(&["a.sf2", "b.sf2"], &["x"])), "Added 2 soundfonts. x");
        assert_eq!(
            report(&adding(&["a.sf2"], &["x", "y"])),
            "Added 1 soundfont; 2 had problems (Help > Log has them all). x"
        );
    }
}
