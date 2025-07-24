/// Statistics tracking for compression operations
#[derive(Debug, Default)]
pub struct CompressionStats {
    // Image statistics
    images_processed: u32,
    images_skipped: u32,
    images_kept_original: u32,
    image_original_size: u64,
    image_compressed_size: u64,

    // Audio statistics
    audio_processed: u32,
    audio_skipped: u32,
    audio_kept_original: u32,
    audio_original_size: u64,
    audio_compressed_size: u64,

    // Video statistics
    video_processed: u32,
    video_skipped: u32,
    video_kept_original: u32,
    video_original_size: u64,
    video_compressed_size: u64,

    // Overall statistics
    total_input_size: u64,
    total_output_size: u64,
    total_updated_refs: u32,
}

impl CompressionStats {
    pub fn new() -> Self {
        Self::default()
    }

    // Image tracking methods
    pub fn add_processed_image(&mut self, original_size: u64, compressed_size: u64) {
        self.images_processed += 1;
        self.image_original_size += original_size;
        self.image_compressed_size += compressed_size;
        self.total_input_size += original_size;
        self.total_output_size += compressed_size;
    }

    pub fn add_kept_original_image(&mut self, size: u64) {
        self.images_kept_original += 1;
        self.image_original_size += size;
        self.image_compressed_size += size;
        self.total_input_size += size;
        self.total_output_size += size;
    }

    pub fn add_skipped_image(&mut self, size: u64) {
        self.images_skipped += 1;
        self.image_original_size += size;
        self.image_compressed_size += size;
        self.total_input_size += size;
        self.total_output_size += size;
    }

    // Audio tracking methods
    pub fn add_processed_audio(&mut self, original_size: u64, compressed_size: u64) {
        self.audio_processed += 1;
        self.audio_original_size += original_size;
        self.audio_compressed_size += compressed_size;
        self.total_input_size += original_size;
        self.total_output_size += compressed_size;
    }

    pub fn add_kept_original_audio(&mut self, size: u64) {
        self.audio_kept_original += 1;
        self.audio_original_size += size;
        self.audio_compressed_size += size;
        self.total_input_size += size;
        self.total_output_size += size;
    }

    pub fn add_skipped_audio(&mut self, size: u64) {
        self.audio_skipped += 1;
        self.audio_original_size += size;
        self.audio_compressed_size += size;
        self.total_input_size += size;
        self.total_output_size += size;
    }

    // Video tracking methods
    pub fn add_processed_video(&mut self, original_size: u64, compressed_size: u64) {
        self.video_processed += 1;
        self.video_original_size += original_size;
        self.video_compressed_size += compressed_size;
        self.total_input_size += original_size;
        self.total_output_size += compressed_size;
    }

    pub fn add_kept_original_video(&mut self, size: u64) {
        self.video_kept_original += 1;
        self.video_original_size += size;
        self.video_compressed_size += size;
        self.total_input_size += size;
        self.total_output_size += size;
    }

    pub fn add_skipped_video(&mut self, size: u64) {
        self.video_skipped += 1;
        self.video_original_size += size;
        self.video_compressed_size += size;
        self.total_input_size += size;
        self.total_output_size += size;
    }

    // Other file tracking
    pub fn add_other_file(&mut self, size: u64) {
        self.total_input_size += size;
        self.total_output_size += size;
    }

    pub fn add_updated_refs(&mut self, count: u32) {
        self.total_updated_refs += count;
    }

    // Calculation methods
    pub fn total_compression_ratio(&self) -> f64 {
        if self.total_input_size > 0 {
            (1.0 - self.total_output_size as f64 / self.total_input_size as f64) * 100.0
        } else {
            0.0
        }
    }

    pub fn image_compression_ratio(&self) -> f64 {
        if self.image_original_size > 0 {
            (1.0 - self.image_compressed_size as f64 / self.image_original_size as f64) * 100.0
        } else {
            0.0
        }
    }

    pub fn audio_compression_ratio(&self) -> f64 {
        if self.audio_original_size > 0 {
            (1.0 - self.audio_compressed_size as f64 / self.audio_original_size as f64) * 100.0
        } else {
            0.0
        }
    }

    pub fn video_compression_ratio(&self) -> f64 {
        if self.video_original_size > 0 {
            (1.0 - self.video_compressed_size as f64 / self.video_original_size as f64) * 100.0
        } else {
            0.0
        }
    }

    // Getter methods for public access to statistics
    pub fn images_processed(&self) -> u32 {
        self.images_processed
    }
    pub fn images_skipped(&self) -> u32 {
        self.images_skipped
    }
    pub fn images_kept_original(&self) -> u32 {
        self.images_kept_original
    }
    pub fn image_original_size(&self) -> u64 {
        self.image_original_size
    }
    pub fn image_compressed_size(&self) -> u64 {
        self.image_compressed_size
    }

    pub fn audio_processed(&self) -> u32 {
        self.audio_processed
    }
    pub fn audio_skipped(&self) -> u32 {
        self.audio_skipped
    }
    pub fn audio_kept_original(&self) -> u32 {
        self.audio_kept_original
    }
    pub fn audio_original_size(&self) -> u64 {
        self.audio_original_size
    }
    pub fn audio_compressed_size(&self) -> u64 {
        self.audio_compressed_size
    }

    pub fn video_processed(&self) -> u32 {
        self.video_processed
    }
    pub fn video_skipped(&self) -> u32 {
        self.video_skipped
    }
    pub fn video_kept_original(&self) -> u32 {
        self.video_kept_original
    }
    pub fn video_original_size(&self) -> u64 {
        self.video_original_size
    }
    pub fn video_compressed_size(&self) -> u64 {
        self.video_compressed_size
    }

    pub fn total_input_size(&self) -> u64 {
        self.total_input_size
    }
    pub fn total_output_size(&self) -> u64 {
        self.total_output_size
    }
}
