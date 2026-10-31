//! Art an album could be shown with, from online (Choose art): a release's
//! pictures at the Cover Art Archive, or the albums the iTunes Store has
//! by a name; and an artist's picture, from Wikidata.

use anyhow::{Context, Result};
use serde_json::Value;

/// A picture an album could have.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    /// Where it is, at about 500 to 600 pixels.
    pub url: String,
    /// What it is ("Front", "Back", or the album it's of).
    pub label: String,
}

fn get(url: &str, query: &[(&str, &str)]) -> Result<Option<Value>> {
    let agent = format!("synththing/{} ( https://github.com/Dissssy/synththing )", crate::build_info::VERSION);
    let mut request = ureq::get(url).header("User-Agent", &agent);
    for (name, value) in query {
        request = request.query(*name, *value);
    }
    match request.call() {
        Ok(mut response) => Ok(Some(response.body_mut().read_json()?)),
        Err(ureq::Error::StatusCode(404)) => Ok(None),
        Err(e) => Err(e).with_context(|| format!("asking {url}")),
    }
}

/// The pictures the Cover Art Archive has of `release` (else of its
/// `group`'s), its fronts first.
pub fn cover_art_archive(release: &str, group: Option<&str>) -> Result<Vec<Choice>> {
    let mut found = get(&format!("https://coverartarchive.org/release/{release}"), &[])?;
    if found.is_none()
        && let Some(group) = group
    {
        found = get(&format!("https://coverartarchive.org/release-group/{group}"), &[])?;
    }
    let Some(found) = found else { return Ok(Vec::new()) };
    Ok(caa_choices(&found))
}

fn caa_choices(found: &Value) -> Vec<Choice> {
    let mut choices: Vec<(bool, Choice)> = found["images"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|image| {
            let thumbnails = &image["thumbnails"];
            let url = thumbnails["500"].as_str().or(thumbnails["large"].as_str()).or(image["image"].as_str())?;
            let types: Vec<&str> = image["types"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            let label = if types.is_empty() { "Picture".to_string() } else { types.join(", ") };
            Some((image["front"].as_bool() == Some(true), Choice { url: url.replace("http://", "https://"), label }))
        })
        .collect();
    choices.sort_by_key(|(front, _)| !front);
    choices.into_iter().map(|(_, c)| c).collect()
}

/// An album the iTunes Store has.
#[derive(Clone, Debug, PartialEq)]
pub struct Listed {
    pub name: String,
    pub artist: String,
    /// Its cover, at 600 pixels.
    pub url: String,
}

impl Listed {
    /// It as a picture an album could have, labeled with what it is.
    pub fn choice(&self) -> Choice {
        Choice { url: self.url.clone(), label: format!("{} - {}", self.name, self.artist) }
    }
}

/// The albums the iTunes Store has by `term` (an album's name and artist),
/// with their covers.
pub fn itunes(term: &str) -> Result<Vec<Listed>> {
    let found = get("https://itunes.apple.com/search", &[("term", term), ("entity", "album"), ("limit", "12")])?;
    Ok(found.map(|f| itunes_albums(&f)).unwrap_or_default())
}

fn itunes_albums(found: &Value) -> Vec<Listed> {
    found["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|album| {
            // (Its 100-pixel cover's address names its size: any size can be asked for.)
            let url = album["artworkUrl100"].as_str()?.replace("100x100bb", "600x600bb");
            let text = |name: &str| album[name].as_str().unwrap_or("?").to_string();
            Some(Listed { name: text("collectionName"), artist: text("artistName"), url })
        })
        .collect()
}

/// Where the picture of the Wikidata item `id` (a Q-id) is, at 512
/// pixels, at Wikimedia Commons: its image (P18), if it has one.
pub fn wikidata_picture(id: &str) -> Result<Option<String>> {
    let Some(found) = get(&format!("https://www.wikidata.org/wiki/Special:EntityData/{id}.json"), &[])? else { return Ok(None) };
    Ok(wikidata_file(&found, id).map(|file| format!("https://commons.wikimedia.org/wiki/Special:FilePath/{}?width=512", file.replace(' ', "_"))))
}

fn wikidata_file<'v>(found: &'v Value, id: &str) -> Option<&'v str> {
    found["entities"][id]["claims"]["P18"].as_array()?.iter().find_map(|claim| claim["mainsnak"]["datavalue"]["value"].as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_read_from_answers() {
        let caa = serde_json::json!({"images": [
            {"image": "http://a/back.jpg", "thumbnails": {"500": "http://a/back-500.jpg"}, "types": ["Back"], "front": false},
            {"image": "http://a/front.jpg", "thumbnails": {"large": "http://a/front-500.jpg"}, "types": ["Front", "Obi"], "front": true},
        ]});
        let choices = caa_choices(&caa);
        assert_eq!(choices[0], Choice { url: "https://a/front-500.jpg".into(), label: "Front, Obi".into() });
        assert_eq!(choices[1].label, "Back");
        let itunes = serde_json::json!({"results": [
            {"collectionName": "Moonmadness", "artistName": "Camel", "artworkUrl100": "https://is1/x/100x100bb.jpg"},
        ]});
        let listed = itunes_albums(&itunes);
        assert_eq!(listed, vec![Listed { name: "Moonmadness".into(), artist: "Camel".into(), url: "https://is1/x/600x600bb.jpg".into() }]);
        assert_eq!(listed[0].choice(), Choice { url: "https://is1/x/600x600bb.jpg".into(), label: "Moonmadness - Camel".into() });
        let wikidata = serde_json::json!({"entities": {"Q1": {"claims": {"P18": [{"mainsnak": {"datavalue": {"value": "Camel band.jpg"}}}]}}}});
        assert_eq!(wikidata_file(&wikidata, "Q1"), Some("Camel band.jpg"));
    }
}
