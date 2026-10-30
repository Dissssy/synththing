//! Pictures the user picks for a playlist (later an album or artist), kept
//! like songs: by their hash (`<sha256>.jpg`, a key as `song_store` makes
//! them), in a subfolder named by its first two characters, under
//! `images/` in the config directory. A picture is made smaller (at most
//! `SIZE` on its longer side) and kept as a JPEG, so the same pick is the
//! same picture on every device.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

/// The longest side a kept picture has.
const SIZE: u32 = 512;

/// How good a kept picture's JPEG is.
const QUALITY: u8 = 85;

/// The folder the pictures are kept in.
pub fn dir() -> Result<PathBuf> {
    Ok(crate::config::config_dir()?.join("images"))
}

/// Where the picture `key` is kept.
pub fn path(key: &str) -> Result<PathBuf> {
    Ok(crate::song_store::path_of(&dir()?, key))
}

/// Keep the picture in `bytes` (a JPEG or PNG): its key.
pub fn add(bytes: &[u8]) -> Result<String> {
    add_in(&dir()?, bytes)
}

/// Keep the picture in the file at `file`: its key.
pub fn add_file(file: &Path) -> Result<String> {
    let bytes = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    add(&bytes)
}

fn add_in(dir: &Path, bytes: &[u8]) -> Result<String> {
    let image = image::load_from_memory(bytes).context("not a picture synththing can read (a JPEG or PNG)")?;
    let image = if image.width().max(image.height()) > SIZE { image.resize(SIZE, SIZE, image::imageops::FilterType::Lanczos3) } else { image };
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, QUALITY).encode_image(&image.to_rgb8()).context("making the picture a JPEG")?;
    let hex: String = Sha256::digest(&jpeg).iter().map(|b| format!("{b:02x}")).collect();
    let key = format!("{hex}.jpg");
    let path = crate::song_store::path_of(dir, &key);
    if !path.exists() {
        let folder = path.parent().context("no folder")?;
        std::fs::create_dir_all(folder).with_context(|| format!("making {}", folder.display()))?;
        let temp = path.with_extension("part");
        std::fs::write(&temp, &jpeg).with_context(|| format!("writing {}", temp.display()))?;
        std::fs::rename(&temp, &path).with_context(|| format!("keeping {}", path.display()))?;
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("synththing-images-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbImage::from_pixel(width, height, image::Rgb([200, 40, 90]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    #[test]
    fn a_picture_is_kept_smaller_by_its_hash() {
        let dir = temp_dir("kept");
        let key = add_in(&dir, &png(1200, 600)).unwrap();
        assert!(crate::song_store::is_key(&key) && key.ends_with(".jpg"), "{key}");
        let kept = image::load_from_memory(&std::fs::read(crate::song_store::path_of(&dir, &key)).unwrap()).unwrap();
        assert_eq!((kept.width(), kept.height()), (SIZE, SIZE / 2));
        assert_eq!(add_in(&dir, &png(1200, 600)).unwrap(), key, "the same pick, the same key");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_isnt_a_picture_is_refused() {
        assert!(add_in(&temp_dir("refused"), b"not a picture").is_err());
    }
}
