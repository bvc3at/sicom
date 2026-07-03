use anyhow::{Context, Result};
use std::path::Path;

pub fn is_supported_image(filename: &str) -> bool {
    let path = Path::new(filename);
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| matches!(ext.to_lowercase().as_str(), "jpg" | "jpeg" | "png" | "webp"))
}

/// libwebp rejects dimensions above this; the `webp` crate panics on them.
const WEBP_MAX_DIMENSION: u32 = 16383;

/// True for animated WebP (VP8X animation flag) or APNG (acTL chunk):
/// re-encoding those through a still-image pipeline would keep only the first
/// frame.
fn is_animated(data: &[u8]) -> bool {
    // WebP: RIFF....WEBPVP8X, animation is bit 1 of the VP8X flags byte.
    if data.len() > 20 && &data[0..4] == b"RIFF" && data[8..16] == *b"WEBPVP8X" {
        return data[20] & 0x02 != 0;
    }
    // APNG: an acTL chunk appears before the first IDAT.
    if data.starts_with(&[0x89, b'P', b'N', b'G']) {
        let first_idat = data.windows(4).position(|w| w == b"IDAT");
        let actl = data.windows(4).position(|w| w == b"acTL");
        return match (actl, first_idat) {
            (Some(a), Some(d)) => a < d,
            (Some(_), None) => true,
            _ => false,
        };
    }
    false
}

pub fn compress_image_file(
    data: &[u8],
    filename: &str,
    quality: u8,
) -> Result<(Vec<u8>, u64, u64)> {
    let original_size = data.len() as u64;

    if is_animated(data) {
        return Err(anyhow::anyhow!(
            "Animated image would lose its animation: {filename}"
        ));
    }

    // Load image (detect format from data, not extension)
    let img = image::load_from_memory(data)
        .with_context(|| format!("Failed to decode image: {filename}"))?;

    if img.width() > WEBP_MAX_DIMENSION || img.height() > WEBP_MAX_DIMENSION {
        return Err(anyhow::anyhow!(
            "Image dimensions {}x{} exceed the WebP limit of {WEBP_MAX_DIMENSION}: {filename}",
            img.width(),
            img.height()
        ));
    }

    // Always convert to WebP format for maximum compression
    let compressed_data = {
        let mut buffer = Vec::new();

        // Use webp crate directly for quality control
        let width = img.width();
        let height = img.height();
        let rgba_img = img.to_rgba8();

        let webp_encoder = webp::Encoder::new(&rgba_img, webp::PixelLayout::Rgba, width, height);
        if quality >= 95 {
            // Use lossless for high quality
            let encoded_data = webp_encoder.encode_lossless();
            buffer.extend_from_slice(&encoded_data);
        } else {
            // Use lossy compression with quality parameter
            let encoded_data = webp_encoder.encode(f32::from(quality));
            buffer.extend_from_slice(&encoded_data);
        }
        buffer
    };

    let compressed_size = compressed_data.len() as u64;
    Ok((compressed_data, original_size, compressed_size))
}

/// Convert image filename to WebP extension.
///
/// ZIP entry names are plain '/'-separated strings, so this works on the
/// string directly: `std::path::Path` would reinterpret separators
/// platform-dependently (e.g. '\\' on Windows), changing more than the
/// extension.
pub fn to_webp_filename(filename: &str) -> String {
    let segment_start = filename.rfind('/').map_or(0, |pos| pos + 1);
    match filename[segment_start..].rfind('.') {
        Some(dot) => format!("{}.webp", &filename[..segment_start + dot]),
        None => format!("{filename}.webp"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_supported_image() {
        assert!(is_supported_image("Images/test.jpg"));
        assert!(is_supported_image("Images/test.jpeg"));
        assert!(is_supported_image("Images/test.png"));
        assert!(is_supported_image("Images/test.webp"));
        assert!(is_supported_image("Images/test.JPG"));
        assert!(!is_supported_image("Images/test.gif"));
        assert!(!is_supported_image("Images/test.bmp"));
        assert!(!is_supported_image("Audio/test.mp3"));
        assert!(!is_supported_image("content.xml"));
    }

    #[test]
    fn test_to_webp_filename() {
        // Test basic conversion
        assert_eq!(to_webp_filename("Images/test.jpg"), "Images/test.webp");
        assert_eq!(to_webp_filename("Images/test.jpeg"), "Images/test.webp");
        assert_eq!(to_webp_filename("Images/test.png"), "Images/test.webp");
        assert_eq!(to_webp_filename("Images/test.webp"), "Images/test.webp");

        // Test with UTF-8 characters (like in the sample pack)
        assert_eq!(
            to_webp_filename("Images/КимЧенИр. Северная Корея.jpg"),
            "Images/КимЧенИр. Северная Корея.webp"
        );
        assert_eq!(
            to_webp_filename("Images/ВДНХ.Москва~2.jpg"),
            "Images/ВДНХ.Москва~2.webp"
        );

        // Test without directory
        assert_eq!(to_webp_filename("test.jpg"), "test.webp");

        // Test edge cases
        assert_eq!(to_webp_filename("test"), "test.webp");
    }
}
