use anyhow::{Context, Result};
use std::path::Path;

pub fn is_supported_image(filename: &str) -> bool {
    let path = Path::new(filename);
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| matches!(ext.to_lowercase().as_str(), "jpg" | "jpeg" | "png" | "webp"))
}

pub fn compress_image_file(
    data: &[u8],
    filename: &str,
    quality: u8,
) -> Result<(Vec<u8>, u64, u64)> {
    let original_size = data.len() as u64;

    // Load image (detect format from data, not extension)
    let img = image::load_from_memory(data)
        .with_context(|| format!("Failed to decode image: {filename}"))?;

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

/// Convert image filename to WebP extension
pub fn to_webp_filename(filename: &str) -> String {
    let path = Path::new(filename);
    path.file_stem().and_then(|s| s.to_str()).map_or_else(
        || filename.to_string(),
        |stem| {
            path.parent().map_or_else(
                || format!("{stem}.webp"),
                |parent| {
                    if parent == Path::new("") {
                        // Handle case where there's no directory
                        format!("{stem}.webp")
                    } else {
                        format!("{}/{}.webp", parent.display(), stem)
                    }
                },
            )
        },
    )
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
