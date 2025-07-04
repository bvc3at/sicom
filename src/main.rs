use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::{VecDeque, HashMap};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use thiserror::Error;
use zip::{ZipArchive, ZipWriter};

mod image;
mod audio;

#[derive(Error, Debug)]
pub enum SicomError {
    #[error("Input file does not exist: {0}")]
    InputNotFound(PathBuf),
    #[error("Input file is not a valid .siq file: {0}")]
    InvalidSiqFile(PathBuf),
    #[error("Failed to process image {name}: {source}")]
    ImageProcessingError { name: String, source: anyhow::Error },
}

struct ProgressLogger {
    _multi_progress: MultiProgress, // Keep alive but prefix with _ to suppress warning
    progress_bar: ProgressBar,
    log_bars: Vec<ProgressBar>,
    log_lines: VecDeque<String>,
    max_lines: usize,
}

impl ProgressLogger {
    fn new(total_files: u64) -> Self {
        let multi_progress = MultiProgress::new();

        // Create main progress bar
        let progress_bar = multi_progress.add(ProgressBar::new(total_files));
        progress_bar.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} files (ETA: {eta})")
                .unwrap()
                .progress_chars("#>-"),
        );

        // Create 6 log lines as progress bars without progress (just for text display)
        let mut log_bars = Vec::new();
        for _ in 0..6 {
            let log_bar = multi_progress.add(ProgressBar::new(1));
            log_bar.set_style(ProgressStyle::default_bar().template("{msg:.dim}").unwrap());
            log_bar.finish(); // Hide the progress part, just show message
            log_bars.push(log_bar);
        }

        Self {
            _multi_progress: multi_progress,
            progress_bar,
            log_bars,
            log_lines: VecDeque::new(),
            max_lines: 6,
        }
    }

    fn log(&mut self, message: String) {
        // Add new log line
        self.log_lines.push_back(message);

        // Remove old lines if we exceed the limit
        while self.log_lines.len() > self.max_lines {
            self.log_lines.pop_front();
        }

        // Update the log display bars
        for (i, log_bar) in self.log_bars.iter().enumerate() {
            if let Some(line) = self.log_lines.get(i) {
                log_bar.set_message(line.clone());
            } else {
                log_bar.set_message("".to_string());
            }
        }
    }

    fn inc(&mut self) {
        self.progress_bar.inc(1);
    }

    fn finish(&mut self) {
        self.progress_bar.finish();

        // Clear all log bars
        for log_bar in &self.log_bars {
            log_bar.finish_and_clear();
        }

        self.progress_bar.finish_and_clear();

        // Show any remaining logs normally
        for line in &self.log_lines {
            println!("{}", line);
        }
    }
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

fn is_supported_video(filename: &str) -> bool {
    let path = std::path::Path::new(filename);
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        matches!(ext.to_lowercase().as_str(), "mp4" | "avi" | "mov" | "mkv" | "wmv" | "webm")
    } else {
        false
    }
}

fn format_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit_index = 0;
    
    while size >= 1024.0 && unit_index < UNITS.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }
    
    if unit_index == 0 {
        format!("{} {}", bytes, UNITS[unit_index])
    } else {
        format!("{:.1} {}", size, UNITS[unit_index])
    }
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

    // Statistics tracking
    let mut processed_images = 0;
    let mut skipped_images = 0;
    let processed_audio = 0;  // Not used yet - audio compression temporarily disabled
    let mut skipped_audio = 0;
    let _processed_video = 0;  // Not used yet, video compression not implemented
    let mut skipped_video = 0;
    
    let mut image_original_size = 0u64;
    let mut image_compressed_size = 0u64;
    let mut audio_original_size = 0u64;
    let audio_compressed_size = 0u64;  // Not used yet - audio compression temporarily disabled
    let mut video_original_size = 0u64;
    let _video_compressed_size = 0u64;  // Not used yet, video compression not implemented
    
    // Track total file sizes (for overall statistics)
    let mut total_input_size = 0u64;
    let mut total_output_size = 0u64;
    
    // Track image conversions for content.xml updates
    let mut image_conversions: HashMap<String, String> = HashMap::new();
    let mut content_xml_data: Option<String> = None;

    // Initialize progress logger
    let total_files = archive.len() as u64;
    let mut logger = ProgressLogger::new(total_files);

    // Process each file in the archive
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .with_context(|| format!("Failed to read file at index {}", i))?;

        let file_name = file.name().to_string();
        let is_image = file_name.starts_with("Images/") && image::is_supported_image(&file_name);
        let is_audio = file_name.starts_with("Audio/") && audio::is_supported_audio(&file_name);
        let is_video = file_name.starts_with("Video/") && is_supported_video(&file_name);
        let is_content_xml = file_name == "content.xml";

        logger.log(format!("Processing: {}", file_name));

        if is_content_xml {
            // Read content.xml for later processing
            let mut xml_data = String::new();
            file.read_to_string(&mut xml_data)
                .with_context(|| "Failed to read content.xml as UTF-8")?;
            
            // Track input size
            total_input_size += xml_data.len() as u64;
            
            content_xml_data = Some(xml_data);
            
            // We'll write content.xml after processing all images
            logger.log("  Stored content.xml for path updates".to_string());
        } else if is_image {
            // Read image data
            let mut image_data = Vec::new();
            file.read_to_end(&mut image_data)
                .with_context(|| format!("Failed to read image data: {}", file_name))?;

            // Track input size
            total_input_size += image_data.len() as u64;

            match image::compress_image_file(&image_data, &file_name, image_quality) {
                Ok((compressed_data, original_size, compressed_size)) => {
                    // Convert to WebP filename
                    let webp_filename = image::to_webp_filename(&file_name);
                    
                    // Add compressed image to output ZIP with WebP extension
                    zip_writer
                        .start_file(&webp_filename, zip::write::FileOptions::default())
                        .with_context(|| {
                            format!("Failed to start file in output ZIP: {}", webp_filename)
                        })?;
                    zip_writer.write_all(&compressed_data).with_context(|| {
                        format!("Failed to write compressed image: {}", webp_filename)
                    })?;

                    // Track the conversion for content.xml updates
                    image_conversions.insert(file_name.clone(), webp_filename.clone());

                    processed_images += 1;
                    image_original_size += original_size;
                    image_compressed_size += compressed_size;
                    total_output_size += compressed_size;

                    logger.log(format!(
                        "  Converted to WebP: {} bytes -> {} bytes ({:.1}% reduction)",
                        original_size,
                        compressed_size,
                        (1.0 - compressed_size as f64 / original_size as f64) * 100.0
                    ));
                }
                Err(e) => {
                    logger.log(format!("  Skipping {}: {}", file_name, e));
                    skipped_images += 1;

                    // Copy original file unchanged (keep original extension)
                    zip_writer
                        .start_file(&file_name, zip::write::FileOptions::default())
                        .with_context(|| {
                            format!("Failed to start file in output ZIP: {}", file_name)
                        })?;
                    zip_writer
                        .write_all(&image_data)
                        .with_context(|| format!("Failed to write original file: {}", file_name))?;
                    
                    total_output_size += image_data.len() as u64;
                    
                    // Do NOT track this conversion - content.xml will keep original path
                }
            }
        } else if is_audio {
            // Read audio data
            let mut audio_data = Vec::new();
            file.read_to_end(&mut audio_data)
                .with_context(|| format!("Failed to read audio data: {}", file_name))?;
            
            // Track input size
            total_input_size += audio_data.len() as u64;
            
            // Temporarily skip audio compression to test logging system - just copy unchanged
            logger.log(format!("  Skipping audio compression (testing): {}", file_name));
            skipped_audio += 1;
            
            // Track original size for skipped audio
            audio_original_size += audio_data.len() as u64;

            // Copy original file unchanged
            zip_writer
                .start_file(&file_name, zip::write::FileOptions::default())
                .with_context(|| {
                    format!("Failed to start file in output ZIP: {}", file_name)
                })?;
            zip_writer
                .write_all(&audio_data)
                .with_context(|| format!("Failed to write original audio file: {}", file_name))?;
            
            total_output_size += audio_data.len() as u64;
        } else if is_video {
            // Read video data
            let mut video_data = Vec::new();
            file.read_to_end(&mut video_data)
                .with_context(|| format!("Failed to read video data: {}", file_name))?;
            
            // Track input size
            total_input_size += video_data.len() as u64;
            
            // For now, just copy video files unchanged (future: add video compression)
            zip_writer
                .start_file(&file_name, zip::write::FileOptions::default())
                .with_context(|| {
                    format!("Failed to start file in output ZIP: {}", file_name)
                })?;
            zip_writer
                .write_all(&video_data)
                .with_context(|| format!("Failed to write video file: {}", file_name))?;
            
            // Track as skipped for now (no compression implemented yet)
            skipped_video += 1;
            video_original_size += video_data.len() as u64;
            total_output_size += video_data.len() as u64;
            
            logger.log(format!("  Copied unchanged (no compression): {} bytes", format_size(video_data.len() as u64)));
        } else {
            // Copy other files unchanged
            let mut buffer = Vec::new();
            file.read_to_end(&mut buffer)
                .with_context(|| format!("Failed to read file: {}", file_name))?;

            // Track input size
            total_input_size += buffer.len() as u64;

            zip_writer
                .start_file(&file_name, zip::write::FileOptions::default())
                .with_context(|| format!("Failed to start file in output ZIP: {}", file_name))?;
            zip_writer
                .write_all(&buffer)
                .with_context(|| format!("Failed to write file: {}", file_name))?;
            
            total_output_size += buffer.len() as u64;
        }

        // Increment progress after processing each file
        logger.inc();
    }

    // Process content.xml with updated image paths
    if let Some(mut xml_content) = content_xml_data {
        logger.log("Updating content.xml with new image paths".to_string());
        
        let mut total_updated_refs = 0;
        
        // Update image paths in content.xml
        for (original_path, webp_path) in &image_conversions {
            // Extract just the filename from the full path for the XML replacement
            let original_filename = original_path.strip_prefix("Images/").unwrap_or(original_path);
            let webp_filename = webp_path.strip_prefix("Images/").unwrap_or(webp_path);
            
            // Try different encoding variations of the filename
            let original_variations = vec![
                original_filename.to_string(),
                urlencoding::decode(original_filename).unwrap_or_else(|_| original_filename.into()).to_string(),
                urlencoding::encode(original_filename).to_string(),
            ];
            
            let webp_variations = vec![
                webp_filename.to_string(),
                urlencoding::decode(webp_filename).unwrap_or_else(|_| webp_filename.into()).to_string(),
                urlencoding::encode(webp_filename).to_string(),
            ];
            
            let mut file_replacements = 0;
            
            // Try all combinations of original and webp variations
            for orig_var in &original_variations {
                for webp_var in &webp_variations {
                    // Try different XML patterns that might contain the filename
                    let patterns = vec![
                        // Simple filename reference
                        (orig_var.clone(), webp_var.clone()),
                        // With isRef="True" wrapper
                        (format!("isRef=\"True\">{}", orig_var), format!("isRef=\"True\">{}", webp_var)),
                        // With type="image" attribute
                        (format!("type=\"image\" isRef=\"True\">{}", orig_var), format!("type=\"image\" isRef=\"True\">{}", webp_var)),
                        // With different quote styles
                        (format!("isRef='True'>{}", orig_var), format!("isRef='True'>{}", webp_var)),
                        // Full path references
                        (format!("Images/{}", orig_var), format!("Images/{}", webp_var)),
                        // Path references with isRef
                        (format!("isRef=\"True\">Images/{}", orig_var), format!("isRef=\"True\">Images/{}", webp_var)),
                    ];
                    
                    for (old_pattern, new_pattern) in patterns {
                        if old_pattern != new_pattern {
                            let count = xml_content.matches(&old_pattern).count();
                            if count > 0 {
                                xml_content = xml_content.replace(&old_pattern, &new_pattern);
                                file_replacements += count;
                            }
                        }
                    }
                }
            }
            
            total_updated_refs += file_replacements;
            
            if file_replacements > 0 {
                logger.log(format!("  Updated: {} -> {} ({} refs)", original_filename, webp_filename, file_replacements));
            } else {
                logger.log(format!("  Warning: No refs found for {}", original_filename));
            }
        }
        
        // Write updated content.xml to output ZIP
        zip_writer
            .start_file("content.xml", zip::write::FileOptions::default())
            .with_context(|| "Failed to start content.xml in output ZIP")?;
        zip_writer
            .write_all(xml_content.as_bytes())
            .with_context(|| "Failed to write updated content.xml")?;
            
        // Track output size
        total_output_size += xml_content.len() as u64;
            
        logger.log(format!("Updated {} image references in content.xml", total_updated_refs));
    } else {
        logger.log("Warning: No content.xml found in pack".to_string());
    }

    zip_writer
        .finish()
        .with_context(|| "Failed to finalize output ZIP")?;

    // Finish progress logging and show final summary
    logger.finish();

    println!("\nCompression complete!");
    
    // Images statistics
    println!("\nImages:");
    println!("  Processed: {}", processed_images);
    println!("  Skipped: {}", skipped_images);
    if image_original_size > 0 {
        println!("  Size reduction: {} -> {} ({:.1}% reduction)",
            format_size(image_original_size),
            format_size(image_compressed_size),
            (1.0 - image_compressed_size as f64 / image_original_size as f64) * 100.0
        );
    }
    
    // Audio statistics
    println!("\nAudio:");
    println!("  Processed: {}", processed_audio);
    println!("  Skipped: {}", skipped_audio);
    if audio_original_size > 0 {
        if audio_compressed_size > 0 {
            println!("  Size reduction: {} -> {} ({:.1}% reduction)",
                format_size(audio_original_size),
                format_size(audio_compressed_size),
                (1.0 - audio_compressed_size as f64 / audio_original_size as f64) * 100.0
            );
        } else {
            println!("  Total size: {} (no compression applied)", format_size(audio_original_size));
        }
    }
    
    // Video statistics
    println!("\nVideo:");
    println!("  Processed: {}", _processed_video);
    println!("  Skipped: {}", skipped_video);
    if video_original_size > 0 {
        println!("  Total size: {} (no compression implemented yet)", format_size(video_original_size));
    }
    
    // Overall statistics
    if total_input_size > 0 {
        println!("\nOverall:");
        println!("  Total original size: {}", format_size(total_input_size));
        println!("  Total compressed size: {}", format_size(total_output_size));
        println!("  Total reduction: {:.1}%", (1.0 - total_output_size as f64 / total_input_size as f64) * 100.0);
        
        // Show actual filesystem sizes for verification
        if let Ok(input_metadata) = std::fs::metadata(&input_pack) {
            let input_file_size = input_metadata.len();
            println!("  Input file size: {} (filesystem)", format_size(input_file_size));
        }
        if let Ok(output_metadata) = std::fs::metadata(&output_path) {
            let output_file_size = output_metadata.len();
            println!("  Output file size: {} (filesystem)", format_size(output_file_size));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

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
