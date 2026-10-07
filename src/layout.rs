//! The dockable section layout (egui_dock): which sections exist, the
//! first-launch arrangement, and where a hidden section goes when it's shown
//! again from the View menu.
//!
//! Every section has a home column, songs/soundfonts on the left, the
//! visualizer, script editor, scripting reference and sprite editor in the
//! middle, playlists on the right. The
//! user can drag tabs anywhere afterwards; homes only decide where a section
//! *reappears*, and only relative to whatever is open at the time, so
//! re-showing one never resets the rest of a custom arrangement.

use egui_dock::{DockState, Node, NodeIndex, Surface, SurfaceIndex, Tree};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Section {
    Songs,
    Soundfonts,
    Playlists,
    Visualizer,
    Editor,
    /// The old Script Settings tab, now the Settings, Controls and Debug
    /// windows. Only here so layouts saved with it still load; `sanitize`
    /// drops it.
    Settings,
    Reference,
    Sprites,
    /// The script library (scripts shared on servers).
    Library,
    /// Songs saved from library servers.
    Downloads,
}

impl Section {
    /// Every section there is (not the removed `Settings`).
    pub const ALL: [Section; 9] = [
        Self::Songs,
        Self::Soundfonts,
        Self::Playlists,
        Self::Visualizer,
        Self::Editor,
        Self::Reference,
        Self::Sprites,
        Self::Library,
        Self::Downloads,
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
            Self::Sprites => "Sprite Editor",
            Self::Library => "Online Library",
            Self::Downloads => "Downloaded songs",
        }
    }

    fn column(self) -> Column {
        match self {
            Self::Songs | Self::Soundfonts | Self::Downloads => Column::Left,
            Self::Visualizer | Self::Editor | Self::Settings | Self::Reference | Self::Sprites | Self::Library => {
                Column::Center
            }
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
            Self::Playlists | Self::Settings | Self::Reference | Self::Sprites | Self::Library | Self::Downloads => None,
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

/// First launch: the song browser on the left with the soundfonts below
/// it, the visualizer on the right: what's needed to start listening and
/// watching. Everything else (playlists included) is a View menu click
/// away.
pub fn default_layout() -> DockState<Section> {
    let mut dock = DockState::new(vec![Section::Songs]);
    let surface = dock.main_surface_mut();
    let [left, _] = surface.split_right(NodeIndex::root(), 0.4, vec![Section::Visualizer]);
    surface.split_below(left, 0.7, vec![Section::Soundfonts]);
    dock
}

/// A built-in arrangement to switch to from View > Layout. Add one by
/// writing its `build` function and listing it in [`PRESETS`].
pub struct Preset {
    pub name: &'static str,
    pub description: &'static str,
    pub build: fn() -> DockState<Section>,
}

pub const PRESETS: &[Preset] = &[
    Preset {
        name: "Listening",
        description: "Songs and soundfonts on the left, the visualizer on the right",
        build: default_layout,
    },
    Preset {
        name: "Watching",
        description: "The visualizer big in the middle, songs and soundfonts on the left, playlists on the right",
        build: watching_layout,
    },
    Preset {
        name: "Script writing",
        description: "The script editor with the reference beside it, the visualizer on the right",
        build: script_writing_layout,
    },
    Preset {
        name: "Sprite editing",
        description: "The sprite editor big on the left, the script editor and the visualizer on the right",
        build: sprite_editing_layout,
    },
];

fn sprite_editing_layout() -> DockState<Section> {
    let mut dock = DockState::new(vec![Section::Sprites]);
    let surface = dock.main_surface_mut();
    let [_, right] = surface.split_right(NodeIndex::root(), 0.58, vec![Section::Editor, Section::Reference]);
    surface.split_below(right, 0.55, vec![Section::Visualizer]);
    dock
}

fn watching_layout() -> DockState<Section> {
    let mut dock = DockState::new(vec![Section::Visualizer]);
    let surface = dock.main_surface_mut();
    let [center, _] = surface.split_left(NodeIndex::root(), 0.78, vec![Section::Songs, Section::Soundfonts]);
    surface.split_right(center, 0.75, vec![Section::Playlists]);
    dock
}

fn script_writing_layout() -> DockState<Section> {
    let mut dock = DockState::new(vec![Section::Editor, Section::Reference]);
    dock.main_surface_mut().split_right(NodeIndex::root(), 0.58, vec![Section::Visualizer]);
    dock
}

/// Whether two layouts are arranged the same: the same tabs in the same
/// places, split the same ways in about the same proportions. Ignores
/// what's only about the moment (which tab is showing in a group, on-screen
/// sizes, scroll positions), which the saved form also holds.
pub fn same_arrangement(a: &DockState<Section>, b: &DockState<Section>) -> bool {
    arrangement(a) == arrangement(b)
}

fn arrangement(dock: &DockState<Section>) -> Vec<String> {
    let mut parts = Vec::new();
    for surface in dock.iter_surfaces() {
        let Some(tree) = surface.node_tree() else {
            parts.push("-".to_string());
            continue;
        };
        parts.push("surface".to_string());
        for node in tree.iter() {
            parts.push(match node {
                Node::Empty => "e".to_string(),
                Node::Leaf(leaf) => format!("{:?}", leaf.tabs),
                Node::Vertical(split) => format!("v{:.2}", split.fraction),
                Node::Horizontal(split) => format!("h{:.2}", split.fraction),
            });
        }
    }
    // Trailing empty slots of the node array don't change anything.
    while parts.last().is_some_and(|p| p == "e") {
        parts.pop();
    }
    parts
}

/// A layout the user saved under a name (View > Layout > Save current
/// layout as...), kept in the config.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedLayout {
    pub name: String,
    pub dock: DockState<Section>,
}

/// A separate fullscreen layout's starting point: just the visualizer, which
/// is what fullscreen showed before it became a layout of its own.
pub fn fullscreen_default() -> DockState<Section> {
    DockState::new(vec![Section::Visualizer])
}

/// A layout loaded from the config, cleaned up: a section appearing more
/// than once (hand-edited config, say) keeps only its first copy, and the
/// removed Script Settings tab goes.
pub fn sanitize(mut dock: DockState<Section>) -> DockState<Section> {
    let mut seen = Vec::new();
    dock.retain_tabs(|tab| {
        if seen.contains(tab) || *tab == Section::Settings {
            false
        } else {
            seen.push(*tab);
            true
        }
    });
    ensure_main_tree(&mut dock);
    dock
}

/// egui_dock's `retain_tabs` turns a main area with no tabs left into an
/// "empty surface" with no tree at all, and `main_surface_mut` panics on
/// that. Put an empty tree back so there's somewhere to add tabs to.
fn ensure_main_tree(dock: &mut DockState<Section>) {
    if !dock.is_surface_valid(SurfaceIndex::main())
        && let Some(surface) = dock.get_surface_mut(SurfaceIndex::main())
    {
        *surface = Surface::Main(Tree::new(Vec::new()));
    }
}

pub fn is_open(dock: &DockState<Section>, section: Section) -> bool {
    dock.find_tab(&section).is_some()
}

pub fn hide(dock: &mut DockState<Section>, section: Section) {
    if let Some(path) = dock.find_tab(&section) {
        dock.remove_tab(path);
    }
}

/// Bring `section`'s tab to the front of its group, if it's open.
pub fn focus(dock: &mut DockState<Section>, section: Section) {
    if let Some(path) = dock.find_tab(&section) {
        let _ = dock.set_active_tab(path);
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

    ensure_main_tree(dock);
    let tree = dock.main_surface_mut();
    if tree.num_tabs() == 0 {
        *tree = Tree::new(vec![section]);
        return;
    }

    // Next to the other section from the same column, if it's open (on the
    // main surface, a partner floating in its own window doesn't count).
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

/// Add `section` as another tab in the same group as `host`, what dropping
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
    // middle. Rects come from the last frame drawn, before the first one,
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
    fn default_layout_is_songs_soundfonts_and_the_visualizer() {
        assert_eq!(open(&default_layout()), vec![Section::Songs, Section::Soundfonts, Section::Visualizer]);
        // Playlists can go in the visualizer's spot, as a tab beside it.
        let mut dock = default_layout();
        assert!(tab_alongside(&mut dock, Section::Playlists, Section::Visualizer, true));
        assert_eq!(dock.find_tab(&Section::Playlists).unwrap().node_path(), dock.find_tab(&Section::Visualizer).unwrap().node_path());
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
        hide(&mut dock, Section::Soundfonts);
        hide(&mut dock, Section::Visualizer);
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

    /// A saved layout with no tabs left in the main area (all closed, or
    /// all floated out into windows) comes back from `sanitize` with no main
    /// tree at all, which egui_dock panics on when anything's added.
    #[test]
    fn showing_into_a_reloaded_layout_with_an_empty_main_area_works() {
        let mut dock = default_layout();
        hide(&mut dock, Section::Songs);
        hide(&mut dock, Section::Soundfonts);
        hide(&mut dock, Section::Visualizer);
        let json = serde_json::to_string(&dock).unwrap();
        let mut dock = sanitize(serde_json::from_str(&json).unwrap());
        show(&mut dock, Section::Visualizer);
        assert_eq!(open(&dock), vec![Section::Visualizer]);
        show(&mut dock, Section::Songs);
        assert_eq!(open(&dock), vec![Section::Songs, Section::Visualizer]);

        let mut floated = DockState::new(vec![Section::Songs]);
        floated.add_window(vec![Section::Playlists]);
        hide(&mut floated, Section::Songs);
        let mut floated = sanitize(floated);
        show(&mut floated, Section::Editor);
        assert_eq!(open(&floated), vec![Section::Playlists, Section::Editor]);
    }

    #[test]
    fn presets_hold_each_section_at_most_once() {
        for preset in PRESETS {
            let dock = (preset.build)();
            let tabs: Vec<Section> = dock.iter_all_tabs().map(|(_, tab)| *tab).collect();
            assert!(!tabs.is_empty(), "{}", preset.name);
            for section in &tabs {
                assert_eq!(tabs.iter().filter(|s| *s == section).count(), 1, "{} has {section:?} twice", preset.name);
            }
            // Showing and hiding still works on them.
            let mut dock = dock;
            for section in Section::ALL {
                show(&mut dock, section);
                assert!(is_open(&dock, section));
            }
        }
        assert_eq!(open(&(PRESETS[2].build)()), [Section::Visualizer, Section::Editor, Section::Reference]);
    }

    #[test]
    fn arrangement_ignores_the_moment_but_not_the_layout() {
        let a = (PRESETS[2].build)();
        let mut b = (PRESETS[2].build)();
        assert!(same_arrangement(&a, &b));
        // Switching the shown tab in a group isn't a change...
        if let Some(path) = b.find_tab(&Section::Reference) {
            b.set_active_tab(path).unwrap();
        }
        assert!(same_arrangement(&a, &b));
        // ...closing a section is.
        hide(&mut b, Section::Visualizer);
        assert!(!same_arrangement(&a, &b));
    }

    #[test]
    fn sanitize_drops_duplicate_tabs() {
        let dock = DockState::new(vec![Section::Songs, Section::Songs, Section::Editor]);
        let dock = sanitize(dock);
        assert_eq!(dock.iter_all_tabs().count(), 2);
    }

    #[test]
    fn sanitize_drops_the_old_script_settings_tab() {
        let mut dock = DockState::new(vec![Section::Editor]);
        dock.main_surface_mut().split_below(NodeIndex::root(), 0.5, vec![Section::Settings]);
        let dock = sanitize(dock);
        assert_eq!(dock.iter_all_tabs().map(|(_, t)| *t).collect::<Vec<_>>(), [Section::Editor]);
    }
}
