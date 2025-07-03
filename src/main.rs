use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
use image::ImageFormat;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;
use zip::{ZipArchive, ZipWriter};

#[derive(Error, Debug)]
pub enum SicomError {
    #[error("Input file does not exist: {0}")]
    InputNotFound(PathBuf),
    #[error("Input file is not a valid .siq file: {0}")]
    InvalidSiqFile(PathBuf),
    #[error("Failed to process image {name}: {source}")]
    ImageProcessingError { name: String, source: anyhow::Error },
}

#[derive(Parser)]
#[command(name = "sicom")]
#[command(about = "SIGame pack compression utility")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Compress {
        #[arg(help = "Path to existing SIGame pack (.siq file)")]
        input_pack: PathBuf,

        #[arg(help = "Path to output compressed pack (optional)")]
        output_pack: Option<PathBuf>,

        #[arg(long, default_value = "85", help = "Image quality (1-100)")]
        image_quality: u8,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Compress {
            input_pack,
            output_pack,
            image_quality,
        } => {
            compress_pack(input_pack, output_pack, image_quality)?;
        }
    }

    Ok(())
}

fn compress_pack(
    input_pack: PathBuf,
    output_pack: Option<PathBuf>,
    image_quality: u8,
) -> Result<()> {
    // Validate input
    if !input_pack.exists() {
        return Err(SicomError::InputNotFound(input_pack).into());
    }

    if input_pack.extension().and_then(|s| s.to_str()) != Some("siq") {
        return Err(SicomError::InvalidSiqFile(input_pack).into());
    }

    // Determine output path
    let output_path = match output_pack {
        Some(path) => path,
        None => {
            let mut path = input_pack.clone();
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| anyhow!("Invalid file name"))?;
            path.set_file_name(format!("{}_compressed.siq", stem));
            path
        }
    };

    println!("Compressing pack: {:?}", input_pack);
    println!("Output to: {:?}", output_path);
    println!("Image quality: {}", image_quality);

    // Validate quality
    if !(1..=100).contains(&image_quality) {
        return Err(anyhow!("Image quality must be between 1 and 100"));
    }

    // Open input ZIP
    let input_file = File::open(&input_pack)
        .with_context(|| format!("Failed to open input file: {:?}", input_pack))?;
    let mut archive = ZipArchive::new(BufReader::new(input_file))
        .with_context(|| "Failed to read ZIP archive")?;

    // Create output ZIP
    let output_file = File::create(&output_path)
        .with_context(|| format!("Failed to create output file: {:?}", output_path))?;
    let mut zip_writer = ZipWriter::new(BufWriter::new(output_file));

    let mut processed_images = 0;
    let mut skipped_images = 0;
    let mut total_original_size = 0u64;
    let mut total_compressed_size = 0u64;

    // Process each file in the archive
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .with_context(|| format!("Failed to read file at index {}", i))?;

        let file_name = file.name().to_string();
        let is_image = file_name.starts_with("Images/") && is_supported_image(&file_name);

        println!("Processing: {}", file_name);

        if is_image {
            match compress_image_file(&mut file, &file_name, image_quality) {
                Ok((compressed_data, original_size, compressed_size)) => {
                    // Add compressed image to output ZIP
                    zip_writer
                        .start_file(&file_name, zip::write::FileOptions::default())
                        .with_context(|| {
                            format!("Failed to start file in output ZIP: {}", file_name)
                        })?;
                    zip_writer.write_all(&compressed_data).with_context(|| {
                        format!("Failed to write compressed image: {}", file_name)
                    })?;

                    processed_images += 1;
                    total_original_size += original_size;
                    total_compressed_size += compressed_size;

                    println!(
                        "  Compressed: {} bytes -> {} bytes ({:.1}% reduction)",
                        original_size,
                        compressed_size,
                        (1.0 - compressed_size as f64 / original_size as f64) * 100.0
                    );
                }
                Err(e) => {
                    eprintln!("  Skipping {}: {}", file_name, e);
                    skipped_images += 1;

                    // Copy original file unchanged
                    let mut buffer = Vec::new();
                    file.read_to_end(&mut buffer)
                        .with_context(|| format!("Failed to read original file: {}", file_name))?;

                    zip_writer
                        .start_file(&file_name, zip::write::FileOptions::default())
                        .with_context(|| {
                            format!("Failed to start file in output ZIP: {}", file_name)
                        })?;
                    zip_writer
                        .write_all(&buffer)
                        .with_context(|| format!("Failed to write original file: {}", file_name))?;
                }
            }
        } else {
            // Copy non-image files unchanged
            let mut buffer = Vec::new();
            file.read_to_end(&mut buffer)
                .with_context(|| format!("Failed to read file: {}", file_name))?;

            zip_writer
                .start_file(&file_name, zip::write::FileOptions::default())
                .with_context(|| format!("Failed to start file in output ZIP: {}", file_name))?;
            zip_writer
                .write_all(&buffer)
                .with_context(|| format!("Failed to write file: {}", file_name))?;
        }
    }

    zip_writer
        .finish()
        .with_context(|| "Failed to finalize output ZIP")?;

    println!("\nCompression complete!");
    println!("Images processed: {}", processed_images);
    println!("Images skipped: {}", skipped_images);
    if total_original_size > 0 {
        println!(
            "Total image size reduction: {} bytes -> {} bytes ({:.1}% reduction)",
            total_original_size,
            total_compressed_size,
            (1.0 - total_compressed_size as f64 / total_original_size as f64) * 100.0
        );
    }

    Ok(())
}

fn is_supported_image(filename: &str) -> bool {
    let path = Path::new(filename);
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        matches!(ext.to_lowercase().as_str(), "jpg" | "jpeg" | "png" | "webp")
    } else {
        false
    }
}

fn compress_image_file(
    file: &mut zip::read::ZipFile,
    filename: &str,
    quality: u8,
) -> Result<(Vec<u8>, u64, u64)> {
    // Read original file data
    let mut original_data = Vec::new();
    file.read_to_end(&mut original_data)
        .with_context(|| format!("Failed to read image data: {}", filename))?;

    let original_size = original_data.len() as u64;

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
    let img = image::load_from_memory(&original_data)
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
                1..=33 => (image::codecs::png::CompressionType::Best, image::codecs::png::FilterType::Adaptive),    // Low quality = maximum compression
                34..=66 => (image::codecs::png::CompressionType::Default, image::codecs::png::FilterType::Adaptive), // Medium quality = default compression
                _ => (image::codecs::png::CompressionType::Fast, image::codecs::png::FilterType::Adaptive),         // High quality = minimum compression
            };
            
            let encoder = image::codecs::png::PngEncoder::new_with_quality(
                &mut buffer, 
                compression_type, 
                filter_type
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
    use std::io::Write;
    use tempfile::NamedTempFile;

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
    fn test_output_path_generation() {
        let input = PathBuf::from("test.siq");
        let expected = PathBuf::from("test_compressed.siq");

        // This tests the logic in compress_pack function
        let mut path = input.clone();
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap();
        path.set_file_name(format!("{}_compressed.siq", stem));

        assert_eq!(path, expected);
    }

    #[test]
    fn test_invalid_input_validation() {
        let result = compress_pack(PathBuf::from("nonexistent.siq"), None, 85);
        assert!(result.is_err());

        // Create a temporary file without .siq extension
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(b"test").unwrap();
        let temp_path = temp_file.path().to_path_buf();

        let result = compress_pack(temp_path, None, 85);
        assert!(result.is_err());
    }

    #[test]
    fn test_quality_validation() {
        // Quality should be between 1 and 100
        let temp_siq = create_temp_siq_file();

        let result = compress_pack(temp_siq.clone(), None, 0);
        assert!(result.is_err());

        let result = compress_pack(temp_siq.clone(), None, 101);
        assert!(result.is_err());

        // Valid quality should work (though will fail due to invalid ZIP content)
        let result = compress_pack(temp_siq, None, 50);
        // This will fail at ZIP reading stage, but quality validation should pass
        assert!(result.is_err());
        assert!(
            !result
                .unwrap_err()
                .to_string()
                .contains("quality must be between")
        );
    }

    fn create_temp_siq_file() -> PathBuf {
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(b"fake siq content").unwrap();

        // Rename to have .siq extension
        let temp_path = temp_file.path().with_extension("siq");
        std::fs::copy(temp_file.path(), &temp_path).unwrap();
        temp_path
    }
}
