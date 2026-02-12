# 🦀 Grab

[![Rust](https://img.shields.io/badge/rust-1.75+-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-lightgrey.svg)]()

A high-performance asynchronous file downloader written in Rust. `grab` combines the simplicity of `wget` with the power of multi-threaded concurrency and modern async I/O.

## ✨ Features

- 🚀 **Multi-threaded Downloads**: Concurrent connections for maximum speed on fresh downloads.
- ⏯️ **Smart Resume**: Detects existing partial downloads and continues seamlessly using a reliable sequential stream.
- 🐚 **Clean CLI**: Professional, non-interactive interface with POSIX-style flags.
- 📊 **Real-time Progress**: Beautiful progress bars with live speed, byte counts, and ETA.
- 🛡️ **Inactivity Timeout**: Intelligent timeout logic that only triggers if the download actually stalls, not if it's just slow.
- 🔄 **Auto-Filename**: Automatically derives filenames from URLs (including query parameters) if no output is specified.
- 🛠️ **Pure Async**: Built on `tokio` and `reqwest` for maximum efficiency and low resource usage.

## 🚀 Quick Start

### Prerequisites

- Rust 1.75+ installed ([rustup.rs](https://rustup.rs/))

### Installation

```bash
# Clone the repository
git clone https://github.com/skorotkiewicz/grab
cd grab

# Build release version
cargo build --release

# The binary will be available at ./target/release/grab
```

## 🎮 Usage

```bash
grab [OPTIONS] <URL>
```

### Examples

**Basic Download** (auto-detects filename):
```bash
./grab https://example.com/file.zip
```

**Custom Output Filename**:
```bash
./grab -O my_file.zip https://example.com/file.zip
```

**Resume an Interrupted Download**:
```bash
./grab -c https://example.com/large_file.iso
```

**Increase Speed with More Threads** (for new downloads):
```bash
./grab -t 16 https://example.com/fast_file.bin
```

**Limit Download Speed**:
```bash
./grab --limit-rate 512K https://example.com/large_file.zip
```

### Options

| Flag | Long Flag | Description | Default |
|------|-----------|-------------|---------|
| `-O` | `--output` | Output filename | Derived from URL |
| `-c` | `--resume` | Resume partial download | `false` |
| `-t` | `--threads` | Concurrent connections | `4` |
| `-s` | `--chunk-size` | Chunk size in bytes | `1048576` (1MB) |
| `-u` | `--user-agent` | HTTP User-Agent string | `RustDownloader/1.0` |
| `-T` | `--timeout` | Inactivity timeout (seconds) | `30` |
| `-l` | `--limit-rate` | Bandwidth limit (e.g. 512K, 1M) | None |

## 🏗️ Architecture

### Multi-threading vs. Resume

- **Fresh Downloads**: Uses concurrent connections to saturate your bandwidth by requesting different byte ranges simultaneously.
- **Resumes**: Uses a single, high-integrity sequential stream starting from the end of your local file. This ensures perfect file integrity and avoids the "holes" or "gaps" often found in multi-threaded resumes.

### Inactivity Timeout

Unlike simple request timeouts, `grab` monitors the *flow* of data. If the server is slow but steady, the download continues. If no bytes are received for the duration of the timeout, it gracefully errors, allowing for a manual or automated retry.

## 🛡️ Reliability

- **Transactional Writes**: Files are opened with standard POSIX flags ensuring data is written where it belongs.
- **Zero Pre-allocation**: Doesn't waste disk space or time pre-allocating large files before the data actually arrives.
- **Error Recovery**: Handles network drops and server timeouts by reporting them clearly so you can resume.

## 📦 Dependencies

- **reqwest**: Leading HTTP client for Rust.
- **tokio**: Industry-standard async runtime.
- **indicatif**: Beautiful CLI progress reporting.
- **clap**: Robust command-line argument parsing.

---

**Made with ❤️ in Rust**
*Fast, light, and reliable.* 🚀