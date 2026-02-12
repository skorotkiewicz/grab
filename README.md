# 🦀 Grab

[![Rust](https://img.shields.io/badge/rust-1.75+-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-lightgrey.svg)]()

A high-performance asynchronous file downloader written in Rust. `grab` combines the simplicity of `wget` with the power of multi-threaded concurrency, parallel file downloads, and modern async I/O. Inspired by the efficiency of `pacman`.

![intro](intro.png)

 <details>
  <summary>✨ Features</summary>

- 🚀 **Multi-threaded Downloads**: Concurrent connections per file for maximum speed.
- 📂 **Parallel File Downloads**: Download multiple files simultaneously with intelligent queueing.
- ⏯️ **Smart Resume**: Detects existing partial downloads and continues seamlessly using reliable sequential streams.
- 🐚 **Command Line Power**: Supports multiple URL arguments or reading a list of URLs from `stdin`.
- 📊 **Multi-Progress UI**: Beautiful, pacman-inspired progress bars showing individual file status and total progress.
- 🛡️ **Inactivity Timeout**: Intelligent timeout logic that only triggers if a download actually stalls.
- ⏳ **Bandwidth Limiting**: Global rate limiting across all concurrent downloads.
- 🔄 **Auto-Filename**: Automatically derives filenames from URLs (including query parameters) if no output is specified.
- 🛠️ **Pure Async**: Built on `tokio` and `reqwest` for maximum efficiency.
</details> 

## Installation

### Arch Linux (AUR)

You can install `grab` from the AUR using your favorite helper:

```bash
yay -S grab-bin
# or
paru -S grab-bin
```

### From Source

```bash
# Clone the repository
git clone https://github.com/skorotkiewicz/grab
cd grab

# Install from source
cargo install --path .

# Build from source
cargo build --release

# The binary will be available at ./target/release/grab
```

## Usage

```bash
grab [OPTIONS] <URL>...
# OR
cat urls.txt | grab [OPTIONS]
```

### Examples

**Basic Single Download**:
```bash
./grab https://example.com/file.zip
```

**Multiple Parallel Downloads**:
```bash
./grab https://example.com/file1.zip https://example.com/file2.zip
```

**Piping From a List**:
```bash
cat url_lists.txt | grab -j 10
```

**Resume Interrupted Downloads**:
```bash
./grab -c https://example.com/large_file.iso
```

**Limit Global Bandwidth**:
```bash
./grab --limit-rate 1M -j 5 https://example.com/file1.zip https://example.com/file2.zip
```

### Options

| Flag | Long Flag | Description | Default |
|------|-----------|-------------|---------|
| `-O` | `--output` | Output filename (single URL only) | Derived from URL |
| `-c` | `--resume` | Resume partial download | `false` |
| `-t` | `--threads` | Concurrent connections *per file* | `4` |
| `-j` | `--parallel-downloads` | Max parallel *file* downloads | `5` |
| `-s` | `--chunk-size` | Chunk size in bytes | `1048576` (1MB) |
| `-u` | `--user-agent` | HTTP User-Agent string | `Grab/2.0` |
| `-T` | `--timeout` | Inactivity timeout (seconds) | `30` |
| `-l` | `--limit-rate` | Bandwidth limit (e.g. 512K, 1M) | None |
| `-4` | `--inet4-only` | Force IPv4 only | `false` |
| `-6` | `--inet6-only` | Force IPv6 only | `false` |

## Architecture

### Parallelism Model

- **Inter-file Parallelism (`-j`)**: `grab` uses a semaphore to limit how many files are being downloaded at once.
- **Intra-file Parallelism (`-t`)**: For each file, `grab` can spawn multiple range-request tasks to saturate individual connections (only for fresh downloads).

### Multi-threading vs. Resume

- **Fresh Downloads**: Uses concurrent connections to saturate your bandwidth by requesting different byte ranges simultaneously.
- **Resumes**: Uses a single, high-integrity sequential stream starting from the end of your local file. This ensures perfect file integrity and avoids the "holes" or "gaps" often found in multi-threaded resumes.

### Inactivity Timeout

Unlike simple request timeouts, `grab` monitors the *flow* of data. If the server is slow but steady, the download continues. If no bytes are received for the duration of the timeout, it gracefully errors, allowing for a manual or automated retry.

## Reliability

- **Transactional Writes**: Files are opened with standard POSIX flags ensuring data is written where it belongs.
- **Zero Pre-allocation**: Doesn't waste disk space or time pre-allocating large files before the data actually arrives.
- **Error Recovery**: Handles network drops and server timeouts by reporting them clearly so you can resume.

## Dependencies

- **reqwest**: Leading HTTP client for Rust.
- **tokio**: Industry-standard async runtime.
- **indicatif**: Beautiful CLI progress reporting.
- **clap**: Robust command-line argument parsing.

---

**Made with ❤️ in Rust**
*Fast, light, and reliable.* 🚀