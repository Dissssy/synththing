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
}

fn yes() -> bool {
    true
}

impl Panel {
    /// The panel in `json`, if it's one synththing can show: why not if not.
    pub fn parse(json: &[u8]) -> Result<Panel, String> {
        let panel: Panel = serde_json::from_slice(json).map_err(|e| format!("not a panel: {e}"))?;
        panel.check()?;
        Ok(panel)
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
                        walk(elements, depth + 1, ids)?;
                        Vec::new()
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

/// What the user did with a panel, as its program's `panel` event says.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Event {
    pub element: String,
    pub kind: EventKind,
    /// A textbox's text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A textbox's text changed.
    Changed,
    /// Enter in a one-line textbox.
    Submitted,
    Clicked,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panels_are_read_and_checked() {
        let panel = Panel::parse(
            br#"{"title": "Finder", "elements": [
                {"id": "q", "type": "textbox", "hint": "Search"},
                {"id": "bar", "type": "row", "elements": [
                    {"id": "go", "type": "button", "text": "Go", "enabled": false},
                    {"id": "status", "type": "label", "text": "Ready", "weak": true}]}]}"#,
        )
        .unwrap();
        assert_eq!(panel.elements[0].kind, Kind::Textbox { hint: "Search".into(), value: None, multiline: false });
        let Kind::Row { elements } = &panel.elements[1].kind else { panic!("a row") };
        assert_eq!(elements[0].kind, Kind::Button { text: "Go".into(), enabled: false });

        let refused = |json: &str| Panel::parse(json.as_bytes()).unwrap_err();
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
        let event = Event { element: "q".into(), kind: EventKind::Changed, value: Some("overw".into()) };
        assert_eq!(serde_json::to_string(&event).unwrap(), r#"{"element":"q","kind":"changed","value":"overw"}"#);
        let event = Event { element: "go".into(), kind: EventKind::Clicked, value: None };
        assert_eq!(serde_json::to_string(&event).unwrap(), r#"{"element":"go","kind":"clicked"}"#);
    }
}
