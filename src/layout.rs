//! The dockable section layout (egui_dock): which sections exist, the
//! first-launch arrangement, and where a hidden section goes when it's shown
//! again from the View menu.
//!
//! Every section has a home column — songs/soundfonts on the left, the
//! visualizer, script editor, script settings and scripting reference in the
//! middle, playlists on the right. The
//! user can drag tabs anywhere afterwards; homes only decide where a section
//! *reappears*, and only relative to whatever is open at the time, so
//! re-showing one never resets the rest of a custom arrangement.

use egui_dock::{DockState, Node, NodeIndex, Tree};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Section {
    Songs,
    Soundfonts,
    Playlists,
    Visualizer,
    Editor,
    Settings,
    Reference,
}

impl Section {
    pub const ALL: [Section; 7] = [
        Self::Songs,
        Self::Soundfonts,
        Self::Playlists,
        Self::Visualizer,
        Self::Editor,
        Self::Settings,
        Self::Reference,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Songs => "Songs",
            Self::Soundfonts => "Soundfonts",
            Self::Playlists => "Playlists",
            Self::Visualizer => "Visualizer",
            Self::Editor => "Script Editor",
            Self::Settings => "Script Settings",
            Self::Reference => "Scripting Reference",
        }
    }

    fn column(self) -> Column {
        match self {
            Self::Songs | Self::Soundfonts => Column::Left,
            Self::Visualizer | Self::Editor | Self::Settings | Self::Reference => Column::Center,
            Self::Playlists => Column::Right,
        }
    }

    /// The other section sharing this one's home column, if any.
    fn partner(self) -> Option<Section> {
        match self {
            Self::Songs => Some(Self::Soundfonts),
            Self::Soundfonts => Some(Self::Songs),
            Self::Visualizer => Some(Self::Editor),
            Self::Editor => Some(Self::Visualizer),
            Self::Playlists | Self::Settings | Self::Reference => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Column {
    Left,
    Center,
    Right,
}

/// Share of the window a side column gets when it's (re)created.
const SIDE_SHARE: f32 = 0.25;

/// First launch: song browser on the left, playlists on the right — the
/// two sections needed to start listening. Everything else is a View menu
/// click away.
pub fn default_layout() -> DockState<Section> {
    let mut dock = DockState::new(vec![Section::Songs]);
    dock.main_surface_mut().split_right(NodeIndex::root(), 0.5, vec![Section::Playlists]);
    dock
}

/// A layout loaded from the config, cleaned up: a section appearing more
/// than once (hand-edited config, say) keeps only its first copy.
pub fn sanitize(mut dock: DockState<Section>) -> DockState<Section> {
    let mut seen = Vec::new();
    dock.retain_tabs(|tab| {
        if seen.contains(tab) {
            false
        } else {
            seen.push(*tab);
            true
        }
    });
    dock
}

pub fn is_open(dock: &DockState<Section>, section: Section) -> bool {
    dock.find_tab(&section).is_some()
}

pub fn hide(dock: &mut DockState<Section>, section: Section) {
    if let Some(path) = dock.find_tab(&section) {
        dock.remove_tab(path);
    }
}

/// Show `section` near its home, if it isn't already open anywhere.
pub fn show(dock: &mut DockState<Section>, section: Section) {
    if is_open(dock, section) {
        return;
    }
    // The reference reads best right where you're writing: a tab in the
    // editor's group (wherever that is, floating window included).
    if section == Section::Reference && tab_alongside(dock, section, Section::Editor, true) {
        return;
    }

    let tree = dock.main_surface_mut();
    if tree.num_tabs() == 0 {
        *tree = Tree::new(vec![section]);
        return;
    }

    // Settings below whatever it's tweaking — the visualizer, so changes are
    // visible as they're made — else below the editor.
    if section == Section::Settings
        && let Some((node, _)) =
            tree.find_tab(&Section::Visualizer).or_else(|| tree.find_tab(&Section::Editor))
    {
        tree.split_below(node, 0.65, vec![section]);
        return;
    }

    // Next to the other section from the same column, if it's open (on the
    // main surface — a partner floating in its own window doesn't count).
    if let Some(partner) = section.partner()
        && let Some((node, _)) = tree.find_tab(&partner)
    {
        match section {
            Section::Songs => tree.split_above(node, 0.4, vec![section]),
            Section::Soundfonts => tree.split_below(node, 0.6, vec![section]),
            Section::Visualizer => tree.split_left(node, 0.5, vec![section]),
            _ => tree.split_right(node, 0.5, vec![section]),
        };
        return;
    }

    match section.column() {
        Column::Left => {
            tree.split_left(NodeIndex::root(), 1.0 - SIDE_SHARE, vec![section]);
        }
        Column::Right => {
            tree.split_right(NodeIndex::root(), 1.0 - SIDE_SHARE, vec![section]);
        }
        Column::Center => show_center(tree, section),
    }
}

/// Add `section` as another tab in the same group as `host` — what dropping
/// a tab on the middle of another one does. `focus` picks which of the two
/// ends up the visible tab. False (nothing changed) if `host` isn't open or
/// `section` already is.
pub fn tab_alongside(dock: &mut DockState<Section>, section: Section, host: Section, focus: bool) -> bool {
    if is_open(dock, section) {
        return false;
    }
    let Some(host_path) = dock.find_tab(&host) else {
        return false;
    };
    let Ok(leaf) = dock.leaf_mut(host_path.node_path()) else {
        return false;
    };
    leaf.append_tab(section); // focuses the new tab
    if !focus {
        let _ = dock.set_active_tab(host_path);
    }
    true
}

/// A middle section with nothing else from the middle column open: carve it
/// out of the widest side leaf, on the side facing the middle, leaving that
/// side leaf about a normal side column's width.
fn show_center(tree: &mut Tree<Section>, section: Section) {
    let total_width = tree.root_node().and_then(Node::rect).map_or(0.0, |r| r.width());
    let widest = tree
        .iter()
        .enumerate()
        .filter_map(|(i, node)| {
            let leaf = node.get_leaf()?;
            let width = leaf.rect().width();
            Some((NodeIndex(i), leaf.tabs().first().copied()?, if width.is_finite() { width } else { 0.0 }))
        })
        .max_by(|a, b| a.2.total_cmp(&b.2));

    let Some((node, tab, width)) = widest else {
        tree.split_right(NodeIndex::root(), 0.5, vec![section]);
        return;
    };
    // The leaf keeps roughly a side column's share; the rest is the new
    // middle. Rects come from the last frame drawn — before the first one,
    // there's nothing to measure, so just halve it.
    let keep = if total_width > 0.0 && width > 0.0 {
        (SIDE_SHARE * total_width / width).clamp(0.2, 0.8)
    } else {
        0.5
    };
    if tab.column() == Column::Right {
        tree.split_left(node, keep, vec![section]);
    } else {
        tree.split_right(node, keep, vec![section]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(dock: &DockState<Section>) -> Vec<Section> {
        Section::ALL.into_iter().filter(|&s| is_open(dock, s)).collect()
    }

    #[test]
    fn default_layout_is_songs_and_playlists() {
        assert_eq!(open(&default_layout()), vec![Section::Songs, Section::Playlists]);
    }

    #[test]
    fn hide_then_show_round_trips_every_section() {
        let mut dock = default_layout();
        for s in Section::ALL {
            show(&mut dock, s);
        }
        assert_eq!(open(&dock), Section::ALL.to_vec());
        for s in Section::ALL {
            hide(&mut dock, s);
            assert!(!is_open(&dock, s));
            show(&mut dock, s);
            assert!(is_open(&dock, s));
        }
        assert_eq!(dock.iter_all_tabs().count(), Section::ALL.len());
    }

    #[test]
    fn showing_into_an_empty_layout_works() {
        let mut dock = default_layout();
        hide(&mut dock, Section::Songs);
        hide(&mut dock, Section::Playlists);
        assert!(open(&dock).is_empty());
        show(&mut dock, Section::Visualizer);
        assert_eq!(open(&dock), vec![Section::Visualizer]);
    }

    #[test]
    fn reference_tabs_in_with_the_editor() {
        let mut dock = default_layout();
        show(&mut dock, Section::Editor);
        assert!(tab_alongside(&mut dock, Section::Reference, Section::Editor, false));
        let editor = dock.find_tab(&Section::Editor).unwrap();
        let reference = dock.find_tab(&Section::Reference).unwrap();
        assert_eq!(editor.node_path(), reference.node_path());
        // The editor stays the visible tab of the pair.
        let leaf = dock.leaf(editor.node_path()).unwrap();
        assert_eq!(leaf.tabs()[leaf.active.0], Section::Editor);
    }

    #[test]
    fn sanitize_drops_duplicate_tabs() {
        let dock = DockState::new(vec![Section::Songs, Section::Songs, Section::Editor]);
        let dock = sanitize(dock);
        assert_eq!(dock.iter_all_tabs().count(), 2);
    }
}
