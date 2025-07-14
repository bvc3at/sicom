use anyhow::{Context, Result, anyhow};
use ffmpeg_sidecar::command::FfmpegCommand;
use ffmpeg_sidecar::event::{FfmpegEvent, LogLevel};
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

/// Check if a video file format is supported
pub fn is_supported_video(filename: &str) -> bool {
    let path = Path::new(filename);
    path.extension().and_then(|s| s.to_str()).is_some_and(|ext| {
        matches!(
            ext.to_lowercase().as_str(),
            "mp4" | "mov" | "avi" | "mkv" | "wmv" | "webm"
        )
    })
}

/// Detect video format from file extension
fn detect_video_format(filename: &str) -> Option<VideoFormat> {
    let path = Path::new(filename);
    path.extension().and_then(|s| s.to_str()).and_then(|ext| {
        match ext.to_lowercase().as_str() {
            "mp4" => Some(VideoFormat::Mp4),
            "mov" => Some(VideoFormat::Mov),
            "avi" => Some(VideoFormat::Avi),
            "mkv" => Some(VideoFormat::Mkv),
            _ => None,
        }
    })
}

/// Parse FFmpeg time string (e.g., "00:01:23.45") to seconds
/// Returns approximate progress percentage (0-100) based on heuristics
/// Since we don't know total duration, we estimate based on time elapsed
fn parse_time_to_progress_percent(time_str: &str) -> u64 {
    // Parse time format: HH:MM:SS.MS or MM:SS.MS
    let parts: Vec<&str> = time_str.split(':').collect();
    let total_seconds = match parts.len() {
        3 => {
            // HH:MM:SS.MS format
            let hours: f64 = parts[0].parse().unwrap_or(0.0);
            let minutes: f64 = parts[1].parse().unwrap_or(0.0);
            let seconds: f64 = parts[2].parse().unwrap_or(0.0);
            hours * 3600.0 + minutes * 60.0 + seconds
        },
        2 => {
            // MM:SS.MS format
            let minutes: f64 = parts[0].parse().unwrap_or(0.0);
            let seconds: f64 = parts[1].parse().unwrap_or(0.0);
            minutes * 60.0 + seconds
        },
        _ => return 0, // Invalid format
    };

    // Heuristic: assume most videos are 10-60 seconds for SIGame packs
    // Map 0-60 seconds to 0-100% progress
    let estimated_progress = (total_seconds / 60.0 * 100.0).min(100.0);
    estimated_progress as u64
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
    let _format = detect_video_format(filename)
        .ok_or_else(|| anyhow!("Unsupported video format: {}", filename))?;

    // Create temporary files for input and output with proper extensions
    let mut input_temp =
        NamedTempFile::with_suffix(".mp4").context("Failed to create temporary input file")?;
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

    let output_temp =
        NamedTempFile::with_suffix(".mp4").context("Failed to create temporary output file")?;
    let output_path = output_temp.path();

    // Calculate CRF from quality
    let crf = quality_to_crf(quality);

    // Setup ffmpeg command
    let mut ffmpeg_cmd = ffmpeg_path.map_or_else(FfmpegCommand::new, |path| FfmpegCommand::new_with_path(path));

    // Configure ffmpeg command for HEVC encoding
    ffmpeg_cmd
        .input(input_path.to_string_lossy())
        .args([
            "-nostats",     // Disable progress output with carriage returns
            "-hide_banner", // Hide banner for cleaner output
            // Let FFmpeg auto-detect input format instead of forcing mp4
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
        .output(output_path.to_string_lossy());

    // Execute FFmpeg with real-time event processing
    let mut child = ffmpeg_cmd
        .spawn()
        .context("Failed to spawn ffmpeg process")?;
    
    let iter = child
        .iter()
        .context("Failed to create event iterator")?;

    let mut has_error = false;
    let mut error_message = String::new();

    for event in iter {
        match event {
            FfmpegEvent::Log(log_level, message) => {
                // Filter for warnings and errors only
                match log_level {
                    LogLevel::Warning | LogLevel::Error | LogLevel::Fatal => {
                        logger.log(format!("FFmpeg: {}", message.trim()));
                    },
                    _ => {} // Ignore Info and Unknown levels
                }
            },
            FfmpegEvent::Error(error_msg) => {
                has_error = true;
                error_message = error_msg.clone();
                logger.log(format!("FFmpeg Error: {}", error_msg.trim()));
            },
            FfmpegEvent::Progress(progress) => {
                // Update video progress bar based on time elapsed
                if let Some(video_bar) = &logger.video_progress_bar {
                    let progress_percent = parse_time_to_progress_percent(&progress.time);
                    video_bar.set_position(progress_percent);
                }
            },
            FfmpegEvent::Done => break,
            _ => {} // Ignore other events (metadata, frames, etc.)
        }
    }

    if has_error {
        return Err(anyhow!("FFmpeg execution failed: {}", error_message));
    }

    // Read compressed data from output file
    let compressed_data = fs::read(output_path).context("Failed to read compressed video data")?;
    let compressed_size = compressed_data.len() as u64;

    // Explicitly keep temp files alive until here
    drop(input_temp);
    drop(output_temp);

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
    fn test_parse_time_to_progress_percent() {
        // Test HH:MM:SS.MS format
        assert_eq!(parse_time_to_progress_percent("00:00:30.00"), 50); // 30 seconds = 50%
        assert_eq!(parse_time_to_progress_percent("00:01:00.00"), 100); // 60 seconds = 100%
        assert_eq!(parse_time_to_progress_percent("00:00:15.50"), 25); // 15.5 seconds ≈ 25%

        // Test MM:SS.MS format
        assert_eq!(parse_time_to_progress_percent("00:30.00"), 50); // 30 seconds = 50%
        assert_eq!(parse_time_to_progress_percent("01:00.00"), 100); // 60 seconds = 100%
        assert_eq!(parse_time_to_progress_percent("00:15.50"), 25); // 15.5 seconds ≈ 25%

        // Test boundary cases
        assert_eq!(parse_time_to_progress_percent("00:00:00.00"), 0); // 0 seconds = 0%
        assert_eq!(parse_time_to_progress_percent("00:02:00.00"), 100); // 120 seconds capped at 100%

        // Test invalid format
        assert_eq!(parse_time_to_progress_percent("invalid"), 0);
        assert_eq!(parse_time_to_progress_percent(""), 0);
    }
}
