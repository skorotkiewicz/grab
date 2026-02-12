use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::header::{HeaderMap, RANGE};
use reqwest::Client;
use tokio::fs::{metadata, File, OpenOptions};
use tokio::io::{AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tokio::sync::Semaphore;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "downloader")]
#[command(about = "An asynchronous file downloader", arg_required_else_help = true)]
struct Args {
    /// URL to download
    #[arg(required = true)]
    url: String,

    /// Output file (optional, derived from URL if missing)
    #[arg(short = 'O', long)]
    output: Option<String>,

    /// Resume download
    #[arg(short = 'c', long, default_value_t = false)]
    resume: bool,

    /// Number of concurrent connections
    #[arg(short = 't', long, default_value_t = 4)]
    threads: usize,

    /// Chunk size in bytes
    #[arg(short = 's', long, default_value_t = 1048576)]
    chunk_size: u64,

    /// User Agent string
    #[arg(short = 'u', long, default_value = "RustDownloader/1.0")]
    user_agent: String,

    /// Timeout in seconds
    #[arg(short = 'T', long, default_value = "30", value_parser = parse_duration)]
    timeout: Duration,

    /// Bandwidth limit (e.g. 512K, 1M, 2M)
    #[arg(short = 'l', long, value_parser = parse_bandwidth)]
    limit_rate: Option<u64>,
}

fn parse_bandwidth(arg: &str) -> Result<u64, String> {
    let s = arg.to_uppercase();
    let (num_str, multiplier) = if s.ends_with('K') {
        (&s[..s.len() - 1], 1024)
    } else if s.ends_with('M') {
        (&s[..s.len() - 1], 1024 * 1024)
    } else if s.ends_with('G') {
        (&s[..s.len() - 1], 1024 * 1024 * 1024)
    } else {
        (s.as_str(), 1)
    };

    num_str.parse::<u64>()
        .map(|n| n * multiplier)
        .map_err(|e| format!("Invalid bandwidth limit: {}", e))
}

fn parse_duration(arg: &str) -> Result<Duration, std::num::ParseIntError> {
    let seconds = arg.parse::<u64>()?;
    Ok(Duration::from_secs(seconds))
}

#[derive(Debug)]
struct DownloadConfig {
    url: String,
    output_path: String,
    concurrent_chunks: usize,
    chunk_size: u64,
    resume: bool,
    user_agent: String,
    timeout: Duration,
    limit_rate: Option<u64>,
}

struct BandwidthLimiter {
    bytes_per_second: u64,
    start_instant: tokio::time::Instant,
    total_bytes_transferred: std::sync::atomic::AtomicU64,
}

impl BandwidthLimiter {
    fn new(bytes_per_second: u64) -> Self {
        Self {
            bytes_per_second,
            start_instant: tokio::time::Instant::now(),
            total_bytes_transferred: std::sync::atomic::AtomicU64::new(0),
        }
    }

    async fn throttle(&self, bytes: u64) {
        if self.bytes_per_second == 0 {
            return;
        }

        let total = self.total_bytes_transferred.fetch_add(bytes, std::sync::atomic::Ordering::Relaxed) + bytes;
        
        let elapsed = self.start_instant.elapsed();
        let expected_duration = Duration::from_secs_f64(total as f64 / self.bytes_per_second as f64);

        if elapsed < expected_duration {
            tokio::time::sleep(expected_duration - elapsed).await;
        }
    }
}

struct FileDownloader {
    client: Client,
    config: DownloadConfig,
    limiter: Option<Arc<BandwidthLimiter>>,
}

impl FileDownloader {
    fn new(config: DownloadConfig) -> Self {
        let client = Client::builder()
            .user_agent(&config.user_agent)
            .connect_timeout(config.timeout)
            .build()
            .expect("Failed to create HTTP client");

        let limiter = config.limit_rate.map(|limit| Arc::new(BandwidthLimiter::new(limit)));

        Self { client, config, limiter }
    }

    async fn download(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        println!("Downloading: {}", self.config.url);
        
        let response = self.client.head(&self.config.url).send().await?;
        let total_size = response
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|ct_len| ct_len.to_str().ok())
            .and_then(|ct_len| ct_len.parse().ok())
            .unwrap_or(0);

        if total_size == 0 {
            return self.download_single_threaded(0).await;
        }

        let supports_range = response
            .headers()
            .get(reqwest::header::ACCEPT_RANGES)
            .map(|h| h == "bytes")
            .unwrap_or(false);

        println!("Length: {}", format_bytes(total_size));

        let mut already_downloaded = 0u64;
        let file_exists = Path::new(&self.config.output_path).exists();

        if self.config.resume && file_exists {
            if let Ok(meta) = metadata(&self.config.output_path).await {
                already_downloaded = meta.len();
                if already_downloaded >= total_size {
                    println!("File already fully downloaded.");
                    return Ok(())
                }
                println!("Resuming from {}", format_bytes(already_downloaded));
            }
        } else if file_exists {
            File::create(&self.config.output_path).await?;
        }

        if supports_range && !self.config.resume && total_size > self.config.chunk_size {
            // Multi-threading is only safe for fresh downloads to avoid holes
            self.download_multi_threaded(total_size).await
        } else {
            // For resume or non-range servers, use reliable sequential download
            self.download_single_threaded(already_downloaded).await
        }
    }

    async fn download_single_threaded(&self, start_pos: u64) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut headers = HeaderMap::new();
        if start_pos > 0 {
            headers.insert(RANGE, format!("bytes={}-", start_pos).parse().unwrap());
        }

        let mut response = tokio::time::timeout(
            self.config.timeout,
            self.client.get(&self.config.url).headers(headers).send()
        ).await??;
        let total_size = response.content_length().unwrap_or(0) + start_pos;

        let pb = ProgressBar::new(total_size);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})")
                .unwrap()
                .progress_chars("=>-"),
                // .progress_chars("██▉▊▋▌▍▎▏  "),
        );
        pb.set_position(start_pos);

        let mut file = if start_pos > 0 {
            OpenOptions::new().write(true).open(&self.config.output_path).await?
        } else {
            File::create(&self.config.output_path).await?
        };

        if start_pos > 0 {
            file.seek(SeekFrom::Start(start_pos)).await?;
        }

        while let Some(chunk) = tokio::time::timeout(self.config.timeout, response.chunk()).await?? {
            file.write_all(&chunk).await?;
            pb.inc(chunk.len() as u64);
            if let Some(ref limiter) = self.limiter {
                limiter.throttle(chunk.len() as u64).await;
            }
        }

        pb.finish_with_message("Download completed.");
        Ok(())
    }

    async fn download_multi_threaded(&self, total_size: u64) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let num_chunks = std::cmp::min(
            self.config.concurrent_chunks,
            (total_size / self.config.chunk_size + 1) as usize,
        );

        println!("Using {} concurrent connections", num_chunks);

        let pb = ProgressBar::new(total_size);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})")
                .unwrap()
                .progress_chars("██▉▊▋▌▍▎▏  "),
                // .progress_chars("=>-"),
        );

        let semaphore = Arc::new(Semaphore::new(num_chunks));
        let pb = Arc::new(pb);
        let mut handles = Vec::new();

        // For multi-threaded writing without gaps, we pre-create the file empty
        File::create(&self.config.output_path).await?;

        for i in 0..num_chunks {
            let chunk_range_size = total_size / num_chunks as u64;
            let start = i as u64 * chunk_range_size;
            let end = if i == num_chunks - 1 {
                total_size - 1
            } else {
                ((i + 1) as u64 * chunk_range_size) - 1
            };

            let client = self.client.clone();
            let url = self.config.url.clone();
            let output_path = self.config.output_path.clone();
            let pb_clone = pb.clone();
            let semaphore_clone = semaphore.clone();

            let timeout = self.config.timeout;
            let limiter = self.limiter.clone();
            let handle = tokio::spawn(async move {
                let _permit = semaphore_clone.acquire().await.unwrap();
                download_chunk(client, url, output_path, start, end, pb_clone, timeout, limiter).await
            });

            handles.push(handle);
        }

        for handle in handles {
            handle.await??;
        }

        pb.finish_with_message("Download completed.");
        Ok(())
    }
}

async fn download_chunk(
    client: Client,
    url: String,
    output_path: String,
    start: u64,
    end: u64,
    pb: Arc<ProgressBar>,
    timeout: Duration,
    limiter: Option<Arc<BandwidthLimiter>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut headers = HeaderMap::new();
    headers.insert(RANGE, format!("bytes={}-{}", start, end).parse().unwrap());

    let mut response = tokio::time::timeout(
        timeout,
        client.get(&url).headers(headers).send()
    ).await??;
    
    let mut file = OpenOptions::new()
        .write(true)
        .open(&output_path).await?;
    
    file.seek(SeekFrom::Start(start)).await?;

    while let Some(chunk) = tokio::time::timeout(timeout, response.chunk()).await?? {
        file.write_all(&chunk).await?;
        pb.inc(chunk.len() as u64);
        if let Some(ref lim) = limiter {
            lim.throttle(chunk.len() as u64).await;
        }
    }

    Ok(())
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit_index = 0;

    while size >= 1024.0 && unit_index < UNITS.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }

    format!("{:.2} {}", size, UNITS[unit_index])
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = Args::parse();
    
    let output_path = args.output.unwrap_or_else(|| {
        args.url
            .split('/')
            .last()
            .filter(|s| !s.is_empty())
            .unwrap_or("index.html")
            .to_string()
    });

    let config = DownloadConfig {
        url: args.url,
        output_path,
        concurrent_chunks: args.threads,
        chunk_size: args.chunk_size,
        resume: args.resume,
        user_agent: args.user_agent,
        timeout: args.timeout,
        limit_rate: args.limit_rate,
    };

    
    let downloader = FileDownloader::new(config);
    
    if let Err(e) = downloader.download().await {
        eprintln!("\nError: Download failed: {}", e);
        std::process::exit(1);
    }

    Ok(())
}