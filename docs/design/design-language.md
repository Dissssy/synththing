# Design language

How synththing's interface looks, so every panel and page reads as one app.
The Songs list and the Groupings view are the references; Preferences is
built entirely from these pieces.

## Rows

- Everything that lists things is rows of one height: the standard
  interact height, with the same padding top and bottom.
- Rows alternate their background: every other one on the faint (striped)
  color.
- Hovered: a rounded rect with a thin neutral outline. Selected: the blue
  selection fill with accent text. Playing: a green tinted fill, green
  outline and green text. Toggled buttons use the selected rows' fill.
- What a row is about goes on the left; its count or status in weak text and
  its actions as icon buttons go at the right end, right-aligned so they line
  up down the list. An action that belongs to the row's name (remove,
  rename) sits right next to the name instead.
- A cell grid (like program permissions) shades alternate cells in a
  checkerboard, so a cell's row and column are easy to follow.
- Shared list behavior (column headers, resizing, widths) lives in the
  shared list code (`select_list`), never per list.

## Sections

- Settings and other grouped content sit in sections drawn like a list
  (`settings_group` in `src/app.rs`): outlined with rounded corners, a header
  row on the darkest (extreme) background with an icon, the title, and an
  optional weak hint line under it; the rows (`setting_row`) striped below.
- A section that is long or rarely needed is closable (`.closable(id)`): the
  header toggles it, with a caret at its right, and remembers it.
- A long list inside a section folds by its own rows: a caret on a row opens
  its items as indented rows under it, rather than showing everything.
- A section only appears when it has something in it.
- The gap between sections equals the room at their sides.
- Something that is one thing, not a list (the account's identity), may be a
  card of its own above the sections: the faint fill, an outline, a larger
  corner radius.

## Settings

- One setting per row. A label column (`setting_label`, a fixed width) then
  the control at a fixed width (`CONTROL_WIDTH`), so controls line up down
  the page.
- The longer explanation is an (i) at the right end of the row
  (`with_info`); hovering the row or the (i) shows it. No paragraphs of help
  text above settings.
- Warnings and errors about a setting are a row of their own under it, in
  the warning or error color.
- Pages in Preferences are pills with icons down the side; Preferences shows
  in place of the panels, so playback and the visualizer keep going.

## Buttons and icons

- Icons are Phosphor (`icon(name)`). Buttons with only an icon carry hover
  text saying what they do.
- Buttons inside rows use `row_icon_button`, sized to the row height so the
  row stays centered. Elsewhere, `icon_button`.
- An on/off choice shown as an icon is a toggle button: the selected fill
  when on, and hover text that says the state and what a click does.
- An icon toggle that changes playback shows a toast with its new state.
- Several choices from a set are toggle chips (`toggle_chip`), grouped into
  rows by kind.
- Title bar buttons all use `title_bar::bar_button`. Menus with options use
  `menu::config` (Shift+click keeps them open).
- Something that can't be undone (removing a program, deleting) asks first
  in a small dialog: the question as its heading, one line on what happens,
  the destructive button in the error color, then Cancel. Esc closes only
  the dialog.

## Text

- The UI font everywhere, with tabular digits for times and numbers; the
  pixel font only inside the visualizer.
- Titles in strong text, secondary lines and counts in weak text.
- Long names are cut short with an ellipsis, never wrapped, in rows; hover
  shows the full name where it matters.
