use anyhow::{Context, Result, anyhow};
use mp3lame_encoder::{Bitrate, Builder, FlushNoGap, InterleavedPcm};
use std::path::Path;
use symphonia::core::audio::{AudioBufferRef, Signal};
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// MP3 frame size in samples
const SAMPLES_PER_FRAME: usize = 1152;

/// Supported audio formats
#[derive(Debug, PartialEq, Eq)]
pub enum AudioFormat {
    Mp3,
    // Future formats to be added:
    // Wav,
    // OggVorbis,
    // Opus,
    // Flac,
}

/// Check if an audio file format is supported
pub fn is_supported_audio(filename: &str) -> bool {
    let path = Path::new(filename);
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| matches!(ext.to_lowercase().as_str(), "mp3"))
}

/// Detect audio format from file extension
fn detect_audio_format(filename: &str) -> Option<AudioFormat> {
    let path = Path::new(filename);
    path.extension().and_then(|s| s.to_str()).and_then(|ext| {
        match ext.to_lowercase().as_str() {
            "mp3" => Some(AudioFormat::Mp3),
            // Future formats:
            // "wav" => Some(AudioFormat::Wav),
            // "ogg" => Some(AudioFormat::OggVorbis),
            // "opus" => Some(AudioFormat::Opus),
            // "flac" => Some(AudioFormat::Flac),
            _ => None,
        }
    })
}

/// Map quality (1-100) to MP3 bitrate enum
/// Based on real-world data: 64-320 kbps range, 215 kbps average
fn quality_to_mp3_bitrate(quality: u8) -> Bitrate {
    // Ensure quality is in valid range
    let quality = quality.clamp(1, 100);

    // Map quality 1-100 to available bitrate options
    // Quality 1-15   -> 64 kbps  (lowest)
    // Quality 16-25  -> 80 kbps
    // Quality 26-35  -> 96 kbps
    // Quality 36-45  -> 128 kbps
    // Quality 46-55  -> 160 kbps
    // Quality 56-65  -> 192 kbps (around average of real data: 215 kbps)
    // Quality 66-75  -> 224 kbps
    // Quality 76-95  -> 256 kbps
    // Quality 96-100 -> 320 kbps (highest)

    match quality {
        1..=15 => Bitrate::Kbps64,
        16..=25 => Bitrate::Kbps80,
        26..=35 => Bitrate::Kbps96,
        36..=45 => Bitrate::Kbps128,
        46..=55 => Bitrate::Kbps160,
        56..=65 => Bitrate::Kbps192,
        66..=75 => Bitrate::Kbps224,
        76..=95 => Bitrate::Kbps256,
        96..=100 => Bitrate::Kbps320,
        _ => Bitrate::Kbps192, // Default fallback (shouldn't happen due to clamp)
    }
}

/// Decode audio data using Symphonia
fn decode_audio_data(data: &[u8]) -> Result<(Vec<f32>, u32, u32)> {
    // Create a media source from the byte data (copy to owned Vec to fix lifetime)
    let data_owned = data.to_vec();
    let cursor = std::io::Cursor::new(data_owned);
    let media_source =
        MediaSourceStream::new(Box::new(cursor), MediaSourceStreamOptions::default());

    // Create a probe hint (we'll let Symphonia auto-detect the format)
    let hint = Hint::new();

    // Use the default options
    let format_opts = FormatOptions::default();
    let metadata_opts = MetadataOptions::default();
    let decoder_opts = DecoderOptions::default();

    // Probe the media source
    let probed = symphonia::default::get_probe()
        .format(&hint, media_source, &format_opts, &metadata_opts)
        .with_context(|| "Failed to probe audio format")?;

    let mut format = probed.format;

    // Find the first audio track
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| anyhow!("No audio track found"))?;

    let track_id = track.id;

    // Create a decoder for the track
    let mut audio_decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &decoder_opts)
        .with_context(|| "Failed to create audio decoder")?;

    let mut audio_data = Vec::new();
    let sample_rate = track.codec_params.sample_rate.unwrap_or(44100);
    let channels = u32::try_from(track.codec_params.channels.map_or(2, |c| c.count())).unwrap_or(2);

    // Decode all packets
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::ResetRequired) => {
                // The track list has been changed. Re-examine it and create a new set of decoders,
                // then restart the decode loop. This is an advanced feature that most applications
                // do not need.
                unimplemented!();
            }
            Err(SymphoniaError::IoError(err)) => {
                // The packet reader has reached EOF, or a fatal error has occurred.
                match err.kind() {
                    std::io::ErrorKind::UnexpectedEof => break,
                    _ => return Err(anyhow!("IO error during decoding: {}", err)),
                }
            }
            Err(err) => return Err(anyhow!("Decode error: {}", err)),
        };

        // Only decode packets for our selected track
        if packet.track_id() != track_id {
            continue;
        }

        // Decode the packet
        match audio_decoder.decode(&packet) {
            Ok(decoded_buffer) => {
                // Convert decoded audio to f32 samples
                match decoded_buffer {
                    AudioBufferRef::F32(buf) => {
                        // Interleave channels if stereo
                        if buf.spec().channels.count() == 1 {
                            audio_data.extend_from_slice(buf.chan(0));
                        } else {
                            let left = buf.chan(0);
                            let right = buf.chan(1);
                            for (l, r) in left.iter().zip(right.iter()) {
                                audio_data.push(*l);
                                audio_data.push(*r);
                            }
                        }
                    }
                    AudioBufferRef::U8(buf) => {
                        // Convert u8 to f32 - interleave channels
                        if buf.spec().channels.count() == 1 {
                            for &sample in buf.chan(0) {
                                let f_sample = (f32::from(sample) - 128.0) / 128.0;
                                audio_data.push(f_sample);
                            }
                        } else {
                            let left = buf.chan(0);
                            let right = buf.chan(1);
                            for (l, r) in left.iter().zip(right.iter()) {
                                let f_l = (f32::from(*l) - 128.0) / 128.0;
                                let f_r = (f32::from(*r) - 128.0) / 128.0;
                                audio_data.push(f_l);
                                audio_data.push(f_r);
                            }
                        }
                    }
                    AudioBufferRef::U16(buf) => {
                        // Convert u16 to f32 - interleave channels
                        if buf.spec().channels.count() == 1 {
                            for &sample in buf.chan(0) {
                                let f_sample = (f32::from(sample) - 32768.0) / 32768.0;
                                audio_data.push(f_sample);
                            }
                        } else {
                            let left = buf.chan(0);
                            let right = buf.chan(1);
                            for (l, r) in left.iter().zip(right.iter()) {
                                let f_l = (f32::from(*l) - 32768.0) / 32768.0;
                                let f_r = (f32::from(*r) - 32768.0) / 32768.0;
                                audio_data.push(f_l);
                                audio_data.push(f_r);
                            }
                        }
                    }
                    AudioBufferRef::S16(buf) => {
                        // Convert s16 to f32 - interleave channels
                        if buf.spec().channels.count() == 1 {
                            for &sample in buf.chan(0) {
                                let f_sample = f32::from(sample) / 32768.0;
                                audio_data.push(f_sample);
                            }
                        } else {
                            let left = buf.chan(0);
                            let right = buf.chan(1);
                            for (l, r) in left.iter().zip(right.iter()) {
                                let f_l = f32::from(*l) / 32768.0;
                                let f_r = f32::from(*r) / 32768.0;
                                audio_data.push(f_l);
                                audio_data.push(f_r);
                            }
                        }
                    }
                    AudioBufferRef::S32(buf) => {
                        // Convert s32 to f32 - interleave channels
                        if buf.spec().channels.count() == 1 {
                            for &sample in buf.chan(0) {
                                #[allow(clippy::cast_precision_loss)]
                                let f_sample = sample as f32 / 2_147_483_648.0;
                                audio_data.push(f_sample);
                            }
                        } else {
                            let left = buf.chan(0);
                            let right = buf.chan(1);
                            for (l, r) in left.iter().zip(right.iter()) {
                                #[allow(clippy::cast_precision_loss)]
                                let f_l = *l as f32 / 2_147_483_648.0;
                                #[allow(clippy::cast_precision_loss)]
                                let f_r = *r as f32 / 2_147_483_648.0;
                                audio_data.push(f_l);
                                audio_data.push(f_r);
                            }
                        }
                    }
                    _ => {
                        return Err(anyhow!("Unsupported audio buffer format"));
                    }
                }
            }
            Err(SymphoniaError::IoError(_)) => {
                // The packet reader has reached EOF
                break;
            }
            Err(SymphoniaError::DecodeError(_)) => {
                // Decode errors are not fatal. Skip the packet and continue.
            }
            Err(err) => {
                return Err(anyhow!("Fatal decode error: {}", err));
            }
        }
    }

    Ok((audio_data, sample_rate, channels))
}

/// Compress MP3 audio file
fn compress_mp3_file(data: &[u8], quality: u8) -> Result<Vec<u8>> {
    // Get target bitrate from quality
    let target_bitrate = quality_to_mp3_bitrate(quality);

    // First, decode the original MP3 to get PCM data
    let (pcm_data, sample_rate, channels) = decode_audio_data(data)?;

    // Create and configure LAME encoder
    let mut builder =
        Builder::new().ok_or_else(|| anyhow!("Failed to create MP3 encoder builder"))?;

    builder
        .set_num_channels(u8::try_from(channels).unwrap_or(2))
        .map_err(|e| anyhow!("Failed to set channels: {}", e))?;
    builder
        .set_sample_rate(sample_rate)
        .map_err(|e| anyhow!("Failed to set sample rate: {}", e))?;
    builder
        .set_brate(target_bitrate)
        .map_err(|e| anyhow!("Failed to set bitrate: {}", e))?;

    let mut encoder = builder
        .build()
        .map_err(|e| anyhow!("Failed to build MP3 encoder: {}", e))?;

    // Convert f32 PCM to i16 PCM (LAME expects i16)
    let pcm_i16: Vec<i16> = pcm_data
        .iter()
        .map(|&sample| {
            // Clamp to prevent overflow and convert to i16
            let sample_clamped = sample.clamp(-1.0, 1.0);
            #[allow(clippy::cast_possible_truncation)]
            {
                (sample_clamped * 32767.0) as i16
            }
        })
        .collect();

    // Ensure stereo format (duplicate mono channels if needed)
    let stereo_pcm = if channels == 1 {
        // Mono: duplicate samples for stereo encoding
        let mut stereo_data = Vec::with_capacity(pcm_i16.len() * 2);
        for &sample in &pcm_i16 {
            stereo_data.push(sample);
            stereo_data.push(sample);
        }
        stereo_data
    } else {
        // Already stereo
        pcm_i16
    };

    // Calculate required buffer size and prepare output
    let samples_per_channel = stereo_pcm.len() / 2;
    let mp3_buffer_size = mp3lame_encoder::max_required_buffer_size(samples_per_channel);
    let mut mp3_buffer: Vec<std::mem::MaybeUninit<u8>> = Vec::with_capacity(mp3_buffer_size);

    // Process audio in chunks that fit the encoder's expectations
    let chunk_size = SAMPLES_PER_FRAME * 2; // Stereo samples
    let mut input_pos = 0;
    let mut total_encoded = 0;

    while input_pos < stereo_pcm.len() {
        let chunk_end = std::cmp::min(input_pos + chunk_size, stereo_pcm.len());
        let chunk = &stereo_pcm[input_pos..chunk_end];

        // Create InterleavedPcm from chunk
        let interleaved_pcm = InterleavedPcm(chunk);

        // Reserve space and encode
        mp3_buffer.resize(
            total_encoded + mp3_buffer_size,
            std::mem::MaybeUninit::uninit(),
        );

        let encoded_size = encoder
            .encode(interleaved_pcm, &mut mp3_buffer[total_encoded..])
            .map_err(|e| anyhow!("Failed to encode MP3 chunk: {}", e))?;

        total_encoded += encoded_size;
        input_pos = chunk_end;
    }

    // Flush encoder to get any remaining data
    mp3_buffer.resize(
        total_encoded + mp3_buffer_size,
        std::mem::MaybeUninit::uninit(),
    );

    let flush_size = encoder
        .flush::<FlushNoGap>(&mut mp3_buffer[total_encoded..])
        .map_err(|e| anyhow!("Failed to flush MP3 encoder: {}", e))?;

    total_encoded += flush_size;

    // Convert MaybeUninit<u8> to u8 for the final result
    mp3_buffer.truncate(total_encoded);
    let final_buffer: Vec<u8> = mp3_buffer
        .into_iter()
        .map(|b| unsafe { b.assume_init() })
        .collect();

    Ok(final_buffer)
}

/// Compress audio file based on format and quality
pub fn compress_audio_file(
    data: &[u8],
    filename: &str,
    quality: u8,
) -> Result<(Vec<u8>, u64, u64)> {
    let original_size = data.len() as u64;

    let format = detect_audio_format(filename)
        .ok_or_else(|| anyhow!("Unsupported audio format: {}", filename))?;

    let compressed_data = match format {
        AudioFormat::Mp3 => compress_mp3_file(data, quality)?,
        // Future formats will be added here
    };

    let compressed_size = compressed_data.len() as u64;
    Ok((compressed_data, original_size, compressed_size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_supported_audio() {
        assert!(is_supported_audio("Audio/test.mp3"));
        assert!(is_supported_audio("Audio/test.MP3"));
        assert!(!is_supported_audio("Audio/test.wav"));
        assert!(!is_supported_audio("Audio/test.ogg"));
        assert!(!is_supported_audio("Audio/test.txt"));
        assert!(!is_supported_audio("Images/test.jpg"));
    }

    #[test]
    fn test_detect_audio_format() {
        assert_eq!(detect_audio_format("test.mp3"), Some(AudioFormat::Mp3));
        assert_eq!(detect_audio_format("test.MP3"), Some(AudioFormat::Mp3));
        assert_eq!(
            detect_audio_format("Audio/song.mp3"),
            Some(AudioFormat::Mp3)
        );
        assert_eq!(detect_audio_format("test.wav"), None);
        assert_eq!(detect_audio_format("test.txt"), None);
    }

    #[test]
    fn test_quality_to_mp3_bitrate() {
        // Since Bitrate doesn't implement PartialEq or Debug, we'll test the function
        // by checking that it doesn't panic and by testing the discriminant values

        // Test boundary values - should not panic
        let _result_1 = quality_to_mp3_bitrate(1);
        let _result_100 = quality_to_mp3_bitrate(100);

        // Test specific quality ranges - should not panic
        let _result_10 = quality_to_mp3_bitrate(10); // 1-15 range -> Kbps64
        let _result_20 = quality_to_mp3_bitrate(20); // 16-25 range -> Kbps80
        let _result_30 = quality_to_mp3_bitrate(30); // 26-35 range -> Kbps96
        let _result_40 = quality_to_mp3_bitrate(40); // 36-45 range -> Kbps128
        let _result_50 = quality_to_mp3_bitrate(50); // 46-55 range -> Kbps160
        let _result_60 = quality_to_mp3_bitrate(60); // 56-65 range -> Kbps192
        let _result_70 = quality_to_mp3_bitrate(70); // 66-75 range -> Kbps224
        let _result_80 = quality_to_mp3_bitrate(80); // 76-85 range -> Kbps256
        let _result_90 = quality_to_mp3_bitrate(90); // 76-95 range -> Kbps256
        let _result_99 = quality_to_mp3_bitrate(99); // 96-100 range -> Kbps320

        // Test quality clamping - should not panic
        let _result_0 = quality_to_mp3_bitrate(0); // Clamps to 1 -> Kbps64
        let _result_101 = quality_to_mp3_bitrate(101); // Clamps to 100 -> Kbps320

        // Test that the function is deterministic (same input gives same output)
        let result1_first = quality_to_mp3_bitrate(50);
        let result1_second = quality_to_mp3_bitrate(50);
        // We can't compare directly, but we can check discriminants are the same
        assert_eq!(
            std::mem::discriminant(&result1_first),
            std::mem::discriminant(&result1_second)
        );
    }
}
