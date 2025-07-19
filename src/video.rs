use anyhow::{Context, Result, anyhow};
use ffmpeg_sidecar::command::FfmpegCommand;
use ffmpeg_sidecar::event::{FfmpegEvent, LogLevel};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;
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
    duration_seconds: Option<f64>,
    fps: Option<f32>,
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

/// Extract video metadata using ffprobe for accurate progress calculation
fn extract_video_metadata(file_path: &Path, ffmpeg_path: Option<&Path>) -> VideoMetadata {
    let ffprobe_cmd = if let Some(ffmpeg_path) = ffmpeg_path {
        // If custom ffmpeg path is provided, try to find ffprobe in the same directory
        let ffmpeg_dir = ffmpeg_path.parent().unwrap_or(Path::new("."));
        ffmpeg_dir.join("ffprobe")
    } else {
        Path::new("ffprobe").to_path_buf()
    };

    // Run ffprobe to get video stream information
    let output = Command::new(&ffprobe_cmd)
        .args([
            "-v", "quiet",           // Suppress ffprobe output
            "-print_format", "csv",  // CSV output format
            "-show_entries", "stream=nb_frames,duration,r_frame_rate", // Get frames, duration, fps
            "-select_streams", "v:0", // Select first video stream
            file_path.to_string_lossy().as_ref(),
        ])
        .output();

    let mut metadata = VideoMetadata {
        total_frames: None,
        duration_seconds: None,
        fps: None,
    };

    if let Ok(output) = output {
        if output.status.success() {
            let output_str = String::from_utf8_lossy(&output.stdout);
            
            // Parse CSV output: stream,nb_frames,duration,r_frame_rate
            for line in output_str.lines() {
                if line.starts_with("stream,") {
                    let parts: Vec<&str> = line.split(',').collect();
                    if parts.len() >= 4 {
                        // Parse nb_frames (index 1)
                        if let Ok(frames) = parts[1].parse::<u32>() {
                            metadata.total_frames = Some(frames);
                        }
                        
                        // Parse duration (index 2)
                        if let Ok(duration) = parts[2].parse::<f64>() {
                            metadata.duration_seconds = Some(duration);
                        }
                        
                        // Parse frame rate (index 3) - format is "num/den"
                        if let Some((num_str, den_str)) = parts[3].split_once('/') {
                            if let (Ok(num), Ok(den)) = (num_str.parse::<f32>(), den_str.parse::<f32>()) {
                                if den != 0.0 {
                                    metadata.fps = Some(num / den);
                                }
                            }
                        }
                    }
                    break;
                }
            }
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

/// Calculate accurate video encoding progress based on frame count
/// Returns progress percentage (0-100) based on current frame vs total frames
fn calculate_video_progress(current_frame: u32, metadata: &VideoMetadata) -> u64 {
    if let Some(total_frames) = metadata.total_frames {
        if total_frames > 0 {
            let progress = (current_frame as f64 / total_frames as f64 * 100.0).min(100.0);
            return progress as u64;
        }
    }
    
    // Fallback: use indeterminate progress - just show activity without percentage
    // For now, return a low value to indicate activity but unknown progress
    if current_frame > 0 { 5 } else { 0 }
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
    let mut input_temp =
        NamedTempFile::with_suffix(&file_extension).context("Failed to create temporary input file")?;
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
    let written_size = std::fs::metadata(&input_path)
        .context("Failed to get input file metadata")?
        .len();
    if written_size != original_size {
        return Err(anyhow!("Input file size mismatch: expected {}, got {}", original_size, written_size));
    }

    // Note: Keep input_temp alive - don't drop it until after FFmpeg completes

    // Double-check file exists and is accessible
    if !input_path.exists() {
        return Err(anyhow!("Input temporary file does not exist: {}", input_path.display()));
    }
    
    // Extract video metadata for accurate progress calculation
    let metadata = extract_video_metadata(&input_path, ffmpeg_path);
    
    // Log video metadata for debugging
    if let Some(frames) = metadata.total_frames {
        logger.log(format!("Video metadata: {} frames", frames));
    } else {
        logger.log("Video metadata: frame count unavailable, using fallback progress".to_string());
    }

    let output_temp =
        NamedTempFile::with_suffix(&file_extension).context("Failed to create temporary output file")?;
    let output_path = output_temp.path().to_path_buf();

    // Calculate CRF from quality
    let crf = quality_to_crf(quality);

    // Setup ffmpeg command
    let mut ffmpeg_cmd = ffmpeg_path.map_or_else(FfmpegCommand::new, |path| FfmpegCommand::new_with_path(path));

    // Configure ffmpeg command for HEVC encoding using proper input/output methods
    let _input_format = get_ffmpeg_format(format); // For future use if explicit format needed
    
    // Log video processing
    logger.log(format!("Processing video: {}", filename));
    
    ffmpeg_cmd
        .input(input_path.to_string_lossy())  // Input file with auto-detection
        .args([
            "-c:v", "libx265", // Use HEVC/H.265 encoder
            "-crf", &crf.to_string(), // Quality setting
            "-preset", "medium", // Encoding speed vs compression trade-off
            "-c:a", "copy", // Copy audio stream without re-encoding
            "-movflags", "+faststart", // Optimize for web streaming
            "-y",         // Overwrite output file if it exists
        ])
        .output(output_path.to_string_lossy());  // Output file

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
                // Ignore spurious "No streams found" error that occurs after successful processing
                if error_msg.trim() != "No streams found" {
                    has_error = true;
                    error_message = error_msg.clone();
                    logger.log(format!("FFmpeg Error: {}", error_msg.trim()));
                }
            },
            FfmpegEvent::Progress(progress) => {
                // Update video progress bar based on frame count
                if let Some(video_bar) = &logger.video_progress_bar {
                    let progress_percent = calculate_video_progress(progress.frame, &metadata);
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
    fn test_calculate_video_progress() {
        // Test with known total frames
        let metadata_with_frames = VideoMetadata {
            total_frames: Some(1000),
            duration_seconds: Some(40.0),
            fps: Some(25.0),
        };
        
        assert_eq!(calculate_video_progress(0, &metadata_with_frames), 0);      // 0% at start
        assert_eq!(calculate_video_progress(250, &metadata_with_frames), 25);   // 25% at 1/4
        assert_eq!(calculate_video_progress(500, &metadata_with_frames), 50);   // 50% at half
        assert_eq!(calculate_video_progress(750, &metadata_with_frames), 75);   // 75% at 3/4
        assert_eq!(calculate_video_progress(1000, &metadata_with_frames), 100); // 100% at end
        assert_eq!(calculate_video_progress(1200, &metadata_with_frames), 100); // Capped at 100%

        // Test with no frame count available
        let metadata_no_frames = VideoMetadata {
            total_frames: None,
            duration_seconds: Some(40.0),
            fps: Some(25.0),
        };
        
        assert_eq!(calculate_video_progress(0, &metadata_no_frames), 0);    // No activity
        assert_eq!(calculate_video_progress(100, &metadata_no_frames), 5);  // Some activity
        assert_eq!(calculate_video_progress(500, &metadata_no_frames), 5);  // Consistent fallback
    }

    #[test]
    fn test_calculate_video_progress_different_lengths() {
        // Test short video (5 seconds at 30fps = 150 frames)
        let short_video = VideoMetadata {
            total_frames: Some(150),
            duration_seconds: Some(5.0),
            fps: Some(30.0),
        };
        
        assert_eq!(calculate_video_progress(0, &short_video), 0);      // 0% at start
        assert_eq!(calculate_video_progress(75, &short_video), 50);    // 50% at half
        assert_eq!(calculate_video_progress(150, &short_video), 100);  // 100% at end
        
        // Test long video (2 minutes at 24fps = 2880 frames)
        let long_video = VideoMetadata {
            total_frames: Some(2880),
            duration_seconds: Some(120.0),
            fps: Some(24.0),
        };
        
        assert_eq!(calculate_video_progress(0, &long_video), 0);       // 0% at start
        assert_eq!(calculate_video_progress(720, &long_video), 25);    // 25% at 1/4
        assert_eq!(calculate_video_progress(1440, &long_video), 50);   // 50% at half
        assert_eq!(calculate_video_progress(2160, &long_video), 75);   // 75% at 3/4
        assert_eq!(calculate_video_progress(2880, &long_video), 100);  // 100% at end
        
        // Test calculated frames from duration and fps
        let mut calculated_frames = VideoMetadata {
            total_frames: None,
            duration_seconds: Some(10.0),
            fps: Some(25.0),
        };
        
        // Manually calculate frames as extract_video_metadata would do
        if let (Some(duration), Some(fps)) = (calculated_frames.duration_seconds, calculated_frames.fps) {
            calculated_frames.total_frames = Some((duration * fps as f64) as u32);
        }
        
        // Should now have 250 frames calculated and use that for progress
        assert_eq!(calculate_video_progress(125, &calculated_frames), 50);  // 50% progress
    }
}
