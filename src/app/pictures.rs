//! Picking a picture of one's own (`images.rs`) for a playlist, an album
//! or an artist: the platform's picker opens (`import::Kind::Image`), and
//! once it's kept, what it was picked for shows it.

use super::App;
use crate::song_store::Art;

/// What a picture's being picked for.
pub(super) enum PictureFor {
    /// A playlist, by ID.
    Playlist(String),
    /// An album: its songs, by key.
    Album(Vec<String>),
    /// An artist, by name (`fields::group_key`).
    Artist(String),
}

impl App {
    /// Ask for a picture for `what` (it's shown with it once it's kept:
    /// `picture_picked`).
    pub(super) fn pick_picture(&mut self, what: PictureFor) {
        self.picture_for = Some(what);
        crate::import::start(crate::import::Kind::Image, &["jpg", "jpeg", "png"]);
    }

    /// The picture asked for is kept as `image`.
    pub(super) fn picture_picked(&mut self, image: String) {
        match self.picture_for.take() {
            Some(PictureFor::Playlist(id)) => {
                if let Some(list) = self.playlists.find(&id) {
                    self.set_playlist_image(list, Some(image));
                }
            }
            Some(PictureFor::Album(keys)) => self.choose_art(&keys, Some(Art::Image(image))),
            Some(PictureFor::Artist(name)) => self.set_artist_image(&name, Some(image)),
            None => {}
        }
    }

    /// Show the artist `name` (`fields::group_key`) with the picture kept
    /// as `image` (`None`: its initials).
    pub(super) fn set_artist_image(&mut self, name: &str, image: Option<String>) {
        self.songs.set_artist_image(name, image);
        self.songs_changed();
    }
}
