use anyhow::{Context, Result, anyhow};
use ffmpeg_sidecar::command::FfmpegCommand;
use ffmpeg_sidecar::event::{FfmpegEvent, LogLevel};
use log::{debug, warn};
use std::fs;
use std::io::Write;
use std::path::Path;
use tempfile::NamedTempFile;

/// Supported video formats
#[derive(Debug, PartialEq, Eq)]
pub enum VideoFormat {
    Mp4,
    Mov,
    Avi,
    Mkv,
    // Future formats can be added here
}

/// Video metadata for progress calculation
#[derive(Debug, Clone)]
struct VideoMetadata {
    total_frames: Option<u32>,
    duration_seconds: Option<f64>, // May not be available - be honest about it
    fps: Option<f32>,
}

/// Check if a video file format is supported
pub fn is_supported_video(filename: &str) -> bool {
    let path = Path::new(filename);
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_lowercase().as_str(),
                "mp4" | "mov" | "avi" | "mkv" | "wmv" | "webm"
            )
        })
}

/// Detect video format from file extension
fn detect_video_format(filename: &str) -> Option<VideoFormat> {
    let path = Path::new(filename);
    path.extension()
        .and_then(|s| s.to_str())
        .and_then(|ext| match ext.to_lowercase().as_str() {
            "mp4" => Some(VideoFormat::Mp4),
            "mov" => Some(VideoFormat::Mov),
            "avi" => Some(VideoFormat::Avi),
            "mkv" => Some(VideoFormat::Mkv),
            _ => None,
        })
}

/// Extract file extension from filename for temporary file creation
fn get_file_extension(filename: &str) -> String {
    let path = Path::new(filename);
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        format!(".{}", ext.to_lowercase())
    } else {
        ".mp4".to_string() // Default fallback
    }
}

/// Get FFmpeg input format string from video format
fn get_ffmpeg_format(format: VideoFormat) -> &'static str {
    match format {
        VideoFormat::Mp4 => "mp4",
        VideoFormat::Mov => "mov",
        VideoFormat::Avi => "avi",
        VideoFormat::Mkv => "matroska",
    }
}

/// Extract video metadata using ffprobe-rs for accurate progress calculation
fn extract_video_metadata(file_path: &Path, _ffmpeg_path: Option<&Path>) -> VideoMetadata {
    // Use ffprobe-rs to get structured video metadata
    let probe_result = ffprobe::ffprobe(file_path);

    let mut metadata = VideoMetadata {
        total_frames: None,
        duration_seconds: None, // Will be set from ffprobe if available
        fps: None,
    };

    match probe_result {
        Ok(probe_data) => {
            // Find the first video stream
            if let Some(video_stream) = probe_data
                .streams
                .iter()
                .find(|s| s.codec_type.as_ref().is_some_and(|t| t == "video"))
            {
                // Extract frame count (nb_frames)
                if let Some(nb_frames_str) = &video_stream.nb_frames {
                    if let Ok(frames) = nb_frames_str.parse::<u32>() {
                        metadata.total_frames = Some(frames);
                    }
                }

                // Extract duration from stream (prefer) or format
                let duration_str = video_stream
                    .duration
                    .as_ref()
                    .or(probe_data.format.duration.as_ref());

                if let Some(duration_str) = duration_str {
                    if let Ok(duration) = duration_str.parse::<f64>() {
                        metadata.duration_seconds = Some(duration);
                    }
                }

                // Extract frame rate - prefer avg_frame_rate for better accuracy
                let frame_rate_str = if !video_stream.avg_frame_rate.is_empty()
                    && video_stream.avg_frame_rate != "0/0"
                {
                    &video_stream.avg_frame_rate
                } else {
                    &video_stream.r_frame_rate
                };

                // Parse frame rate (format: "num/den")
                if let Some((num_str, den_str)) = frame_rate_str.split_once('/') {
                    if let (Ok(num), Ok(den)) = (num_str.parse::<f32>(), den_str.parse::<f32>()) {
                        if den != 0.0 {
                            metadata.fps = Some(num / den);
                        }
                    }
                }
            }
        }
        Err(_) => {
            // ffprobe failed - metadata will use fallback values
        }
    }

    // If nb_frames is not available but we have duration and fps, calculate it
    if metadata.total_frames.is_none() {
        if let (Some(duration), Some(fps)) = (metadata.duration_seconds, metadata.fps) {
            metadata.total_frames = Some((duration * fps as f64) as u32);
        }
    }

    metadata
}

/// Parse FFmpeg time string (e.g., "00:01:23.45") to seconds
/// Handles both HH:MM:SS.MS and MM:SS.MS formats
fn parse_ffmpeg_time_to_seconds(time_str: &str) -> Option<f64> {
    let parts: Vec<&str> = time_str.split(':').collect();

    match parts.len() {
        3 => {
            // HH:MM:SS.MS format
            let hours: f64 = parts[0].parse().ok()?;
            let minutes: f64 = parts[1].parse().ok()?;
            let seconds: f64 = parts[2].parse().ok()?;
            Some(hours * 3600.0 + minutes * 60.0 + seconds)
        }
        2 => {
            // MM:SS.MS format
            let minutes: f64 = parts[0].parse().ok()?;
            let seconds: f64 = parts[1].parse().ok()?;
            Some(minutes * 60.0 + seconds)
        }
        _ => None, // Invalid format
    }
}

/// Calculate accurate video encoding progress with hybrid approach
/// Primary: Frame-based progress when frame count is available
/// Fallback: Time-based progress using video duration
/// Returns Some(percentage) for accurate progress, None for indeterminate activity
fn calculate_video_progress(
    current_frame: u32,
    current_time: &str,
    metadata: &VideoMetadata,
) -> Option<u64> {
    // Primary method: Frame-based progress (most accurate)
    if let Some(total_frames) = metadata.total_frames {
        if total_frames > 0 {
            let progress = (current_frame as f64 / total_frames as f64 * 100.0).min(100.0);
            return Some(progress as u64);
        }
    }

    // Fallback method: Time-based progress using duration
    if let (Some(current_seconds), Some(duration)) = (
        parse_ffmpeg_time_to_seconds(current_time),
        metadata.duration_seconds,
    ) {
        if duration > 0.0 {
            let progress = (current_seconds / duration * 100.0).min(100.0);
            return Some(progress as u64);
        }
    }

    // Cannot calculate accurate progress - return None for indeterminate activity
    None
}

/// Map quality (1-100) to x265 CRF value (0-51)
/// Lower CRF = higher quality, larger size
/// Higher CRF = lower quality, smaller size
fn quality_to_crf(quality: u8) -> u8 {
    // Ensure quality is in valid range
    let quality = quality.clamp(1, 100);

    // Map quality 1-100 to CRF 51-18
    // Quality 1   → CRF 51 (lowest quality, smallest size)
    // Quality 50  → CRF 28 (balanced)
    // Quality 100 → CRF 18 (high quality, larger size)
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        51 - ((f32::from(quality) - 1.0) * 33.0 / 99.0) as u8
    }
}

/// Compress video file using HEVC (H.265) encoding via ffmpeg-sidecar
/// Returns (`compressed_data`, `original_size`, `compressed_size`)
/// Logging is handled in real-time through the provided logger
pub fn compress_video_file(
    data: &[u8],
    filename: &str,
    quality: u8,
    ffmpeg_path: Option<&Path>,
    logger: &mut crate::ProgressLogger,
) -> Result<(Vec<u8>, u64, u64)> {
    let original_size = data.len() as u64;

    // Detect video format
    let format = detect_video_format(filename)
        .ok_or_else(|| anyhow!("Unsupported video format: {}", filename))?;

    // Get proper file extension for temporary files
    let file_extension = get_file_extension(filename);

    // Create temporary files for input and output with proper extensions
    let mut input_temp = NamedTempFile::with_suffix(&file_extension)
        .context("Failed to create temporary input file")?;
    input_temp
        .write_all(data)
        .context("Failed to write input data to temporary file")?;
    input_temp
        .flush()
        .context("Failed to flush input data to temporary file")?;

    // Ensure file is fully written and synced
    input_temp
        .as_file()
        .sync_all()
        .context("Failed to sync input data to disk")?;

    let input_path = input_temp.path();

    // Validate that file was written correctly
    let written_size = std::fs::metadata(input_path)
        .context("Failed to get input file metadata")?
        .len();
    if written_size != original_size {
        return Err(anyhow!(
            "Input file size mismatch: expected {}, got {}",
            original_size,
            written_size
        ));
    }

    // Note: Keep input_temp alive - don't drop it until after FFmpeg completes

    // Double-check file exists and is accessible
    if !input_path.exists() {
        return Err(anyhow!(
            "Input temporary file does not exist: {}",
            input_path.display()
        ));
    }

    // Extract video metadata for accurate progress calculation
    let metadata = extract_video_metadata(input_path, ffmpeg_path);

    // Log video metadata for debugging
    if let Some(frames) = metadata.total_frames {
        debug!("Video metadata: {} frames", frames);
    } else {
        debug!("Video metadata: frame count unavailable, using fallback progress");
    }

    let output_temp = NamedTempFile::with_suffix(&file_extension)
        .context("Failed to create temporary output file")?;
    let output_path = output_temp.path().to_path_buf();

    // Calculate CRF from quality
    let crf = quality_to_crf(quality);

    // Setup ffmpeg command
    let mut ffmpeg_cmd = ffmpeg_path.map_or_else(FfmpegCommand::new, |path| {
        FfmpegCommand::new_with_path(path)
    });

    // Configure ffmpeg command for HEVC encoding using proper input/output methods
    let _input_format = get_ffmpeg_format(format); // For future use if explicit format needed

    // Log video processing
    debug!("Processing video: {}", filename);

    ffmpeg_cmd
        .input(input_path.to_string_lossy()) // Input file with auto-detection
        .args([
            "-c:v",
            "libx265", // Use HEVC/H.265 encoder
            "-crf",
            &crf.to_string(), // Quality setting
            "-preset",
            "medium", // Encoding speed vs compression trade-off
            "-c:a",
            "copy", // Copy audio stream without re-encoding
            "-movflags",
            "+faststart", // Optimize for web streaming
            "-y",         // Overwrite output file if it exists
        ])
        .output(output_path.to_string_lossy()); // Output file

    // Execute FFmpeg with real-time event processing
    let mut child = ffmpeg_cmd
        .spawn()
        .context("Failed to spawn ffmpeg process")?;

    let iter = child.iter().context("Failed to create event iterator")?;

    let mut has_error = false;
    let mut error_message = String::new();

    for event in iter {
        match event {
            FfmpegEvent::Log(LogLevel::Warning | LogLevel::Error | LogLevel::Fatal, message) => {
                // Filter for warnings and errors only
                debug!("FFmpeg: {}", message.trim());
            }
            FfmpegEvent::Log(_, _) => {} // Ignore Info and Unknown levels
            FfmpegEvent::Error(error_msg) => {
                // Ignore spurious "No streams found" error that occurs after successful processing
                if error_msg.trim() != "No streams found" {
                    has_error = true;
                    error_message = error_msg.clone();
                    warn!("FFmpeg Error: {}", error_msg.trim());
                }
            }
            FfmpegEvent::Progress(progress) => {
                // Update video progress bar using hybrid frame/time-based calculation
                if let Some(video_bar) = &logger.video_progress_bar {
                    match calculate_video_progress(progress.frame, &progress.time, &metadata) {
                        Some(progress_percent) => {
                            // Accurate progress available - set position
                            video_bar.set_position(progress_percent);
                        }
                        None => {
                            // No accurate progress - show indeterminate activity
                            video_bar.tick();
                        }
                    }
                }
            }
            FfmpegEvent::Done => break,
            _ => {} // Ignore other events (metadata, frames, etc.)
        }
    }

    if has_error {
        return Err(anyhow!("FFmpeg execution failed: {}", error_message));
    }

    // Read compressed data from output file
    let compressed_data = fs::read(&output_path).context("Failed to read compressed video data")?;
    let compressed_size = compressed_data.len() as u64;

    // Clean up temporary files automatically when they go out of scope
    // Both input_temp and output_temp will be cleaned up at function end

    Ok((compressed_data, original_size, compressed_size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_supported_video() {
        assert!(is_supported_video("Video/test.mp4"));
        assert!(is_supported_video("Video/test.MP4"));
        assert!(is_supported_video("Video/test.mov"));
        assert!(is_supported_video("Video/test.avi"));
        assert!(is_supported_video("Video/test.mkv"));
        assert!(!is_supported_video("Video/test.txt"));
        assert!(!is_supported_video("Audio/test.mp3"));
        assert!(!is_supported_video("Images/test.jpg"));
    }

    #[test]
    fn test_detect_video_format() {
        assert_eq!(detect_video_format("test.mp4"), Some(VideoFormat::Mp4));
        assert_eq!(detect_video_format("test.MP4"), Some(VideoFormat::Mp4));
        assert_eq!(detect_video_format("test.mov"), Some(VideoFormat::Mov));
        assert_eq!(
            detect_video_format("Video/movie.avi"),
            Some(VideoFormat::Avi)
        );
        assert_eq!(detect_video_format("test.mkv"), Some(VideoFormat::Mkv));
        assert_eq!(detect_video_format("test.wmv"), None);
        assert_eq!(detect_video_format("test.txt"), None);
    }

    #[test]
    fn test_quality_to_crf() {
        // Test boundary values
        assert_eq!(quality_to_crf(1), 51); // Lowest quality
        assert_eq!(quality_to_crf(100), 18); // Highest quality

        // Test middle values
        assert_eq!(quality_to_crf(50), 35); // Balanced

        // Test clamping
        assert_eq!(quality_to_crf(0), 51); // Should clamp to 1
        assert_eq!(quality_to_crf(101), 18); // Should clamp to 100

        // Test specific quality ranges
        assert_eq!(quality_to_crf(30), 42); // Lower quality
        assert_eq!(quality_to_crf(80), 25); // Higher quality
    }

    #[test]
    fn test_get_file_extension() {
        assert_eq!(get_file_extension("video.mp4"), ".mp4");
        assert_eq!(get_file_extension("Video/test.MOV"), ".mov");
        assert_eq!(get_file_extension("path/to/file.AVI"), ".avi");
        assert_eq!(get_file_extension("test.MKV"), ".mkv");
        assert_eq!(get_file_extension("noextension"), ".mp4"); // Default fallback
    }

    #[test]
    fn test_get_ffmpeg_format() {
        assert_eq!(get_ffmpeg_format(VideoFormat::Mp4), "mp4");
        assert_eq!(get_ffmpeg_format(VideoFormat::Mov), "mov");
        assert_eq!(get_ffmpeg_format(VideoFormat::Avi), "avi");
        assert_eq!(get_ffmpeg_format(VideoFormat::Mkv), "matroska");
    }

    #[test]
    fn test_parse_ffmpeg_time_to_seconds() {
        // Test HH:MM:SS.MS format
        assert_eq!(parse_ffmpeg_time_to_seconds("00:01:30.50"), Some(90.5)); // 1min 30.5sec = 90.5sec
        assert_eq!(parse_ffmpeg_time_to_seconds("01:02:15.25"), Some(3735.25)); // 1h 2min 15.25sec
        assert_eq!(parse_ffmpeg_time_to_seconds("00:00:00.00"), Some(0.0)); // Start time

        // Test MM:SS.MS format
        assert_eq!(parse_ffmpeg_time_to_seconds("02:30.75"), Some(150.75)); // 2min 30.75sec
        assert_eq!(parse_ffmpeg_time_to_seconds("00:15.50"), Some(15.5)); // 15.5 seconds

        // Test invalid formats
        assert_eq!(parse_ffmpeg_time_to_seconds("invalid"), None);
        assert_eq!(parse_ffmpeg_time_to_seconds(""), None);
        assert_eq!(parse_ffmpeg_time_to_seconds("1:2:3:4"), None); // Too many parts
    }

    #[test]
    fn test_calculate_video_progress_frame_based() {
        // Test frame-based progress (primary method)
        let metadata_with_frames = VideoMetadata {
            total_frames: Some(1000),
            duration_seconds: Some(40.0),
            fps: Some(25.0),
        };

        assert_eq!(
            calculate_video_progress(0, "00:00:00.00", &metadata_with_frames),
            Some(0)
        ); // 0% at start
        assert_eq!(
            calculate_video_progress(250, "00:00:10.00", &metadata_with_frames),
            Some(25)
        ); // 25% at 1/4
        assert_eq!(
            calculate_video_progress(500, "00:00:20.00", &metadata_with_frames),
            Some(50)
        ); // 50% at half
        assert_eq!(
            calculate_video_progress(750, "00:00:30.00", &metadata_with_frames),
            Some(75)
        ); // 75% at 3/4
        assert_eq!(
            calculate_video_progress(1000, "00:00:40.00", &metadata_with_frames),
            Some(100)
        ); // 100% at end
        assert_eq!(
            calculate_video_progress(1200, "00:00:48.00", &metadata_with_frames),
            Some(100)
        ); // Capped at 100%
    }

    #[test]
    fn test_calculate_video_progress_time_based() {
        // Test time-based progress (fallback method when no frame count)
        let metadata_no_frames = VideoMetadata {
            total_frames: None,
            duration_seconds: Some(60.0), // 1 minute video
            fps: Some(30.0),
        };

        assert_eq!(
            calculate_video_progress(0, "00:00:00.00", &metadata_no_frames),
            Some(0)
        ); // 0% at start
        assert_eq!(
            calculate_video_progress(100, "00:00:15.00", &metadata_no_frames),
            Some(25)
        ); // 25% at 15 seconds
        assert_eq!(
            calculate_video_progress(200, "00:00:30.00", &metadata_no_frames),
            Some(50)
        ); // 50% at 30 seconds
        assert_eq!(
            calculate_video_progress(300, "00:00:45.00", &metadata_no_frames),
            Some(75)
        ); // 75% at 45 seconds
        assert_eq!(
            calculate_video_progress(400, "00:01:00.00", &metadata_no_frames),
            Some(100)
        ); // 100% at end
        assert_eq!(
            calculate_video_progress(500, "00:01:15.00", &metadata_no_frames),
            Some(100)
        ); // Capped at 100%
    }

    #[test]
    fn test_calculate_video_progress_edge_cases() {
        // Test edge case: no frames and invalid time
        let metadata_edge_case = VideoMetadata {
            total_frames: None,
            duration_seconds: Some(30.0),
            fps: None,
        };

        // Invalid time format should return None for indeterminate progress
        assert_eq!(
            calculate_video_progress(0, "invalid", &metadata_edge_case),
            None
        ); // No progress
        assert_eq!(
            calculate_video_progress(100, "invalid", &metadata_edge_case),
            None
        ); // No progress

        // No duration edge case - completely unknown metadata
        let metadata_no_duration = VideoMetadata {
            total_frames: None,
            duration_seconds: None,
            fps: None,
        };

        assert_eq!(
            calculate_video_progress(0, "00:00:10.00", &metadata_no_duration),
            None
        ); // No progress
        assert_eq!(
            calculate_video_progress(100, "00:00:10.00", &metadata_no_duration),
            None
        ); // No progress

        // Test completely unknown metadata with valid time - should still return None
        assert_eq!(
            calculate_video_progress(50, "00:00:05.00", &metadata_no_duration),
            None
        ); // No progress even with valid time
    }

    #[test]
    fn test_calculate_video_progress_different_lengths() {
        // Test short video (5 seconds at 30fps = 150 frames)
        let short_video = VideoMetadata {
            total_frames: Some(150),
            duration_seconds: Some(5.0),
            fps: Some(30.0),
        };

        assert_eq!(
            calculate_video_progress(0, "00:00:00.00", &short_video),
            Some(0)
        ); // 0% at start
        assert_eq!(
            calculate_video_progress(75, "00:00:02.50", &short_video),
            Some(50)
        ); // 50% at half
        assert_eq!(
            calculate_video_progress(150, "00:00:05.00", &short_video),
            Some(100)
        ); // 100% at end

        // Test long video (2 minutes at 24fps = 2880 frames)
        let long_video = VideoMetadata {
            total_frames: Some(2880),
            duration_seconds: Some(120.0),
            fps: Some(24.0),
        };

        assert_eq!(
            calculate_video_progress(0, "00:00:00.00", &long_video),
            Some(0)
        ); // 0% at start
        assert_eq!(
            calculate_video_progress(720, "00:00:30.00", &long_video),
            Some(25)
        ); // 25% at 1/4
        assert_eq!(
            calculate_video_progress(1440, "00:01:00.00", &long_video),
            Some(50)
        ); // 50% at half
        assert_eq!(
            calculate_video_progress(2160, "00:01:30.00", &long_video),
            Some(75)
        ); // 75% at 3/4
        assert_eq!(
            calculate_video_progress(2880, "00:02:00.00", &long_video),
            Some(100)
        ); // 100% at end

        // Test calculated frames from duration and fps
        let mut calculated_frames = VideoMetadata {
            total_frames: None,
            duration_seconds: Some(10.0),
            fps: Some(25.0),
        };

        // Manually calculate frames as extract_video_metadata would do
        if let (Some(duration), Some(fps)) =
            (calculated_frames.duration_seconds, calculated_frames.fps)
        {
            calculated_frames.total_frames = Some((duration * fps as f64) as u32);
        }

        // Should now have 250 frames calculated and use that for progress
        assert_eq!(
            calculate_video_progress(125, "00:00:05.00", &calculated_frames),
            Some(50)
        ); // 50% progress
    }
}
