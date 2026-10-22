//! Image decoding with the `image` crate (JPEG and PNG).

use std::path::Path;

use image::{DynamicImage, ImageDecoder, ImageReader, RgbImage};

/// Decodes the file and applies the EXIF orientation, like gdk-pixbuf does in swaybg.
/// Wallpapers are opaque, so alpha (and 16-bit depth) is dropped right away.
pub fn load(path: &Path) -> image::ImageResult<RgbImage> {
    let mut decoder = ImageReader::open(path)?
        .with_guessed_format()?
        .into_decoder()?;
    let orientation = decoder.orientation()?;
    let mut img = DynamicImage::from_decoder(decoder)?;
    img.apply_orientation(orientation);
    Ok(img.into_rgb8())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgb};

    #[test]
    fn loads_webp() {
        let img = RgbImage::from_fn(9, 5, |x, y| Rgb([(x * 20) as u8, (y * 40) as u8, 7]));
        let path = std::env::temp_dir().join(format!("reel-test-{}.webp", std::process::id()));
        img.save_with_format(&path, ImageFormat::WebP).unwrap();
        let loaded = load(&path);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(loaded.unwrap(), img);
    }
}
