#![allow(clippy::collapsible_if)]

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use indicatif::MultiProgress;
use indicatif_log_bridge::LogWrapper;
use log::{debug, error, info, warn};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, Write};
use std::path::PathBuf;
use thiserror::Error;
use zip::{ZipArchive, ZipWriter};

mod audio;
mod content_xml;
mod image;
mod progress;
mod stats;
mod video;

use content_xml::Category;

use progress::{ProgressLogger, get_log_color_with_module};
use stats::CompressionStats;

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

        #[arg(long, default_value = "40", help = "Image quality (1-100)")]
        image_quality: u8,

        #[arg(long, default_value = "85", help = "Audio quality (1-100)")]
        audio_quality: u8,

        #[arg(long, default_value = "50", help = "Video quality (1-100)")]
        video_quality: u8,

        #[arg(long, help = "Skip video compression")]
        skip_video: bool,

        #[arg(long, help = "Skip image compression")]
        skip_image: bool,

        #[arg(long, help = "Skip audio compression")]
        skip_audio: bool,

        #[arg(
            long,
            help = "Path to ffmpeg binary (optional, auto-detected if not provided)"
        )]
        ffmpeg_path: Option<PathBuf>,

        #[arg(
            long,
            help = "Always use compressed file even if it's larger than original"
        )]
        always_compress: bool,
    },
}

fn format_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    #[allow(clippy::cast_precision_loss)]
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

fn main() {
    // Initialize logger with indicatif-log-bridge to prevent log interference with progress bars
    let mut builder = env_logger::Builder::new();
    builder.target(env_logger::Target::Stderr);

    // Custom formatter to show only grey time on the left
    builder.format(|buf, record| {
        use std::io::Write;
        use std::time::{SystemTime, UNIX_EPOCH};

        // Get current local time
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Convert to local time (simple UTC offset approximation)
        let local_offset = 0; // Using UTC for simplicity, could be enhanced with timezone detection
        let local_time = now + local_offset;

        // Extract hours, minutes, seconds
        let hours = (local_time / 3600) % 24;
        let minutes = (local_time / 60) % 60;
        let seconds = local_time % 60;

        // Format with grey timestamp and color-coded message based on log level and module
        let message_color = get_log_color_with_module(record.level(), record.module_path());
        writeln!(
            buf,
            "\x1b[90m{:02}:{:02}:{:02}\x1b[0m {}{}\x1b[0m",
            hours,
            minutes,
            seconds,
            message_color,
            record.args()
        )
    });

    // Set default to info level if RUST_LOG is not set
    if std::env::var("RUST_LOG").is_err() {
        builder.filter_level(log::LevelFilter::Info);
    } else {
        builder.parse_default_env();
    }

    // Create logger and MultiProgress instance
    let logger = builder.build();
    let multi_progress = MultiProgress::new();

    // Wrap logger and multi-progress with LogWrapper to coordinate log output and progress bars
    LogWrapper::new(multi_progress.clone(), logger)
        .try_init()
        .expect("Failed to initialize logger");

    let cli = Cli::parse();

    match cli.command {
        Commands::Compress {
            input_pack,
            output_pack,
            image_quality,
            audio_quality,
            video_quality,
            skip_image,
            skip_audio,
            skip_video,
            ffmpeg_path,
            always_compress,
        } => {
            match compress_pack(
                input_pack,
                output_pack,
                image_quality,
                audio_quality,
                video_quality,
                skip_image,
                skip_audio,
                skip_video,
                ffmpeg_path,
                always_compress,
                multi_progress,
            ) {
                Ok(()) => {
                    // Success - exit normally
                }
                Err(e) => {
                    // Display error in red using our custom logger and exit with error code
                    error!("{e}");
                    std::process::exit(1);
                }
            }
        }
    }
}

/// Removes a partially written file unless disarmed.
struct TempFileGuard {
    path: PathBuf,
    armed: bool,
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Copies entry `index` into the output without recompressing, preserving the
/// original compression method and data byte-for-byte. Returns the
/// uncompressed size.
fn raw_copy_entry<R: Read + Seek, W: Write + Seek>(
    archive: &mut ZipArchive<R>,
    writer: &mut ZipWriter<W>,
    index: usize,
) -> Result<u64> {
    let file = archive
        .by_index_raw(index)
        .with_context(|| format!("Failed to read file at index {index}"))?;
    let size = file.size();
    let name = file.name().to_string();
    writer
        .raw_copy_file(file)
        .with_context(|| format!("Failed to copy file to output ZIP: {name}"))?;
    Ok(size)
}

/// Plans which entries may be renamed (extension swapped) during conversion.
///
/// A rename is disqualified when the target name — raw or URL-decoded — would
/// collide with an existing entry or another planned target, or when several
/// entries URL-decode to the same name (SIGame resolves references against
/// URL-decoded entry names, so decoded collisions shadow files just like raw
/// ones). Disqualified files keep their original name and bytes.
fn plan_renames(
    entry_names: &[String],
    allowed: bool,
    target_for: impl Fn(&str) -> Option<String>,
) -> HashMap<String, String> {
    if !allowed {
        return HashMap::new();
    }

    let mut decoded_counts: HashMap<String, usize> = HashMap::new();
    for name in entry_names {
        *decoded_counts
            .entry(content_xml::unescape_data_string(name))
            .or_default() += 1;
    }

    let candidates: Vec<(String, String)> = entry_names
        .iter()
        .filter_map(|name| target_for(name).map(|target| (name.clone(), target)))
        .filter(|(name, target)| name != target)
        .collect();

    let mut target_counts: HashMap<String, usize> = HashMap::new();
    let mut target_decoded_counts: HashMap<String, usize> = HashMap::new();
    for (_, target) in &candidates {
        *target_counts.entry(target.clone()).or_default() += 1;
        *target_decoded_counts
            .entry(content_xml::unescape_data_string(target))
            .or_default() += 1;
    }

    let existing: HashSet<&String> = entry_names.iter().collect();

    candidates
        .into_iter()
        .filter(|(name, target)| {
            let target_decoded = content_xml::unescape_data_string(target);
            let ok = target_counts[target] == 1
                && target_decoded_counts[&target_decoded] == 1
                && !existing.contains(target)
                && !decoded_counts.contains_key(&target_decoded)
                && decoded_counts[&content_xml::unescape_data_string(name)] == 1;
            if !ok {
                warn!(
                    "Not converting {name}: the new name {target} would collide with another file"
                );
            }
            ok
        })
        .collect()
}

/// Records a rename for the content.xml update, keyed the way SIGame resolves
/// references: by URL-decoded file name without the category directory.
fn record_conversion(
    conversions: &mut HashMap<Category, HashMap<String, String>>,
    category: Category,
    old_name: &str,
    new_name: &str,
) {
    let strip = |name: &str| {
        content_xml::unescape_data_string(name.strip_prefix(category.dir_prefix()).unwrap_or(name))
    };
    conversions
        .entry(category)
        .or_default()
        .insert(strip(old_name), strip(new_name));
}

/// Adds `<Default Extension=... />` declarations to [Content_Types].xml for
/// extensions introduced by conversion. Modern SIGame ignores this file, but
/// legacy OPC-based readers hide entries with undeclared extensions.
fn add_content_type_defaults(
    content_types: &str,
    conversions: &HashMap<Category, HashMap<String, String>>,
) -> String {
    let lower = content_types.to_lowercase();
    let mut additions = String::new();

    let converted_any = |category: Category| {
        conversions
            .get(&category)
            .is_some_and(|renames| !renames.is_empty())
    };

    if converted_any(Category::Images) && !lower.contains("extension=\"webp\"") {
        additions.push_str("<Default Extension=\"webp\" ContentType=\"si/image\" />");
    }
    if converted_any(Category::Video) && !lower.contains("extension=\"mp4\"") {
        additions.push_str("<Default Extension=\"mp4\" ContentType=\"si/video\" />");
    }

    if additions.is_empty() {
        return content_types.to_string();
    }
    match content_types.rfind("</Types>") {
        Some(pos) => format!(
            "{}{}{}",
            &content_types[..pos],
            additions,
            &content_types[pos..]
        ),
        None => content_types.to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
fn compress_pack(
    input_pack: PathBuf,
    output_pack: Option<PathBuf>,
    image_quality: u8,
    audio_quality: u8,
    video_quality: u8,
    skip_image: bool,
    skip_audio: bool,
    skip_video: bool,
    ffmpeg_path: Option<PathBuf>,
    always_compress: bool,
    multi_progress: MultiProgress,
) -> Result<()> {
    // Validate input
    if !input_pack.exists() {
        return Err(SicomError::InputNotFound(input_pack).into());
    }

    if input_pack.extension().and_then(|s| s.to_str()) != Some("siq") {
        return Err(SicomError::InvalidSiqFile(input_pack).into());
    }

    // Determine output path
    let output_path = if let Some(path) = output_pack {
        path
    } else {
        let mut path = input_pack.clone();
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow!("Invalid file name"))?;
        path.set_file_name(format!("{stem}_compressed.siq"));
        path
    };

    info!("Compressing pack: {input_pack:?}");
    info!("Output to: {output_path:?}");
    info!("Image quality: {image_quality}");
    info!("Audio quality: {audio_quality}");
    info!("Video quality: {video_quality}");
    info!("Skip image: {skip_image}");
    info!("Skip audio: {skip_audio}");
    info!("Skip video: {skip_video}");

    // Detect or validate ffmpeg path
    let ffmpeg_available = if let Some(path) = &ffmpeg_path {
        if path.exists() {
            info!("Using ffmpeg at: {path:?}");
            true
        } else {
            warn!("Specified ffmpeg path does not exist: {path:?}");
            false
        }
    } else {
        // Auto-detect ffmpeg using 'which' command
        match std::process::Command::new("which").arg("ffmpeg").output() {
            Ok(output) if output.status.success() => {
                let ffmpeg_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                info!("Auto-detected ffmpeg at: {ffmpeg_path}");
                true
            }
            _ => {
                if !skip_video {
                    warn!("ffmpeg not found in PATH. Video compression will be skipped.");
                    info!("To enable video compression:");
                    info!(
                        "  1. Install ffmpeg: brew install ffmpeg (macOS) or apt install ffmpeg (Ubuntu)"
                    );
                    info!("  2. Or specify path with --ffmpeg-path");
                    info!("  3. Or use --skip-video to suppress this warning");
                }
                false
            }
        }
    };

    // Validate quality
    if !(1..=100).contains(&image_quality) {
        return Err(anyhow!("Image quality must be between 1 and 100"));
    }
    if !(1..=100).contains(&audio_quality) {
        return Err(anyhow!("Audio quality must be between 1 and 100"));
    }
    if !(1..=100).contains(&video_quality) {
        return Err(anyhow!("Video quality must be between 1 and 100"));
    }

    // Open input ZIP
    let input_file = File::open(&input_pack)
        .with_context(|| format!("Failed to open input file: {input_pack:?}"))?;
    let mut archive = ZipArchive::new(BufReader::new(input_file))
        .with_context(|| "Failed to read ZIP archive")?;

    // --- Pre-pass: read content.xml and plan renames before writing anything ---

    let content_xml_data: Option<String> = match archive.by_name("content.xml") {
        Ok(mut file) => {
            let mut xml_data = String::new();
            file.read_to_string(&mut xml_data)
                .with_context(|| "Failed to read content.xml as UTF-8")?;
            Some(xml_data)
        }
        Err(zip::result::ZipError::FileNotFound) => None,
        Err(e) => return Err(e).with_context(|| "Failed to open content.xml"),
    };

    // Renaming a file is only safe when its references in content.xml can be
    // rewritten. A missing or unparseable content.xml therefore disables every
    // rename (media is still recompressed in place where possible).
    let renames_allowed = match content_xml_data.as_deref() {
        Some(xml) => match content_xml::find_media_refs(xml) {
            Ok(_) => true,
            Err(e) => {
                warn!("content.xml cannot be safely rewritten ({e}); files will keep their names");
                false
            }
        },
        None => {
            warn!("No content.xml found in pack; files will keep their names");
            false
        }
    };

    let entry_names: Vec<String> = archive.file_names().map(String::from).collect();

    let planned_images = plan_renames(&entry_names, renames_allowed && !skip_image, |name| {
        (name.starts_with("Images/") && image::is_supported_image(name))
            .then(|| image::to_webp_filename(name))
    });
    let planned_videos = plan_renames(
        &entry_names,
        renames_allowed && !skip_video && ffmpeg_available,
        |name| {
            (name.starts_with("Video/") && video::is_supported_video(name))
                .then(|| video::to_mp4_filename(name))
        },
    );

    // Write to a temporary file next to the output and rename into place at
    // the end: a failed run never leaves a partial .siq behind, and
    // compressing onto the input path cannot destroy the input.
    let part_path = output_path.with_extension("siq.part");
    let output_file = File::create(&part_path)
        .with_context(|| format!("Failed to create output file: {part_path:?}"))?;
    let mut zip_writer = ZipWriter::new(BufWriter::new(output_file));

    // Statistics tracking
    let mut stats = CompressionStats::new();

    // URL-decoded old name -> new name (without the category dir prefix),
    // per category, for content.xml updates.
    let mut conversions: HashMap<Category, HashMap<String, String>> = HashMap::new();
    // Every entry name written to the output, for final validation.
    let mut written_names: Vec<String> = Vec::new();
    let mut content_types_data: Option<String> = None;

    // Initialize progress logger
    let total_files = archive.len() as u64;
    let mut logger = ProgressLogger::new(total_files, &multi_progress);

    // Note: indicatif-log-bridge now handles coordination between log messages and progress bars

    // Helper function to get display filename (strip directory and URL decode)
    fn get_display_filename(file_path: &str) -> String {
        // Strip directory prefix (Images/, Audio/, Video/)
        let filename = if let Some(pos) = file_path.find('/') {
            &file_path[pos + 1..]
        } else {
            file_path
        };

        // URL decode the filename
        urlencoding::decode(filename)
            .unwrap_or_else(|_| filename.into())
            .to_string()
    }

    // Remove the partial output file unless the run completes.
    let mut part_guard = TempFileGuard {
        path: part_path.clone(),
        armed: true,
    };

    // Process each file in the archive
    for i in 0..archive.len() {
        let file_name = {
            archive
                .by_index_raw(i)
                .with_context(|| format!("Failed to read file at index {i}"))?
                .name()
                .to_string()
        };

        let is_image = file_name.starts_with("Images/") && image::is_supported_image(&file_name);
        let is_audio = file_name.starts_with("Audio/") && audio::is_supported_audio(&file_name);
        let is_video = file_name.starts_with("Video/") && video::is_supported_video(&file_name);

        debug!("Processing: {file_name}");

        if file_name == "content.xml" && content_xml_data.is_some() {
            // Rewritten and written after the loop.
            debug!("  Deferred content.xml for reference updates");
        } else if file_name == "[Content_Types].xml" && content_types_data.is_none() {
            let mut data = Vec::new();
            archive
                .by_index(i)
                .with_context(|| format!("Failed to read file at index {i}"))?
                .read_to_end(&mut data)
                .with_context(|| "Failed to read [Content_Types].xml")?;
            match String::from_utf8(data) {
                // Written after the loop, possibly with added type declarations.
                Ok(text) => content_types_data = Some(text),
                Err(e) => {
                    let size = raw_copy_entry(&mut archive, &mut zip_writer, i)?;
                    written_names.push(file_name.clone());
                    stats.add_other_file(size);
                    debug!("  [Content_Types].xml is not UTF-8 ({e}); copied unchanged");
                }
            }
        } else if is_image && !skip_image {
            // Renaming to .webp is only allowed when planned (no collisions,
            // rewritable content.xml). Files already named .webp are
            // recompressed in place.
            let target = planned_images.get(&file_name).cloned().or_else(|| {
                let same = image::to_webp_filename(&file_name);
                (same == file_name).then_some(same)
            });

            if let Some(webp_filename) = target {
                let mut image_data = Vec::new();
                archive
                    .by_index(i)
                    .with_context(|| format!("Failed to read file at index {i}"))?
                    .read_to_end(&mut image_data)
                    .with_context(|| format!("Failed to read image data: {file_name}"))?;

                match image::compress_image_file(&image_data, &file_name, image_quality) {
                    Ok((compressed_data, original_size, compressed_size))
                        if compressed_size < original_size || always_compress =>
                    {
                        zip_writer
                            .start_file(&webp_filename, zip::write::FileOptions::default())
                            .with_context(|| {
                                format!("Failed to start file in output ZIP: {webp_filename}")
                            })?;
                        zip_writer.write_all(&compressed_data).with_context(|| {
                            format!("Failed to write compressed image: {webp_filename}")
                        })?;
                        written_names.push(webp_filename.clone());

                        if webp_filename != file_name {
                            record_conversion(
                                &mut conversions,
                                Category::Images,
                                &file_name,
                                &webp_filename,
                            );
                        }

                        stats.add_processed_image(original_size, compressed_size);

                        let display_filename = get_display_filename(&file_name);
                        if compressed_size >= original_size {
                            debug!(
                                "  Converted \"{}\" to WebP (forced): {} bytes -> {} bytes ({:.1}% increase)",
                                display_filename,
                                original_size,
                                compressed_size,
                                (compressed_size as f64 / original_size as f64 - 1.0) * 100.0
                            );
                        } else {
                            debug!(
                                "  Converted \"{}\" to WebP: {} bytes -> {} bytes ({:.1}% reduction)",
                                display_filename,
                                original_size,
                                compressed_size,
                                (1.0 - compressed_size as f64 / original_size as f64) * 100.0
                            );
                        }
                    }
                    Ok((_, original_size, compressed_size)) => {
                        // Keep original file since compressed version is larger
                        raw_copy_entry(&mut archive, &mut zip_writer, i)?;
                        written_names.push(file_name.clone());
                        stats.add_kept_original_image(original_size);
                        info!(
                            "  Keeping original (compressed would be larger): {original_size} bytes vs {compressed_size} bytes"
                        );
                    }
                    Err(e) => {
                        debug!("  Skipping {file_name}: {e}");
                        let size = raw_copy_entry(&mut archive, &mut zip_writer, i)?;
                        written_names.push(file_name.clone());
                        stats.add_skipped_image(size);
                    }
                }
            } else {
                // Rename disqualified: keep the original bytes so the
                // references in content.xml stay valid.
                debug!("  Keeping original (renaming is not safe here): {file_name}");
                let size = raw_copy_entry(&mut archive, &mut zip_writer, i)?;
                written_names.push(file_name.clone());
                stats.add_skipped_image(size);
            }
        } else if is_image {
            debug!("  Skipping image compression (skip_image flag): {file_name}");
            let size = raw_copy_entry(&mut archive, &mut zip_writer, i)?;
            written_names.push(file_name.clone());
            stats.add_skipped_image(size);
        } else if is_audio && !skip_audio {
            let mut audio_data = Vec::new();
            archive
                .by_index(i)
                .with_context(|| format!("Failed to read file at index {i}"))?
                .read_to_end(&mut audio_data)
                .with_context(|| format!("Failed to read audio data: {file_name}"))?;

            match audio::compress_audio_file(&audio_data, &file_name, audio_quality) {
                Ok((compressed_data, original_size, compressed_size))
                    if compressed_size < original_size || always_compress =>
                {
                    zip_writer
                        .start_file(&file_name, zip::write::FileOptions::default())
                        .with_context(|| {
                            format!("Failed to start file in output ZIP: {file_name}")
                        })?;
                    zip_writer.write_all(&compressed_data).with_context(|| {
                        format!("Failed to write compressed audio: {file_name}")
                    })?;
                    written_names.push(file_name.clone());

                    stats.add_processed_audio(original_size, compressed_size);

                    let display_filename = get_display_filename(&file_name);
                    if compressed_size >= original_size {
                        debug!(
                            "  Compressed \"{}\" to MP3 (forced): {} bytes -> {} bytes ({:.1}% increase)",
                            display_filename,
                            original_size,
                            compressed_size,
                            (compressed_size as f64 / original_size as f64 - 1.0) * 100.0
                        );
                    } else {
                        debug!(
                            "  Compressed \"{}\" to MP3: {} bytes -> {} bytes ({:.1}% reduction)",
                            display_filename,
                            original_size,
                            compressed_size,
                            (1.0 - compressed_size as f64 / original_size as f64) * 100.0
                        );
                    }
                }
                Ok((_, original_size, compressed_size)) => {
                    raw_copy_entry(&mut archive, &mut zip_writer, i)?;
                    written_names.push(file_name.clone());
                    stats.add_kept_original_audio(original_size);
                    info!(
                        "  Keeping original (compressed would be larger): {original_size} bytes vs {compressed_size} bytes"
                    );
                }
                Err(e) => {
                    debug!("  Skipping {file_name}: {e}");
                    let size = raw_copy_entry(&mut archive, &mut zip_writer, i)?;
                    written_names.push(file_name.clone());
                    stats.add_skipped_audio(size);
                }
            }
        } else if is_audio {
            debug!("  Skipping audio compression (skip_audio flag): {file_name}");
            let size = raw_copy_entry(&mut archive, &mut zip_writer, i)?;
            written_names.push(file_name.clone());
            stats.add_skipped_audio(size);
        } else if is_video {
            let is_mp4 = file_name.to_lowercase().ends_with(".mp4");
            let target = planned_videos.get(&file_name).cloned();

            if skip_video || !ffmpeg_available {
                let reason = if skip_video {
                    "skip_video flag"
                } else {
                    "ffmpeg not available"
                };
                debug!("  Skipping video compression ({reason}): {file_name}");
                let size = raw_copy_entry(&mut archive, &mut zip_writer, i)?;
                written_names.push(file_name.clone());
                stats.add_skipped_video(size);
            } else if target.is_none() && !is_mp4 {
                // Compression re-muxes into MP4; without a safe rename the
                // extension would lie about the container. Keep the original.
                debug!("  Keeping original (renaming to .mp4 is not safe here): {file_name}");
                let size = raw_copy_entry(&mut archive, &mut zip_writer, i)?;
                written_names.push(file_name.clone());
                stats.add_skipped_video(size);
            } else {
                let mp4_filename = target.unwrap_or_else(|| file_name.clone());

                let mut video_data = Vec::new();
                archive
                    .by_index(i)
                    .with_context(|| format!("Failed to read file at index {i}"))?
                    .read_to_end(&mut video_data)
                    .with_context(|| format!("Failed to read video data: {file_name}"))?;

                // Try to compress video using ffmpeg-sidecar
                logger.start_video_progress(&file_name, &multi_progress);
                let video_result = video::compress_video_file(
                    &video_data,
                    &file_name,
                    video_quality,
                    ffmpeg_path.as_deref(),
                    &mut logger,
                );
                logger.finish_video_progress();

                match video_result {
                    Ok((compressed_data, original_size, compressed_size))
                        if compressed_size < original_size || always_compress =>
                    {
                        zip_writer
                            .start_file(&mp4_filename, zip::write::FileOptions::default())
                            .with_context(|| {
                                format!("Failed to start file in output ZIP: {mp4_filename}")
                            })?;
                        zip_writer.write_all(&compressed_data).with_context(|| {
                            format!("Failed to write compressed video: {mp4_filename}")
                        })?;
                        written_names.push(mp4_filename.clone());

                        if mp4_filename != file_name {
                            record_conversion(
                                &mut conversions,
                                Category::Video,
                                &file_name,
                                &mp4_filename,
                            );
                        }

                        stats.add_processed_video(original_size, compressed_size);

                        let display_filename = get_display_filename(&file_name);
                        if compressed_size >= original_size {
                            debug!(
                                "  Compressed \"{}\" to HEVC (forced): {} -> {} ({:.1}% increase)",
                                display_filename,
                                format_size(original_size),
                                format_size(compressed_size),
                                (compressed_size as f64 / original_size as f64 - 1.0) * 100.0
                            );
                        } else {
                            debug!(
                                "  Compressed \"{}\" to HEVC: {} -> {} ({:.1}% reduction)",
                                display_filename,
                                format_size(original_size),
                                format_size(compressed_size),
                                (1.0 - compressed_size as f64 / original_size as f64) * 100.0
                            );
                        }
                    }
                    Ok((_, original_size, compressed_size)) => {
                        raw_copy_entry(&mut archive, &mut zip_writer, i)?;
                        written_names.push(file_name.clone());
                        stats.add_kept_original_video(original_size);
                        info!(
                            "  Keeping original (compressed would be larger): {} vs {}",
                            format_size(original_size),
                            format_size(compressed_size)
                        );
                    }
                    Err(e) => {
                        warn!("  Video compression failed for {file_name}: {e}");
                        let size = raw_copy_entry(&mut archive, &mut zip_writer, i)?;
                        written_names.push(file_name.clone());
                        stats.add_skipped_video(size);
                    }
                }
            }
        } else {
            // Copy other files unchanged, preserving their compression and
            // metadata byte-for-byte.
            let size = raw_copy_entry(&mut archive, &mut zip_writer, i)?;
            written_names.push(file_name.clone());
            stats.add_other_file(size);
        }

        // Increment progress after processing each file
        logger.inc();
    }

    // Write content.xml with references updated for the renamed files.
    if let Some(xml_content) = content_xml_data {
        stats.add_other_file(xml_content.len() as u64);
        info!("Updating content.xml media references");

        let (new_xml, updated_refs) = content_xml::rewrite_media_refs(&xml_content, &conversions)
            .with_context(|| "Failed to rewrite content.xml")?;

        // Final safety net: every reference that resolved against the input
        // pack must still resolve against the output pack. Refusing to write
        // beats writing a corrupted pack; this can only trip on a bug.
        let mut final_names = written_names.clone();
        final_names.push("content.xml".to_string());
        if content_types_data.is_some() {
            final_names.push("[Content_Types].xml".to_string());
        }
        let broken_before: HashSet<String> =
            content_xml::unresolved_refs(&xml_content, &entry_names)?
                .into_iter()
                .collect();
        let regressions: Vec<String> = content_xml::unresolved_refs(&new_xml, &final_names)?
            .into_iter()
            .filter(|r| !broken_before.contains(r))
            .collect();
        if !regressions.is_empty() {
            bail!(
                "Refusing to write output: media references would break: {}",
                regressions.join(", ")
            );
        }

        zip_writer
            .start_file("content.xml", zip::write::FileOptions::default())
            .with_context(|| "Failed to start content.xml in output ZIP")?;
        zip_writer
            .write_all(new_xml.as_bytes())
            .with_context(|| "Failed to write updated content.xml")?;

        #[allow(clippy::cast_possible_truncation)]
        stats.add_updated_refs(updated_refs as u32);

        info!("Updated {updated_refs} media references in content.xml");
    }

    // Write [Content_Types].xml, declaring types for newly introduced
    // extensions so legacy OPC-based readers keep seeing the renamed files.
    if let Some(content_types) = content_types_data {
        stats.add_other_file(content_types.len() as u64);
        let content_types = add_content_type_defaults(&content_types, &conversions);
        zip_writer
            .start_file("[Content_Types].xml", zip::write::FileOptions::default())
            .with_context(|| "Failed to start [Content_Types].xml in output ZIP")?;
        zip_writer
            .write_all(content_types.as_bytes())
            .with_context(|| "Failed to write [Content_Types].xml")?;
    }

    zip_writer
        .finish()
        .with_context(|| "Failed to finalize output ZIP")?;

    std::fs::rename(&part_path, &output_path)
        .with_context(|| format!("Failed to move output into place: {output_path:?}"))?;
    part_guard.armed = false;

    // Finish progress logging and show final summary
    logger.finish();

    info!("Compression complete!");

    // Images statistics
    info!("");
    info!("Images:");
    info!("  Processed: {}", stats.images_processed());
    info!(
        "  Kept original (due to size): {}",
        stats.images_kept_original()
    );
    info!("  Skipped: {}", stats.images_skipped());
    if stats.image_original_size() > 0 {
        info!(
            "  Size reduction: {} -> {} ({:.1}% reduction)",
            format_size(stats.image_original_size()),
            format_size(stats.image_compressed_size()),
            stats.image_compression_ratio()
        );
    }

    // Audio statistics
    info!("");
    info!("Audio:");
    info!("  Processed: {}", stats.audio_processed());
    info!(
        "  Kept original (due to size): {}",
        stats.audio_kept_original()
    );
    info!("  Skipped: {}", stats.audio_skipped());
    if stats.audio_original_size() > 0 {
        if stats.audio_compressed_size() > 0 {
            info!(
                "  Size reduction: {} -> {} ({:.1}% reduction)",
                format_size(stats.audio_original_size()),
                format_size(stats.audio_compressed_size()),
                stats.audio_compression_ratio()
            );
        } else {
            info!(
                "  Total size: {} (no compression applied)",
                format_size(stats.audio_original_size())
            );
        }
    }

    // Video statistics
    info!("");
    info!("Video:");
    info!("  Processed: {}", stats.video_processed());
    info!(
        "  Kept original (due to size): {}",
        stats.video_kept_original()
    );
    info!("  Skipped: {}", stats.video_skipped());
    if stats.video_original_size() > 0 {
        if stats.video_compressed_size() > 0 {
            info!(
                "  Size reduction: {} -> {} ({:.1}% reduction)",
                format_size(stats.video_original_size()),
                format_size(stats.video_compressed_size()),
                stats.video_compression_ratio()
            );
        } else {
            info!(
                "  Total size: {} (no compression applied)",
                format_size(stats.video_original_size())
            );
        }
    }

    // Overall statistics
    if stats.total_input_size() > 0 {
        info!("");
        info!("Overall:");
        info!(
            "  Total original size: {}",
            format_size(stats.total_input_size())
        );
        info!(
            "  Total compressed size: {}",
            format_size(stats.total_output_size())
        );
        info!("  Total reduction: {:.1}%", stats.total_compression_ratio());

        // Show actual filesystem sizes for verification
        if let Ok(input_metadata) = std::fs::metadata(&input_pack) {
            let input_file_size = input_metadata.len();
            info!(
                "  Input file size: {} (filesystem)",
                format_size(input_file_size)
            );
        }
        if let Ok(output_metadata) = std::fs::metadata(&output_path) {
            let output_file_size = output_metadata.len();
            info!(
                "  Output file size: {} (filesystem)",
                format_size(output_file_size)
            );
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
        path.set_file_name(format!("{stem}_compressed.siq"));

        assert_eq!(path, expected);
    }

    #[test]
    fn test_invalid_input_validation() {
        let result = compress_pack(
            PathBuf::from("nonexistent.siq"),
            None,
            85,
            85,
            75,
            false,
            false,
            false,
            None,
            false,
            MultiProgress::new(),
        );
        assert!(result.is_err());

        // Create a temporary file without .siq extension
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(b"test").unwrap();
        let temp_path = temp_file.path().to_path_buf();

        let result = compress_pack(
            temp_path,
            None,
            85,
            85,
            75,
            false,
            false,
            false,
            None,
            false,
            MultiProgress::new(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_quality_validation() {
        // Quality should be between 1 and 100
        let temp_siq = create_temp_siq_file();

        let result = compress_pack(
            temp_siq.clone(),
            None,
            0,
            85,
            75,
            false,
            false,
            false,
            None,
            false,
            MultiProgress::new(),
        );
        assert!(result.is_err());

        let result = compress_pack(
            temp_siq.clone(),
            None,
            101,
            85,
            75,
            false,
            false,
            false,
            None,
            false,
            MultiProgress::new(),
        );
        assert!(result.is_err());

        let result = compress_pack(
            temp_siq.clone(),
            None,
            85,
            0,
            75,
            false,
            false,
            false,
            None,
            false,
            MultiProgress::new(),
        );
        assert!(result.is_err());

        let result = compress_pack(
            temp_siq.clone(),
            None,
            85,
            101,
            75,
            false,
            false,
            false,
            None,
            false,
            MultiProgress::new(),
        );
        assert!(result.is_err());

        let result = compress_pack(
            temp_siq.clone(),
            None,
            85,
            85,
            0,
            false,
            false,
            false,
            None,
            false,
            MultiProgress::new(),
        );
        assert!(result.is_err());

        let result = compress_pack(
            temp_siq.clone(),
            None,
            85,
            85,
            101,
            false,
            false,
            false,
            None,
            false,
            MultiProgress::new(),
        );
        assert!(result.is_err());

        // Valid quality should work (though will fail due to invalid ZIP content)
        let result = compress_pack(
            temp_siq,
            None,
            50,
            75,
            60,
            false,
            false,
            false,
            None,
            false,
            MultiProgress::new(),
        );
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

    fn tiny_png() -> Vec<u8> {
        let img = ::image::RgbaImage::from_fn(32, 32, |x, y| {
            ::image::Rgba([(x * 8) as u8, (y * 8) as u8, 128, 255])
        });
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, ::image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    /// End-to-end guard against the historical corruption bugs: adversarial
    /// file names must never produce dangling references or altered text.
    #[test]
    fn test_compress_pack_rewrites_refs_safely() {
        let dir = tempfile::tempdir().unwrap();
        let input_path = dir.path().join("input.siq");
        let output_path = dir.path().join("output.siq");

        let xml = "\u{feff}<?xml version=\"1.0\" encoding=\"utf-8\"?>\
<package name=\"t\" version=\"5\" logo=\"@logo one.png\"><rounds><round name=\"r\"><themes><theme name=\"t\"><questions>\
<question price=\"100\"><scenario>\
<item type=\"image\" isRef=\"True\">1.png</item>\
<item type=\"image\" isRef=\"True\">11.png</item>\
<item>Look at 1.png carefully</item>\
<item type=\"image\" isRef=\"True\">a&amp;b.png</item>\
<item type=\"image\" isRef=\"True\">pic.png</item>\
<item type=\"image\" isRef=\"True\">pic.jpg</item>\
<item type=\"image\">https://example.com/1.png</item>\
</scenario></question>\
</questions></theme></themes></round></rounds></package>";

        {
            let file = File::create(&input_path).unwrap();
            let mut writer = ZipWriter::new(BufWriter::new(file));
            let options = zip::write::FileOptions::default();
            writer.start_file("content.xml", options).unwrap();
            writer.write_all(xml.as_bytes()).unwrap();
            for name in [
                "Images/1.png",
                "Images/11.png",
                "Images/a&b.png",
                "Images/pic.png",
                "Images/pic.jpg",
                // Real-world convention: entry name Uri-escaped, XML literal.
                "Images/logo%20one.png",
            ] {
                writer.start_file(name, options).unwrap();
                writer.write_all(&tiny_png()).unwrap();
            }
            writer.finish().unwrap();
        }

        compress_pack(
            input_path,
            Some(output_path.clone()),
            40,
            85,
            50,
            false, // images on
            true,  // skip audio
            true,  // skip video
            None,
            true, // always compress: force renames even if webp is larger
            MultiProgress::new(),
        )
        .unwrap();

        let file = File::open(&output_path).unwrap();
        let mut archive = ZipArchive::new(BufReader::new(file)).unwrap();
        let names: Vec<String> = archive.file_names().map(String::from).collect();

        // Renamed cleanly, preserving percent-encoding in the entry name.
        for expected in [
            "Images/1.webp",
            "Images/11.webp",
            "Images/a&b.webp",
            "Images/logo%20one.webp",
        ] {
            assert!(names.contains(&expected.to_string()), "missing {expected}");
        }
        // The stem collision pair must keep its original names and bytes.
        assert!(names.contains(&"Images/pic.png".to_string()));
        assert!(names.contains(&"Images/pic.jpg".to_string()));
        assert!(!names.contains(&"Images/pic.webp".to_string()));

        let mut xml_out = String::new();
        archive
            .by_name("content.xml")
            .unwrap()
            .read_to_string(&mut xml_out)
            .unwrap();

        assert!(xml_out.contains(">1.webp</item>"));
        assert!(xml_out.contains(">11.webp</item>"));
        assert!(xml_out.contains(">a&amp;b.webp</item>"));
        assert!(xml_out.contains("logo=\"@logo one.webp\""));
        // Untouched: question text, external URL, collision-disqualified refs.
        assert!(xml_out.contains("Look at 1.png carefully"));
        assert!(xml_out.contains("https://example.com/1.png"));
        assert!(xml_out.contains(">pic.png</item>"));
        assert!(xml_out.contains(">pic.jpg</item>"));

        // Every reference must resolve exactly as SIGame would resolve it.
        assert!(
            content_xml::unresolved_refs(&xml_out, &names)
                .unwrap()
                .is_empty()
        );
    }
}
