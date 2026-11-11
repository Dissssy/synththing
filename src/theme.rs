//! Themes: the app's colors and shapes (not its layout). A theme starts
//! from egui's dark or light look and sets a palette (backgrounds, text,
//! accent, widget states, ...) and a few shape options (corner radius,
//! border width, shadows) on top, nothing that moves things around, so no
//! theme can break a layout.
//!
//! Built-in themes can't be changed; your own live in the config
//! (`Config::custom_themes`, colors as `#rrggbb` hex), so settings sync
//! carries them, made by duplicating another or by saving a changed built-in one
//! as a copy. The Themes window (`app/themes.rs`) picks and edits them.


use anyhow::{Result, bail};
use eframe::egui::{self, Color32, CornerRadius, Shadow, Stroke, Visuals};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A color, saved as `#rrggbb` (or `#rrggbbaa` when not opaque, with the
/// channels as egui keeps them, premultiplied, so an additive color like
/// egui's own faint background survives the trip).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hex(pub Color32);

impl Serialize for Hex {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let [r, g, b, a] = self.0.to_array();
        let text = if a == 255 { format!("#{r:02x}{g:02x}{b:02x}") } else { format!("#{r:02x}{g:02x}{b:02x}{a:02x}") };
        serializer.serialize_str(&text)
    }
}

impl<'de> Deserialize<'de> for Hex {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        parse_hex(&text).map(Hex).ok_or_else(|| serde::de::Error::custom(format!("not a #rrggbb color: {text}")))
    }
}

fn parse_hex(text: &str) -> Option<Color32> {
    let hex = text.strip_prefix('#')?;
    let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    match hex.len() {
        6 => Some(Color32::from_rgb(byte(0)?, byte(2)?, byte(4)?)),
        8 => Some(Color32::from_rgba_premultiplied(byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
        _ => None,
    }
}

const fn hex(rgb: u32) -> Hex {
    Hex(Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Palette {
    /// Behind the tabs and panels.
    pub background: Hex,
    /// Windows, popups and menus.
    pub window: Hex,
    /// Subtle stripes and highlights.
    pub faint: Hex,
    /// Text fields and the deepest backgrounds.
    pub deep: Hex,
    /// Labels and other text.
    pub text: Hex,
    /// Text on buttons and in fields.
    pub widget_text: Hex,
    /// Text being hovered or pressed, and headings.
    pub strong_text: Hex,
    /// Secondary text.
    pub weak_text: Hex,
    /// Selected items and text.
    pub selection: Hex,
    /// Focus outlines, selected text, checkmarks and slider fills.
    pub accent: Hex,
    pub link: Hex,
    pub warning: Hex,
    pub error: Hex,
    /// Behind `inline code`.
    pub code: Hex,
    /// Buttons, fields and the like: idle, hovered, pressed.
    pub widget: Hex,
    pub widget_hovered: Hex,
    pub widget_pressed: Hex,
    /// Around a widget being hovered or pressed.
    pub widget_outline: Hex,
    /// Separators, window and widget borders.
    pub border: Hex,
    /// Outlining and tinting the song playing, in lists.
    #[serde(default = "default_playing")]
    pub playing: Hex,
    /// Starred songs' alien, in lists.
    #[serde(default = "default_star")]
    pub star: Hex,
    /// Something done: "Saved ...", a line added in a diff, "Listening".
    #[serde(default = "default_success")]
    pub success: Hex,
    /// The name of synththing's own, official things in the online library.
    #[serde(default = "default_official")]
    pub official: Hex,
    /// Under windows and popups (its alpha is how dark).
    #[serde(default = "default_shadow")]
    pub shadow: Hex,
    /// The script editor's Lua: keywords, synththing's own functions,
    /// strings, numbers and comments.
    #[serde(default = "default_syntax")]
    pub syntax: Syntax,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Syntax {
    pub keyword: Hex,
    pub function: Hex,
    pub string: Hex,
    pub number: Hex,
    pub comment: Hex,
}

fn default_playing() -> Hex {
    hex(0x4caf50)
}

fn default_success() -> Hex {
    hex(0x5abe6e)
}

fn light_success() -> Hex {
    hex(0x2e8b45)
}

fn default_official() -> Hex {
    hex(0xe6b43c)
}

fn light_official() -> Hex {
    hex(0xa87a00)
}

fn default_shadow() -> Hex {
    Hex(Color32::from_black_alpha(110))
}

fn light_shadow() -> Hex {
    Hex(Color32::from_black_alpha(45))
}

/// One Dark's, for the dark themes.
fn default_syntax() -> Syntax {
    Syntax { keyword: hex(0xc678dd), function: hex(0x61afef), string: hex(0x98c379), number: hex(0xd19a66), comment: hex(0x828997) }
}

/// One Light's, for the light themes.
fn light_syntax() -> Syntax {
    Syntax { keyword: hex(0xa626a4), function: hex(0x4078f2), string: hex(0x50a14f), number: hex(0x986801), comment: hex(0x8a8c94) }
}

/// Neon green, for the dark themes (`light_star` on light ones).
fn default_star() -> Hex {
    hex(0x39ff14)
}

/// Neon green dark enough to read on a light background.
fn light_star() -> Hex {
    hex(0x1fc40a)
}

/// The theme's palette, kept where everything drawing finds it (`palette`).
fn palette_id() -> egui::Id {
    egui::Id::unique("theme_palette")
}

/// The palette of the theme in use: for the colors egui's own look
/// doesn't have (`Theme::apply` keeps it).
pub fn palette(ctx: &egui::Context) -> Palette {
    ctx.data(|d| d.get_temp(palette_id())).unwrap_or_else(|| default_theme().palette)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shape {
    /// Corner radius of buttons, fields and the like.
    pub widget_radius: u8,
    /// Corner radius of windows, popups and menus.
    pub window_radius: u8,
    pub border_width: f32,
    /// How far popup and window shadows spread (0: none).
    pub shadow: u8,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Theme {
    pub name: String,
    /// Built on egui's dark look (else its light one).
    pub dark: bool,
    pub palette: Palette,
    pub shape: Shape,
}

pub const DEFAULT_THEME: &str = "Dark";

/// The theme used until another is picked (`DEFAULT_THEME`).
pub fn default_theme() -> Theme {
    dark()
}

/// The built-in themes, in the order they're listed.
pub fn builtins() -> Vec<Theme> {
    vec![dark(), light(), frutiger_aero(), programmer_art()]
}

fn dark() -> Theme {
    Theme {
        name: "Dark".into(),
        dark: true,
        palette: Palette {
            background: hex(0x1e1f24),
            window: hex(0x25272d),
            faint: hex(0x2a2c33),
            deep: hex(0x16171b),
            text: hex(0xc9ccd3),
            widget_text: hex(0xe1e3e8),
            strong_text: hex(0xffffff),
            weak_text: hex(0x8a8f99),
            selection: hex(0x34507f),
            accent: hex(0x6d9eff),
            link: hex(0x7aa9ff),
            warning: hex(0xe0b050),
            error: hex(0xe8675f),
            code: hex(0x2e3038),
            widget: hex(0x31343c),
            widget_hovered: hex(0x3b3f49),
            widget_pressed: hex(0x4a505d),
            widget_outline: hex(0x6d9eff),
            border: hex(0x3a3d46),
            playing: default_playing(),
            star: default_star(),
            success: default_success(),
            official: default_official(),
            shadow: default_shadow(),
            syntax: default_syntax(),
        },
        shape: Shape { widget_radius: 4, window_radius: 8, border_width: 1.0, shadow: 16 },
    }
}

fn light() -> Theme {
    Theme {
        name: "Light".into(),
        dark: false,
        palette: Palette {
            background: hex(0xf2f3f5),
            window: hex(0xffffff),
            faint: hex(0xe8e9ed),
            deep: hex(0xfbfbfc),
            text: hex(0x2a2d34),
            widget_text: hex(0x1f2228),
            strong_text: hex(0x000000),
            weak_text: hex(0x6b707b),
            selection: hex(0xbcd2ff),
            accent: hex(0x2f6fde),
            link: hex(0x2563d6),
            warning: hex(0xa66900),
            error: hex(0xc8382f),
            code: hex(0xe5e7eb),
            widget: hex(0xe1e3e8),
            widget_hovered: hex(0xd5d8df),
            widget_pressed: hex(0xc3c8d3),
            widget_outline: hex(0x2f6fde),
            border: hex(0xcdd0d7),
            playing: default_playing(),
            star: light_star(),
            success: light_success(),
            official: light_official(),
            shadow: light_shadow(),
            syntax: light_syntax(),
        },
        shape: Shape { widget_radius: 4, window_radius: 8, border_width: 1.0, shadow: 12 },
    }
}

/// Sky and aqua, rounded and soft, after the glossy mid-2000s look.
fn frutiger_aero() -> Theme {
    Theme {
        name: "Frutiger Aero".into(),
        dark: false,
        palette: Palette {
            background: hex(0xc4e7f8),
            window: hex(0xe8f6fe),
            faint: hex(0xb0dcf3),
            deep: hex(0xffffff),
            text: hex(0x0e2d45),
            widget_text: hex(0x0b2a40),
            strong_text: hex(0x001c30),
            weak_text: hex(0x46708c),
            selection: hex(0x74cbf0),
            accent: hex(0x0a93d1),
            link: hex(0x0072b5),
            warning: hex(0xb87300),
            error: hex(0xcc4238),
            code: hex(0xbde3f5),
            widget: hex(0x9fd8f2),
            widget_hovered: hex(0x82ccee),
            widget_pressed: hex(0x52b8e6),
            widget_outline: hex(0x0a93d1),
            border: hex(0x66b7df),
            playing: default_playing(),
            star: light_star(),
            success: light_success(),
            official: light_official(),
            shadow: light_shadow(),
            syntax: light_syntax(),
        },
        shape: Shape { widget_radius: 10, window_radius: 14, border_width: 1.0, shadow: 22 },
    }
}

/// egui's own look, as it comes.
fn programmer_art() -> Theme {
    let mut theme = from_visuals("Programmer art", &Visuals::dark());
    theme.shape.shadow = 0; // marks "leave the shadows alone" (see `apply`)
    theme
}

/// The palette and shape `visuals` has (as `apply` reads them back).
fn from_visuals(name: &str, visuals: &Visuals) -> Theme {
    let w = &visuals.widgets;
    Theme {
        name: name.into(),
        dark: visuals.dark_mode,
        palette: Palette {
            background: Hex(visuals.panel_fill),
            window: Hex(visuals.window_fill),
            faint: Hex(visuals.faint_bg_color),
            deep: Hex(visuals.extreme_bg_color),
            text: Hex(w.noninteractive.fg_stroke.color),
            widget_text: Hex(w.inactive.fg_stroke.color),
            strong_text: Hex(w.active.fg_stroke.color),
            weak_text: Hex(visuals.weak_text_color()),
            selection: Hex(visuals.selection.bg_fill),
            accent: Hex(visuals.selection.stroke.color),
            link: Hex(visuals.hyperlink_color),
            warning: Hex(visuals.warn_fg_color),
            error: Hex(visuals.error_fg_color),
            code: Hex(visuals.code_bg_color),
            widget: Hex(w.inactive.weak_bg_fill),
            widget_hovered: Hex(w.hovered.weak_bg_fill),
            widget_pressed: Hex(w.active.weak_bg_fill),
            widget_outline: Hex(w.hovered.bg_stroke.color),
            border: Hex(w.noninteractive.bg_stroke.color),
            playing: default_playing(),
            star: default_star(),
            success: default_success(),
            official: default_official(),
            shadow: Hex(visuals.window_shadow.color),
            syntax: default_syntax(),
        },
        shape: Shape {
            widget_radius: w.inactive.corner_radius.nw,
            window_radius: visuals.window_corner_radius.nw,
            border_width: visuals.window_stroke.width,
            shadow: 0,
        },
    }
}

impl Theme {
    pub fn visuals(&self) -> Visuals {
        let p = &self.palette;
        let mut v = if self.dark { Visuals::dark() } else { Visuals::light() };
        v.panel_fill = p.background.0;
        v.window_fill = p.window.0;
        v.faint_bg_color = p.faint.0;
        v.extreme_bg_color = p.deep.0;
        v.text_edit_bg_color = Some(p.deep.0);
        v.code_bg_color = p.code.0;
        v.hyperlink_color = p.link.0;
        v.warn_fg_color = p.warning.0;
        v.error_fg_color = p.error.0;
        v.weak_text_color = Some(p.weak_text.0);
        v.selection.bg_fill = p.selection.0;
        v.selection.stroke.color = p.accent.0;

        let widget_radius = CornerRadius::same(self.shape.widget_radius);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = p.window.0;
        w.noninteractive.weak_bg_fill = p.window.0;
        w.noninteractive.bg_stroke = Stroke::new(self.shape.border_width, p.border.0);
        w.noninteractive.fg_stroke.color = p.text.0;
        for (state, fill) in [
            (&mut w.inactive, p.widget.0),
            (&mut w.hovered, p.widget_hovered.0),
            (&mut w.active, p.widget_pressed.0),
            (&mut w.open, p.widget_hovered.0),
        ] {
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
        }
        w.inactive.fg_stroke.color = p.widget_text.0;
        w.hovered.fg_stroke.color = p.strong_text.0;
        w.active.fg_stroke.color = p.strong_text.0;
        w.open.fg_stroke.color = p.strong_text.0;
        w.hovered.bg_stroke.color = p.widget_outline.0;
        w.active.bg_stroke.color = p.widget_outline.0;
        w.open.bg_stroke.color = p.border.0;
        for state in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
            state.corner_radius = widget_radius;
        }

        v.window_corner_radius = CornerRadius::same(self.shape.window_radius);
        v.menu_corner_radius = CornerRadius::same(self.shape.window_radius.min(10));
        v.window_stroke = Stroke::new(self.shape.border_width, p.border.0);
        v.text_cursor.stroke.color = p.accent.0;
        v.ime_composition.active_underline_stroke.color = p.strong_text.0;
        v.ime_composition.inactive_underline_stroke.color = p.weak_text.0;
        v.window_shadow.color = p.shadow.0;
        v.popup_shadow.color = p.shadow.0;
        if self.shape.shadow > 0 {
            let shadow = Shadow {
                offset: [0, (self.shape.shadow / 4) as i8],
                blur: self.shape.shadow,
                spread: 0,
                color: p.shadow.0,
            };
            v.window_shadow = shadow;
            v.popup_shadow = Shadow { blur: self.shape.shadow / 2, offset: [0, (self.shape.shadow / 8) as i8], ..shadow };
        }
        v
    }

    /// Make this the app's look (every window of it).
    pub fn apply(&self, ctx: &egui::Context) {
        let theme = if self.dark { egui::Theme::Dark } else { egui::Theme::Light };
        ctx.set_theme(theme);
        ctx.set_visuals_of(theme, self.visuals());
        ctx.data_mut(|d| d.insert_temp(palette_id(), self.palette.clone()));
    }
}

/// How tall the top margin of a section's body is: where a section without
/// a tab bar is dragged from (`App::dock_ui`).
pub const DOCK_HANDLE: f32 = 10.0;
/// The other sides' margins of a section's body.
pub const DOCK_MARGIN: i8 = 6;

/// The tabs' look, from the theme's (`style`): the open tab and its panel
/// are one shape in the window's fill (the tab rounded on top, the panel
/// all round), with no outlines; the
/// other tabs are just their names until hovered. Between panels there's
/// only a gap, the divider showing when it's hovered or dragged. Dragging
/// a tab tints where it would land.
pub fn dock_style(style: &egui::Style) -> egui_dock::Style {
    let v = &style.visuals;
    let mut dock = egui_dock::Style::from_egui(style);
    let radius = v.window_corner_radius.nw.min(8);
    let top = CornerRadius { nw: radius, ne: radius, sw: 0, se: 0 };
    dock.tab_bar.bg_fill = v.panel_fill;
    dock.tab_bar.hline_color = Color32::TRANSPARENT;
    dock.tab_bar.corner_radius = CornerRadius::ZERO;
    dock.tab_bar.height = 28.0;
    let open = egui_dock::TabInteractionStyle {
        outline_color: Color32::TRANSPARENT,
        corner_radius: top,
        bg_fill: v.window_fill,
        text_color: v.text_color(),
    };
    let tab = &mut dock.tab;
    tab.inactive = egui_dock::TabInteractionStyle { bg_fill: Color32::TRANSPARENT, text_color: v.weak_text_color(), ..open.clone() };
    tab.inactive_with_kb_focus = tab.inactive.clone();
    tab.hovered = egui_dock::TabInteractionStyle { bg_fill: v.widgets.hovered.weak_bg_fill, text_color: v.strong_text_color(), ..open.clone() };
    tab.focused = egui_dock::TabInteractionStyle { text_color: v.strong_text_color(), ..open.clone() };
    tab.focused_with_kb_focus = tab.focused.clone();
    tab.active_with_kb_focus = open.clone();
    tab.active = open;
    tab.spacing = 2.0;
    tab.hline_below_active_tab_name = false;
    tab.tab_body.stroke = Stroke::NONE;
    tab.tab_body.bg_fill = v.window_fill;
    // (Square under a first tab: `App::dock_ui`.)
    tab.tab_body.corner_radius = CornerRadius::same(radius);
    tab.tab_body.inner_margin = egui::Margin { left: DOCK_MARGIN, right: DOCK_MARGIN, top: DOCK_HANDLE as i8, bottom: DOCK_MARGIN };
    dock.buttons.show_tab_bar_size = 0.0;
    dock.buttons.show_tab_bar_hover_expand = 0.0;
    dock.separator.color_idle = Color32::TRANSPARENT;
    dock.buttons.close_tab_color = v.weak_text_color();
    dock.overlay.overlay_type = egui_dock::OverlayType::HighlightedAreas;
    dock.overlay.selection_color = v.selection.bg_fill.gamma_multiply(0.5);
    dock
}

/// Whether `name` would do for a new theme: not empty, not taken (built-in
/// or your own, ignoring case).
pub fn check_name(name: &str, taken: &[String]) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        bail!("give it a name");
    }
    if name.len() > 40 {
        bail!("that name is too long");
    }
    if taken.iter().any(|t| t.eq_ignore_ascii_case(name)) {
        bail!("there's already a theme called {name}");
    }
    Ok(())
}

/// The first of "Name copy", "Name copy 2", ... not in `taken`.
pub fn copy_name(name: &str, taken: &[String]) -> String {
    let base = format!("{name} copy");
    (1..)
        .map(|n| if n == 1 { base.clone() } else { format!("{base} {n}") })
        .find(|candidate| check_name(candidate, taken).is_ok())
        .unwrap_or(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programmer_art_is_eguis_own_look() {
        let theme = programmer_art();
        let visuals = theme.visuals();
        let stock = Visuals::dark();
        assert_eq!(visuals.panel_fill, stock.panel_fill);
        assert_eq!(visuals.widgets.inactive.weak_bg_fill, stock.widgets.inactive.weak_bg_fill);
        assert_eq!(visuals.widgets.hovered.weak_bg_fill, stock.widgets.hovered.weak_bg_fill);
        assert_eq!(visuals.selection.bg_fill, stock.selection.bg_fill);
        assert_eq!(visuals.window_corner_radius, stock.window_corner_radius);
        assert_eq!(visuals.window_shadow, stock.window_shadow);
        assert_eq!(visuals.widgets.noninteractive.fg_stroke, stock.widgets.noninteractive.fg_stroke);
        assert_eq!(visuals.widgets.inactive.fg_stroke, stock.widgets.inactive.fg_stroke);
        assert_eq!(visuals.widgets.hovered.bg_stroke, stock.widgets.hovered.bg_stroke);
        // And reading a theme back from its own visuals gives the theme.
        for theme in builtins() {
            let mut back = from_visuals(&theme.name, &theme.visuals());
            back.shape.shadow = theme.shape.shadow;
            // (Like the playing color, these aren't egui's.)
            let p = &theme.palette;
            (back.palette.star, back.palette.success, back.palette.official) = (p.star, p.success, p.official);
            back.palette.syntax = p.syntax.clone();
            assert_eq!(back.palette, theme.palette, "{}", theme.name);
        }
    }

    #[test]
    fn themes_round_trip_through_json() {
        for theme in builtins() {
            let json = serde_json::to_string_pretty(&theme).unwrap();
            assert!(json.contains("\"background\": \"#"), "{json}");
            assert_eq!(serde_json::from_str::<Theme>(&json).unwrap(), theme);
        }
        let translucent = Hex(Color32::from_rgba_premultiplied(1, 2, 3, 128));
        let json = serde_json::to_string(&translucent).unwrap();
        assert_eq!(json, "\"#01020380\"");
        assert_eq!(serde_json::from_str::<Hex>(&json).unwrap(), translucent);
        assert!(serde_json::from_str::<Hex>("\"red\"").is_err());
    }

    #[test]
    fn names() {
        let taken: Vec<String> = builtins().into_iter().map(|t| t.name).collect();
        assert!(check_name("dark", &taken).is_err());
        assert!(check_name("  ", &taken).is_err());
        assert!(check_name("Midnight", &taken).is_ok());
        assert_eq!(copy_name("Dark", &taken), "Dark copy");
        let mut more = taken.clone();
        more.push("Dark copy".into());
        assert_eq!(copy_name("Dark", &more), "Dark copy 2");
        assert!(builtins().iter().any(|t| t.name == DEFAULT_THEME));
    }
}
