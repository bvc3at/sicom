use anyhow::{anyhow, Context, Result};
use ffmpeg_sidecar::command::FfmpegCommand;
use std::path::Path;
use std::fs;
use tempfile::NamedTempFile;
use std::io::Write;

/// Clean control characters from log messages that can interfere with progress bars
fn clean_log_message(message: &str) -> String {
    message
        .chars()
        .filter(|&c| {
            // Keep printable ASCII and spaces, filter out control characters
            c.is_ascii_graphic() || c == ' '
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Supported video formats
#[derive(Debug, PartialEq)]
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
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        matches!(ext.to_lowercase().as_str(), "mp4" | "mov" | "avi" | "mkv" | "wmv" | "webm")
    } else {
        false
    }
}

/// Detect video format from file extension
fn detect_video_format(filename: &str) -> Option<VideoFormat> {
    let path = Path::new(filename);
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        match ext.to_lowercase().as_str() {
            "mp4" => Some(VideoFormat::Mp4),
            "mov" => Some(VideoFormat::Mov),
            "avi" => Some(VideoFormat::Avi),
            "mkv" => Some(VideoFormat::Mkv),
            _ => None,
        }
    } else {
        None
    }
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
    51 - ((quality as f32 - 1.0) * 33.0 / 99.0) as u8
}

/// Compress video file using HEVC (H.265) encoding via ffmpeg-sidecar
/// Returns (compressed_data, original_size, compressed_size, log_messages)
pub fn compress_video_file(
    data: &[u8],
    filename: &str,
    quality: u8,
    ffmpeg_path: Option<&Path>,
) -> Result<(Vec<u8>, u64, u64, Vec<String>)> {
    let original_size = data.len() as u64;
    
    // Detect video format
    let _format = detect_video_format(filename)
        .ok_or_else(|| anyhow!("Unsupported video format: {}", filename))?;
    
    // Create temporary files for input and output with proper extensions
    let mut input_temp = NamedTempFile::with_suffix(".mp4")
        .context("Failed to create temporary input file")?;
    input_temp.write_all(data)
        .context("Failed to write input data to temporary file")?;
    input_temp.flush()
        .context("Failed to flush input data to temporary file")?;
    let input_path = input_temp.path();
    
    let output_temp = NamedTempFile::with_suffix(".mp4")
        .context("Failed to create temporary output file")?;
    let output_path = output_temp.path();
    
    
    // Calculate CRF from quality
    let crf = quality_to_crf(quality);
    
    // Setup ffmpeg command
    let mut ffmpeg_cmd = if let Some(path) = ffmpeg_path {
        FfmpegCommand::new_with_path(path)
    } else {
        FfmpegCommand::new()
    };
    
    // Configure ffmpeg command for HEVC encoding
    ffmpeg_cmd
        .input(input_path.to_string_lossy())
        .args([
            "-nostats",               // Disable progress output with carriage returns
            "-hide_banner",           // Hide banner for cleaner output
            "-c:v", "libx265",       // Use HEVC/H.265 encoder
            "-crf", &crf.to_string(), // Quality setting
            "-preset", "medium",      // Encoding speed vs compression trade-off
            "-c:a", "copy",          // Copy audio stream without re-encoding
            "-movflags", "+faststart", // Optimize for web streaming
            "-y"                     // Overwrite output file if it exists
        ])
        .output(output_path.to_string_lossy());
    
    // Execute with event iteration to capture logs
    let iter = ffmpeg_cmd
        .spawn()
        .context("Failed to spawn ffmpeg process")?
        .iter()
        .context("Failed to get ffmpeg iterator")?;
    
    let mut log_messages = Vec::new();
    
    for event in iter {
        match event {
            ffmpeg_sidecar::event::FfmpegEvent::Progress(_) => {
                // Progress events can be ignored for now
            }
            ffmpeg_sidecar::event::FfmpegEvent::Log(level, line) => {
                // Clean the log line and collect for progress logger
                let clean_line = clean_log_message(&line);
                if !clean_line.is_empty() {
                    let log_msg = format!("  FFmpeg {:?}: {}", level, clean_line);
                    log_messages.push(log_msg);
                }
            }
            ffmpeg_sidecar::event::FfmpegEvent::Done => {
                // Process completed successfully
                break;
            }
            ffmpeg_sidecar::event::FfmpegEvent::Error(e) => {
                return Err(anyhow!("FFmpeg error: {}", e));
            }
            _ => {
                // Ignore other events
            }
        }
    }
    
    // Read compressed data from output file
    let compressed_data = fs::read(output_path)
        .context("Failed to read compressed video data")?;
    let compressed_size = compressed_data.len() as u64;
    
    // Explicitly keep temp files alive until here
    drop(input_temp);
    drop(output_temp);
    
    
    Ok((compressed_data, original_size, compressed_size, log_messages))
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
        assert_eq!(detect_video_format("Video/movie.avi"), Some(VideoFormat::Avi));
        assert_eq!(detect_video_format("test.mkv"), Some(VideoFormat::Mkv));
        assert_eq!(detect_video_format("test.wmv"), None);
        assert_eq!(detect_video_format("test.txt"), None);
    }

    #[test]
    fn test_quality_to_crf() {
        // Test boundary values
        assert_eq!(quality_to_crf(1), 51);    // Lowest quality
        assert_eq!(quality_to_crf(100), 18);  // Highest quality
        
        // Test middle values
        assert_eq!(quality_to_crf(50), 35);   // Balanced
        
        // Test clamping
        assert_eq!(quality_to_crf(0), 51);    // Should clamp to 1
        assert_eq!(quality_to_crf(101), 18);  // Should clamp to 100
        
        // Test specific quality ranges
        assert_eq!(quality_to_crf(30), 42);   // Lower quality
        assert_eq!(quality_to_crf(80), 25);   // Higher quality
    }
}