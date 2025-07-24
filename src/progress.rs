use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

pub struct ProgressLogger {
    progress_bar: ProgressBar,
    video_progress_bar: Option<ProgressBar>, // Video encoding progress
}

impl ProgressLogger {
    pub fn new(total_files: u64, multi_progress: &MultiProgress) -> Self {
        // Create main progress bar
        let progress_bar = multi_progress.add(ProgressBar::new(total_files));
        progress_bar.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} files (ETA: {eta})")
                .unwrap()
                .progress_chars("#>-"),
        );

        Self {
            progress_bar,
            video_progress_bar: None,
        }
    }

    pub fn inc(&mut self) {
        self.progress_bar.inc(1);
    }

    pub fn start_video_progress(&mut self, filename: &str, multi_progress: &MultiProgress) {
        let video_bar = multi_progress.add(ProgressBar::new(100));
        video_bar.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.blue} Encoding {msg}: [{wide_bar:.yellow/blue}] {percent}%")
                .unwrap()
                .progress_chars("#>-"),
        );
        video_bar.set_message(filename.to_string());
        self.video_progress_bar = Some(video_bar);
    }

    pub fn finish_video_progress(&mut self) {
        if let Some(bar) = self.video_progress_bar.take() {
            bar.finish_and_clear();
        }
    }

    pub fn finish(&mut self) {
        // Finish video progress bar if still active
        self.finish_video_progress();

        // Finish and clear the main progress bar
        self.progress_bar.finish_and_clear();
    }

    pub fn video_progress_bar(&self) -> Option<&ProgressBar> {
        self.video_progress_bar.as_ref()
    }
}

/// Get ANSI color code for log level
pub const fn get_log_color(level: log::Level) -> &'static str {
    match level {
        log::Level::Error => "\x1b[91m",                     // Red
        log::Level::Warn => "\x1b[33m",                      // Orange-red/Yellow
        log::Level::Info => "\x1b[32m",                      // Darker green (same as Cargo)
        log::Level::Debug | log::Level::Trace => "\x1b[90m", // Grey
    }
}

/// Get ANSI color code for log level with module-specific overrides
pub fn get_log_color_with_module(level: log::Level, module_path: Option<&str>) -> &'static str {
    // Special handling for Symphonia library messages
    if let Some(module) = module_path {
        if module.starts_with("symphonia") && level == log::Level::Info {
            // Make Symphonia info messages gray to reduce visual noise
            return "\x1b[90m"; // Grey
        }
    }

    // Use default color for all other cases
    get_log_color(level)
}
