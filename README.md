# sicom 📦

**SI COMpress** - A fast, efficient compression utility for SIGame pack files (.siq)

`sicom` reduces the size of SIGame pack files by compressing images, audio, and video content using modern, web-compatible formats. Achieve 50-80% size reduction while maintaining compatibility with all SIGame applications.

> **Note**: This project is not affiliated with the original [SIGame project](https://github.com/VladimirKhil/SI) by Vladimir Khil, but is designed to work seamlessly with SIGame pack files.

## ✨ Features

### 🖼️ Image Compression
- **Formats**: JPG, JPEG, PNG, WebP → WebP
- **Compression**: Lossless (quality ≥95) or lossy with quality control
- **Compatibility**: 95%+ browser support across all modern browsers
- **Benefits**: 25-50% smaller than JPEG, 25-35% smaller than PNG

### 🎵 Audio Compression  
- **Formats**: MP3 → MP3 (re-encoded with quality control)
- **Bitrates**: 64-320 kbps based on quality setting
- **Optimization**: Smart bitrate selection based on real-world SIGame pack analysis

### 🎬 Video Compression
- **Formats**: MP4, MOV, AVI, MKV → MP4 (HEVC/H.265)
- **Compression**: 60-80% size reduction compared to H.264
- **Quality**: CRF-based encoding with presets
- **Requirements**: System FFmpeg installation required

## 🌐 Why These Formats?

### WebP Images
- **Browser Support**: 95%+ compatibility (Chrome, Firefox, Safari, Edge)
- **Compression**: Superior to JPEG (25-50% smaller) and PNG (up to 35% smaller)
- **Features**: Supports both lossy and lossless compression with alpha transparency

### HEVC/H.265 Video  
- **Compression**: 50% better than H.264 at same quality
- **Browser Support**: Growing support in modern browsers (Safari, Chrome 107+)
- **Future-Proof**: Industry standard for high-efficiency video compression

## 🚀 Installation

### Prerequisites
- **Rust** (1.85 or later) - Install from [rustup.rs](https://rustup.rs/)
- **FFmpeg** (for video compression) - Required for video processing

#### Install FFmpeg:
```bash
# macOS (Homebrew)
brew install ffmpeg

# Ubuntu/Debian
sudo apt update && sudo apt install ffmpeg

# Windows (Chocolatey)
choco install ffmpeg

# Or download from https://ffmpeg.org/download.html
```

### Build from Source
```bash
git clone https://github.com/bvc3at/sicom.git
cd sicom
cargo build --release
```

The compiled binary will be available at `target/release/sicom`.

## 📖 Usage

### Basic Compression
```bash
# Compress with default quality settings
sicom compress input.siq

# Compress with custom output filename
sicom compress input.siq compressed_output.siq
```

### Quality Control
```bash
# High quality compression (larger files)
sicom compress input.siq --image-quality 95 --audio-quality 90 --video-quality 85

# Aggressive compression (smaller files)
sicom compress input.siq --image-quality 60 --audio-quality 70 --video-quality 60
```

### Selective Compression
```bash
# Skip video compression (if FFmpeg not available)
sicom compress input.siq --skip-video

# Only compress images
sicom compress input.siq --skip-audio --skip-video

# Always use compressed files even if larger
sicom compress input.siq --always-compress
```

### Advanced Options
```bash
# Custom FFmpeg path
sicom compress input.siq --ffmpeg-path /usr/local/bin/ffmpeg

# Full control
sicom compress input.siq \
  --image-quality 80 \
  --audio-quality 85 \
  --video-quality 75 \
  --ffmpeg-path /custom/path/ffmpeg
```

## 📊 Compression Results

Typical size reductions on real SIGame packs:

| Media Type | Original Format | Compressed Format | Size Reduction |
|------------|----------------|-------------------|----------------|
| Images     | JPG/PNG        | WebP             | 30-50%         |
| Audio      | MP3            | MP3 (optimized)  | 10-30%         |  
| Video      | H.264/AVC      | HEVC/H.265       | 60-80%         |
| **Overall Pack** | **.siq**   | **.siq**         | **50-70%**     |

## ⚙️ Current Limitations

- **Audio**: Only MP3 files are supported (WAV, OGG, FLAC support planned)
- **Images**: All images are converted to WebP format
- **Video**: Requires system FFmpeg installation for processing
- **Formats**: Limited to formats commonly found in SIGame packs

## 🔧 Technical Details

### SIGame Pack Format
SIGame packs (`.siq` files) are ZIP archives containing:
- `content.xml` - Questions and metadata
- `Images/` - Image files (JPG, PNG, WebP)
- `Audio/` - Audio files (MP3, WAV, OGG)
- `Video/` - Video files (MP4, AVI, MOV)

### Intelligent Compression
- **Size Comparison**: Only uses compressed versions if they're actually smaller
- **Quality Preservation**: Maintains visual/audio quality while reducing file size
- **Path Updates**: Automatically updates `content.xml` references for format changes
- **Error Handling**: Gracefully handles unsupported files by copying originals

### Performance
- **Progress Bars**: Real-time compression progress with ETA
- **Parallel Processing**: Efficient handling of large media files
- **Memory Efficient**: Streams large files without loading entirely into memory

## 🤝 Contributing

This is an open-source project under the MIT License. Contributions are welcome!

### Development Setup
```bash
git clone https://github.com/your-username/sicom.git
cd sicom
cargo build
cargo test
```

### Planned Features
- WAV and OGG audio support
- AVIF image format support  
- AV1 video codec support
- Batch processing multiple packs
- TUI application
- (Maybe) WebApp?

## 📄 License

MIT License - see [LICENSE](LICENSE) file for details.

## 🙏 Acknowledgments

- [SIGame](https://github.com/VladimirKhil/SI) by Vladimir Khil - The original quiz platform
- [FFmpeg](https://ffmpeg.org/) - Video processing capabilities
- Rust community for excellent multimedia libraries
- [Claude](https://claude.ai/) - This project was partially vibe coded with claude-code

---

**sicom** - Making SIGame packs smaller, faster, and more efficient! 🚀
