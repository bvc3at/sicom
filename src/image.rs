use anyhow::{Context, Result};
use std::path::Path;

pub fn is_supported_image(filename: &str) -> bool {
    let path = Path::new(filename);
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        matches!(ext.to_lowercase().as_str(), "jpg" | "jpeg" | "png" | "webp")
    } else {
        false
    }
}

pub fn compress_image_file(
    data: &[u8],
    filename: &str,
    quality: u8,
) -> Result<(Vec<u8>, u64, u64)> {
    let original_size = data.len() as u64;

    // Load image (detect format from data, not extension)
    let img = image::load_from_memory(data)
        .with_context(|| format!("Failed to decode image: {}", filename))?;

    // Always convert to WebP format for maximum compression
    let compressed_data = {
        let mut buffer = Vec::new();

        // Use webp crate directly for quality control
        let width = img.width();
        let height = img.height();
        let rgba_img = img.to_rgba8();

        if quality >= 95 {
            // Use lossless for high quality
            let encoder = webp::Encoder::new(&rgba_img, webp::PixelLayout::Rgba, width, height);
            let encoded = encoder.encode_lossless();
            buffer.extend_from_slice(&encoded);
        } else {
            // Use lossy compression with quality parameter
            let encoder = webp::Encoder::new(&rgba_img, webp::PixelLayout::Rgba, width, height);
            let encoded = encoder.encode(quality as f32);
            buffer.extend_from_slice(&encoded);
        }
        buffer
    };

    let compressed_size = compressed_data.len() as u64;
    Ok((compressed_data, original_size, compressed_size))
}

/// Convert image filename to WebP extension
pub fn to_webp_filename(filename: &str) -> String {
    let path = Path::new(filename);
    match path.file_stem().and_then(|s| s.to_str()) {
        Some(stem) => {
            if let Some(parent) = path.parent() {
                if parent == Path::new("") {
                    // Handle case where there's no directory
                    format!("{}.webp", stem)
                } else {
                    format!("{}/{}.webp", parent.display(), stem)
                }
            } else {
                format!("{}.webp", stem)
            }
        }
        None => filename.to_string(), // Fallback to original if we can't parse
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
        assert_eq!(to_webp_filename("Images/КимЧенИр. Северная Корея.jpg"), "Images/КимЧенИр. Северная Корея.webp");
        assert_eq!(to_webp_filename("Images/ВДНХ.Москва~2.jpg"), "Images/ВДНХ.Москва~2.webp");
        
        // Test without directory
        assert_eq!(to_webp_filename("test.jpg"), "test.webp");
        
        // Test edge cases
        assert_eq!(to_webp_filename("test"), "test.webp");
    }
}
