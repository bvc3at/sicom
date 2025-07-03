use anyhow::{Context, Result, anyhow};
use image::ImageFormat;
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

    // Detect image format from file extension
    let path = Path::new(filename);
    let format = match path.extension().and_then(|s| s.to_str()) {
        Some(ext) => match ext.to_lowercase().as_str() {
            "jpg" | "jpeg" => ImageFormat::Jpeg,
            "png" => ImageFormat::Png,
            "webp" => ImageFormat::WebP,
            _ => return Err(anyhow!("Unsupported image format: {}", ext)),
        },
        None => return Err(anyhow!("No file extension found")),
    };

    // Load image
    let img = image::load_from_memory(data)
        .with_context(|| format!("Failed to decode image: {}", filename))?;

    // Compress image based on format
    let compressed_data = match format {
        ImageFormat::Jpeg => {
            let mut buffer = Vec::new();
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buffer, quality);
            img.write_with_encoder(encoder)
                .with_context(|| "Failed to encode JPEG")?;
            buffer
        }
        ImageFormat::Png => {
            let mut buffer = Vec::new();
            // Map quality to PNG compression type using image crate
            // Higher quality = less compression (faster), lower quality = more compression (better)
            let (compression_type, filter_type) = match quality {
                1..=33 => (
                    image::codecs::png::CompressionType::Best,
                    image::codecs::png::FilterType::Adaptive,
                ), // Low quality = maximum compression
                34..=66 => (
                    image::codecs::png::CompressionType::Default,
                    image::codecs::png::FilterType::Adaptive,
                ), // Medium quality = default compression
                _ => (
                    image::codecs::png::CompressionType::Fast,
                    image::codecs::png::FilterType::Adaptive,
                ), // High quality = minimum compression
            };

            let encoder = image::codecs::png::PngEncoder::new_with_quality(
                &mut buffer,
                compression_type,
                filter_type,
            );
            img.write_with_encoder(encoder)
                .with_context(|| "Failed to encode PNG")?;
            buffer
        }
        ImageFormat::WebP => {
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
        }
        _ => return Err(anyhow!("Unsupported format for compression")),
    };

    let compressed_size = compressed_data.len() as u64;
    Ok((compressed_data, original_size, compressed_size))
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
}
