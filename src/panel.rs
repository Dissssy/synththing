//! App panels (docs/API.md, "Panels"): a program using the local API puts
//! a small UI of its own in the Apps tab, built from a few elements, and
//! hears what the user does with it on its event stream. This is the
//! panel as the program sends it, and the events it gets back; the API
//! keeps each program's (`api.rs`) and the app draws them
//! (`app/panels.rs`).

use serde::{Deserialize, Serialize};

/// The largest panel a program can send, in bytes of JSON.
pub const LARGEST: usize = 64 << 10;
/// Elements in one panel, counting those inside rows.
const MOST_ELEMENTS: usize = 200;
/// Characters in any one text, the title included.
const LONGEST_TEXT: usize = 500;
/// Rows inside rows.
const DEEPEST: usize = 4;
/// Items in one list.
const MOST_ITEMS: usize = 2000;
/// Buttons on one list item.
const MOST_ACTIONS: usize = 2;

/// A program's panel.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Panel {
    pub title: String,
    pub elements: Vec<Element>,
}

/// One element, with the ID the program gave it (what events name it by).
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Element {
    pub id: String,
    #[serde(flatten)]
    pub kind: Kind,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Kind {
    Label {
        text: String,
        /// Greyed, for less important text.
        #[serde(default)]
        weak: bool,
    },
    Textbox {
        /// Shown while it's empty.
        #[serde(default)]
        hint: String,
        /// What it says: given, it replaces what's typed (unless the user is
        /// typing in it); left out, what's typed stays.
        value: Option<String>,
        #[serde(default)]
        multiline: bool,
    },
    Button {
        text: String,
        #[serde(default = "yes")]
        enabled: bool,
    },
    /// Its elements side by side.
    Row { elements: Vec<Element> },
    /// Rows to pick from, sharing the height left under the elements
    /// above with any other lists.
    List { items: Vec<Item> },
}

/// A list's row.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Item {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub subtitle: String,
    /// What in synththing it is: it then shows that's picture, and clicking
    /// it goes there.
    pub link: Option<Link>,
    /// Buttons at its end.
    #[serde(default)]
    pub actions: Vec<String>,
}

/// Something in synththing an item can stand for.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Link {
    /// A song, by its key: clicking plays it.
    Song(String),
    /// A wish, by its ID: clicking shows the Wishlist.
    Wish(String),
}

fn yes() -> bool {
    true
}

impl Panel {
    /// The panel in `json`, if it's one synththing can show: why not if not.
    pub fn parse(json: &serde_json::Value) -> Result<Panel, String> {
        let panel = Panel::deserialize(json).map_err(|e| format!("not a panel: {e}"))?;
        panel.check()?;
        Ok(panel)
    }

    /// The textboxes given a `value`: their IDs and values.
    pub fn values(&self) -> Vec<(String, String)> {
        fn walk(elements: &[Element], out: &mut Vec<(String, String)>) {
            for element in elements {
                match &element.kind {
                    Kind::Textbox { value: Some(value), .. } => out.push((element.id.clone(), value.clone())),
                    Kind::Row { elements } => walk(elements, out),
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.elements, &mut out);
        out
    }

    fn check(&self) -> Result<(), String> {
        fn walk<'a>(elements: &'a [Element], depth: usize, ids: &mut Vec<&'a str>) -> Result<(), String> {
            // (`depth` counts the rows around these elements, plus one.)
            if depth > DEEPEST + 1 {
                return Err(format!("rows go at most {DEEPEST} deep"));
            }
            for element in elements {
                if element.id.is_empty() {
                    return Err("every element needs an id".into());
                }
                if ids.contains(&element.id.as_str()) {
                    return Err(format!("two elements are called \"{}\"", element.id));
                }
                ids.push(&element.id);
                let texts: Vec<&String> = match &element.kind {
                    Kind::Label { text, .. } | Kind::Button { text, .. } => vec![text],
                    Kind::Textbox { hint, value, .. } => vec![hint].into_iter().chain(value).collect(),
                    Kind::Row { elements } => {
                        if let Some(list) = elements.iter().find(|e| matches!(e.kind, Kind::List { .. })) {
                            return Err(format!("\"{}\" is a list: lists can't go in a row", list.id));
                        }
                        walk(elements, depth + 1, ids)?;
                        Vec::new()
                    }
                    Kind::List { items } => {
                        if items.len() > MOST_ITEMS {
                            return Err(format!("\"{}\" has more than {MOST_ITEMS} items", element.id));
                        }
                        let mut item_ids = std::collections::HashSet::new();
                        for item in items {
                            if item.id.is_empty() || !item_ids.insert(&item.id) {
                                return Err(format!("every item of \"{}\" needs an id of its own", element.id));
                            }
                            if item.actions.len() > MOST_ACTIONS {
                                return Err(format!("an item has at most {MOST_ACTIONS} actions"));
                            }
                        }
                        items.iter().flat_map(|i| [&i.text, &i.subtitle].into_iter().chain(&i.actions)).collect()
                    }
                };
                if texts.iter().any(|t| t.chars().count() > LONGEST_TEXT) {
                    return Err(format!("\"{}\" has more than {LONGEST_TEXT} characters", element.id));
                }
            }
            Ok(())
        }
        if self.title.chars().count() > LONGEST_TEXT {
            return Err(format!("the title has more than {LONGEST_TEXT} characters"));
        }
        let mut ids = Vec::new();
        walk(&self.elements, 1, &mut ids)?;
        if ids.len() > MOST_ELEMENTS {
            return Err(format!("a panel has at most {MOST_ELEMENTS} elements"));
        }
        Ok(())
    }
}

/// Change the fields in `changes` (a JSON object) of the element `id` in
/// `panel` (a panel's JSON): `Err` with its status if there's no such
/// element (404) or the change can't be made (400). The panel is checked
/// as a whole again after (`Panel::parse`).
pub fn patch(panel: &mut serde_json::Value, id: &str, changes: serde_json::Value) -> Result<(), (u16, String)> {
    fn find<'a>(elements: &'a mut serde_json::Value, id: &str) -> Option<&'a mut serde_json::Map<String, serde_json::Value>> {
        for element in elements.as_array_mut()? {
            let element = element.as_object_mut()?;
            if element.get("id").and_then(|i| i.as_str()) == Some(id) {
                return Some(element);
            }
            if let Some(found) = element.get_mut("elements").and_then(|inner| find(inner, id)) {
                return Some(found);
            }
        }
        None
    }
    let serde_json::Value::Object(changes) = changes else { return Err((400, "send the fields to change as a JSON object".into())) };
    if changes.contains_key("id") || changes.contains_key("type") {
        return Err((400, "an element's id and type stay as they are: send the whole panel to change them".into()));
    }
    let element = panel.get_mut("elements").and_then(|elements| find(elements, id)).ok_or((404, format!("there's no element \"{id}\"")))?;
    element.extend(changes);
    Ok(())
}

/// What the user did with a panel, as its program's `panel` event says.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Event {
    /// The element (none for `shown` and `hidden`, about the panel).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub element: Option<String>,
    pub kind: EventKind,
    /// A textbox's text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// A list's item picked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// The item's action clicked (none: the item itself).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

impl Event {
    pub fn of(element: &str, kind: EventKind) -> Self {
        Event { element: Some(element.to_string()), kind, ..Event::default() }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A textbox's text changed.
    #[default]
    Changed,
    /// Enter in a one-line textbox.
    Submitted,
    Clicked,
    /// A list's item (or one of its actions) was clicked.
    Picked,
    /// The panel came into view.
    Shown,
    /// The panel went out of view.
    Hidden,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Result<Panel, String> {
        Panel::parse(&serde_json::from_str(json).unwrap())
    }

    #[test]
    fn panels_are_read_and_checked() {
        let panel = parse(
            r#"{"title": "Finder", "elements": [
                {"id": "q", "type": "textbox", "hint": "Search"},
                {"id": "bar", "type": "row", "elements": [
                    {"id": "go", "type": "button", "text": "Go", "enabled": false},
                    {"id": "status", "type": "label", "text": "Ready", "weak": true}]}]}"#,
        )
        .unwrap();
        assert_eq!(panel.elements[0].kind, Kind::Textbox { hint: "Search".into(), value: None, multiline: false });
        let Kind::Row { elements } = &panel.elements[1].kind else { panic!("a row") };
        assert_eq!(elements[0].kind, Kind::Button { text: "Go".into(), enabled: false });

        let refused = |json: &str| parse(json).unwrap_err();
        assert!(refused(r#"{"title": "", "elements": [{"id": "a", "type": "slider"}]}"#).starts_with("not a panel"));
        assert!(refused(r#"{"title": "", "elements": [{"id": "a", "type": "label", "text": ""}, {"id": "a", "type": "label", "text": ""}]}"#).contains("two elements"));
        assert!(refused(r#"{"title": "", "elements": [{"id": "", "type": "label", "text": ""}]}"#).contains("needs an id"));
        let long = "x".repeat(LONGEST_TEXT + 1);
        assert!(refused(&format!(r#"{{"title": "", "elements": [{{"id": "a", "type": "label", "text": "{long}"}}]}}"#)).contains("characters"));
        let deep = r#"{"id": "r", "type": "row", "elements": [X]}"#;
        let mut nested = r#"{"id": "l", "type": "label", "text": ""}"#.to_string();
        for i in 0..=DEEPEST {
            nested = deep.replace("X", &nested).replace("\"r\"", &format!("\"r{i}\""));
        }
        assert!(refused(&format!(r#"{{"title": "", "elements": [{nested}]}}"#)).contains("deep"));
    }

    #[test]
    fn events_read_as_the_docs_say() {
        let event = Event { value: Some("overw".into()), ..Event::of("q", EventKind::Changed) };
        assert_eq!(serde_json::to_string(&event).unwrap(), r#"{"element":"q","kind":"changed","value":"overw"}"#);
        assert_eq!(serde_json::to_string(&Event::of("go", EventKind::Clicked)).unwrap(), r#"{"element":"go","kind":"clicked"}"#);
        let event = Event { item: Some("r1".into()), action: Some("Get".into()), ..Event::of("results", EventKind::Picked) };
        assert_eq!(serde_json::to_string(&event).unwrap(), r#"{"element":"results","kind":"picked","item":"r1","action":"Get"}"#);
        let event = Event { kind: EventKind::Shown, ..Event::default() };
        assert_eq!(serde_json::to_string(&event).unwrap(), r#"{"kind":"shown"}"#);
    }

    #[test]
    fn lists_hold_items_with_links() {
        let panel = parse(
            r#"{"title": "", "elements": [{"id": "results", "type": "list", "items": [
                {"id": "r1", "text": "Overworld", "subtitle": "Koji Kondo", "link": {"song": "3f2a"}, "actions": ["Get"]},
                {"id": "r2", "text": "Modal Soul", "link": {"wish": "w1"}}]}]}"#,
        )
        .unwrap();
        let Kind::List { items } = &panel.elements[0].kind else { panic!("a list") };
        assert_eq!(items[0].link, Some(Link::Song("3f2a".into())));
        assert_eq!(items[1].link, Some(Link::Wish("w1".into())));
        let refused = |json: &str| parse(json).unwrap_err();
        assert!(refused(r#"{"title": "", "elements": [{"id": "r", "type": "row", "elements": [{"id": "l", "type": "list", "items": []}]}]}"#).contains("can't go in a row"));
        assert!(refused(r#"{"title": "", "elements": [{"id": "l", "type": "list", "items": [{"id": "a", "text": ""}, {"id": "a", "text": ""}]}]}"#).contains("of its own"));
        assert!(refused(r#"{"title": "", "elements": [{"id": "l", "type": "list", "items": [{"id": "a", "text": "", "actions": ["1", "2", "3"]}]}]}"#).contains("actions"));
    }

    #[test]
    fn one_element_is_changed_by_a_patch() {
        let mut json: serde_json::Value = serde_json::from_str(
            r#"{"title": "", "elements": [{"id": "bar", "type": "row", "elements": [{"id": "status", "type": "label", "text": "Ready"}]}]}"#,
        )
        .unwrap();
        patch(&mut json, "status", serde_json::json!({"text": "Searching"})).unwrap();
        let Kind::Row { elements } = &Panel::parse(&json).unwrap().elements[0].kind else { panic!("a row") };
        assert_eq!(elements[0].kind, Kind::Label { text: "Searching".into(), weak: false });
        assert_eq!(patch(&mut json, "nope", serde_json::json!({})).unwrap_err().0, 404);
        assert_eq!(patch(&mut json, "status", serde_json::json!({"type": "button"})).unwrap_err().0, 400);
        assert_eq!(patch(&mut json, "status", serde_json::json!([1])).unwrap_err().0, 400);
    }
}
