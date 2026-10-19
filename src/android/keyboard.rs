//! Android's keyboard in the app's text fields, as in any other app: with
//! autocorrect, predictions and gesture typing. winit only shows the
//! keyboard, for NativeActivity's own view, which has no input connection:
//! the keyboard then knows nothing of the field and just sends key presses.
//! So the keyboard is ours instead: a view with an input connection on the
//! Java side (`KeyboardView`), holding a copy of the field being edited.
//!
//! Rust tells Java what the field holds ([`after_frame`], after each frame
//! it changed in), numbering each time; the keyboard edits Java's copy, and
//! each edit comes back whole with the number it was made on ([`typed`]),
//! to be applied to the egui text edit as events before the next frame
//! ([`before_frame`]). An edit made on an older number is dropped: Java has
//! been told what the field holds since, and the keyboard starts over with
//! that. Positions are in characters, as egui counts them.

use std::ops::Range;
use std::sync::Mutex;

use eframe::egui;
use egui::text::{CCursor, CCursorRange};
use egui::text_edit::TextEditState;
use jni::objects::JValue;

use super::call;

/// The field being edited, as both sides have it.
struct Field {
    id: egui::Id,
    /// As Java has it: a password's as egui shows it (masked), with what
    /// the keyboard typed since.
    text: String,
    /// What's selected (empty: the cursor).
    selection: Range<usize>,
    password: bool,
}

/// An edit from the keyboard: the whole field after it.
struct Typed {
    /// The number of what Java was last told when it was made.
    revision: i32,
    text: String,
    selection: Range<usize>,
}

struct State {
    field: Option<Field>,
    /// The number of what Java was last told.
    revision: i32,
    /// The keyboard's latest edit, not yet applied.
    typed: Option<Typed>,
    /// Where the keyboard put the selection with its last edit, for the
    /// frame after it (applied as events, it leaves the cursor after what
    /// it inserted).
    selection: Option<Range<usize>>,
    /// For a frame when the keyboard edits.
    ctx: Option<egui::Context>,
}

static STATE: Mutex<State> = Mutex::new(State { field: None, revision: 0, typed: None, selection: None, ctx: None });

/// From Java: the keyboard edited the field, which now holds `text`, with
/// `selection` selected; made on what Java was told as `revision`.
pub fn typed(revision: i32, text: String, selection: Range<usize>) {
    let mut state = STATE.lock().unwrap();
    state.typed = Some(Typed { revision, text, selection });
    if let Some(ctx) = &state.ctx {
        ctx.request_repaint();
    }
}

/// Apply the keyboard's edit to the field (from the raw input hook).
pub(super) fn before_frame(ctx: &egui::Context, raw_input: &mut egui::RawInput) {
    let mut state = STATE.lock().unwrap();
    state.ctx = Some(ctx.clone());
    let State { field, revision, typed, selection, .. } = &mut *state;
    let typed = typed.take().filter(|typed| typed.revision == *revision);
    let Some(field) = field else {
        *selection = None;
        return;
    };
    let Some(typed) = typed else {
        if let Some(selection) = selection.take() {
            select(ctx, field.id, selection.clone());
            field.selection = selection;
        }
        return;
    };
    match changed(&field.text, &typed.text) {
        Some((replaced, inserted)) => {
            // Select what the keyboard replaced, then replace it.
            select(ctx, field.id, replaced.clone());
            if inserted.is_empty() {
                for pressed in [true, false] {
                    raw_input.events.push(egui::Event::Key {
                        key: egui::Key::Backspace,
                        physical_key: None,
                        pressed,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
            } else {
                raw_input.events.push(egui::Event::Paste(inserted.clone()));
            }
            let cursor = replaced.start + inserted.chars().count();
            field.selection = cursor..cursor;
            if typed.selection != field.selection {
                *selection = Some(typed.selection);
                ctx.request_repaint();
            }
            field.text = typed.text;
        }
        None => {
            select(ctx, field.id, typed.selection.clone());
            field.selection = typed.selection;
            *selection = None;
        }
    }
}

/// Keep the keyboard on the focused text field (from `ui`, after the
/// app's): show it for a field, tell it what the field's own changes were,
/// and hide it once there's none.
pub(super) fn after_frame(ctx: &egui::Context) {
    // winit's keyboard is NativeActivity's: none of that.
    let ime = ctx.output_mut(|o| o.ime.take());
    let focused = ctx.memory(|m| m.focused());
    // The focused field's text, when this frame says it (gaining focus,
    // changed, its selection changed, tapped): only a focused field
    // changes, so the last a text edit says. Tapped, the keyboard shows
    // again, in case it was put away.
    let (edited, tapped) = ctx.output(|o| {
        let mut edited = None;
        let mut tapped = false;
        for event in &o.events {
            let info = event.widget_info();
            if info.typ == egui::WidgetType::TextEdit && info.current_text_value.is_some() {
                edited.clone_from(&info.current_text_value);
                tapped |= matches!(event, egui::output::OutputEvent::Clicked(_));
            }
        }
        (edited, tapped)
    });

    let mut state = STATE.lock().unwrap();
    let (Some(ime), Some(id)) = (ime, focused) else {
        if state.field.take().is_some() {
            hide();
        }
        return;
    };
    let password = ime.purpose == egui::IMEPurpose::Password;
    let selection = TextEditState::load(ctx, id).and_then(|s| s.cursor.char_range()).map(|range| {
        let range = range.as_sorted_char_range();
        range.start.0..range.end.0
    });
    let state = &mut *state;
    match &mut state.field {
        Some(field) if field.id == id => {
            // (egui shows a password masked, and so does Java, but the
            // keyboard's edits to it aren't.)
            let shown = |text: &str| {
                if field.password { mask(text) } else { text.to_string() }
            };
            let text_changed = edited.as_ref().is_some_and(|text| *text != shown(&field.text));
            let selection_changed = selection.as_ref().is_some_and(|s| *s != field.selection);
            let restart = text_changed || password != field.password;
            if restart || selection_changed {
                if let Some(text) = edited.filter(|_| text_changed) {
                    field.text = text;
                }
                if let Some(selection) = selection {
                    field.selection = selection;
                }
                field.password = password;
                state.revision += 1;
                state.typed = None;
                state.selection = None;
                edit(state.revision, field, restart);
            }
            if tapped {
                show();
            }
        }
        _ => {
            // A field the keyboard hasn't had yet: once its text is known.
            let Some(text) = edited else {
                if state.field.take().is_some() {
                    hide();
                }
                return;
            };
            let end = text.chars().count();
            let field = state.field.insert(Field { id, text, selection: selection.unwrap_or(end..end), password });
            state.revision += 1;
            state.typed = None;
            state.selection = None;
            edit(state.revision, field, true);
            show();
        }
    }
}

/// Select `range` in the text edit `id`, for its next frame.
fn select(ctx: &egui::Context, id: egui::Id, range: Range<usize>) {
    let mut state = TextEditState::load(ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(range.start), CCursor::new(range.end))));
    state.store(ctx, id);
}

/// What changed from `old` to `new`: the characters of `old` replaced, and
/// what replaced them; `None` if nothing did.
fn changed(old: &str, new: &str) -> Option<(Range<usize>, String)> {
    if old == new {
        return None;
    }
    let old: Vec<char> = old.chars().collect();
    let new: Vec<char> = new.chars().collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..].iter().rev().zip(new[prefix..].iter().rev()).take_while(|(a, b)| a == b).count();
    Some((prefix..old.len() - suffix, new[prefix..new.len() - suffix].iter().collect()))
}

/// `text` as egui shows a password.
fn mask(text: &str) -> String {
    text.chars().map(|_| egui::epaint::text::PASSWORD_REPLACEMENT_CHAR).collect()
}

/// Tell Java what `field` holds, as `revision`; `restart`: the keyboard
/// starts over with it (a new field, or text it didn't type), rather than
/// just moving the selection.
fn edit(revision: i32, field: &Field, restart: bool) {
    call("editing a text field", |env, activity| {
        let text = env.new_string(&field.text)?;
        env.call_method(
            activity,
            "editText",
            "(ILjava/lang/String;IIZZ)V",
            &[
                JValue::Int(revision),
                JValue::Object(&text),
                JValue::Int(field.selection.start as i32),
                JValue::Int(field.selection.end as i32),
                JValue::Bool(field.password.into()),
                JValue::Bool(restart.into()),
            ],
        )?;
        Ok(())
    });
}

fn show() {
    call("showing the keyboard", |env, activity| {
        env.call_method(activity, "showKeyboard", "()V", &[])?;
        Ok(())
    });
}

fn hide() {
    call("hiding the keyboard", |env, activity| {
        env.call_method(activity, "hideKeyboard", "()V", &[])?;
        Ok(())
    });
}
