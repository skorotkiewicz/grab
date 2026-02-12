use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::header::{HeaderMap, RANGE};
use reqwest::Client;
use tokio::fs::{metadata, File, OpenOptions};
use tokio::io::{AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tokio::sync::Semaphore;
use std::net::IpAddr;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "grab")]
#[command(about = "Asynchronous file downloader")]
struct Args {
    /// URLs to download
    #[arg(num_args = 0..)]
    urls: Vec<String>,

    /// Output file (only works for single URL)
    #[arg(short = 'O', long)]
    output: Option<String>,

    /// Resume download
    #[arg(short = 'c', long, default_value_t = false)]
    resume: bool,

    /// Number of concurrent chunks per file
    #[arg(short = 't', long, default_value_t = 4)]
    threads: usize,

    /// Number of parallel file downloads
    #[arg(short = 'j', long, default_value_t = 5)]
    parallel_downloads: usize,

    /// Chunk size in bytes
    #[arg(short = 's', long, default_value_t = 1048576)]
    chunk_size: u64,

    /// User Agent string
    #[arg(short = 'u', long, default_value = "Grab/2.0")]
    user_agent: String,

    /// Timeout in seconds
    #[arg(short = 'T', long, default_value = "30", value_parser = parse_duration)]
    timeout: Duration,

    /// Bandwidth limit (e.g. 512K, 1M, 2M)
    #[arg(short = 'l', long, value_parser = parse_bandwidth)]
    limit_rate: Option<u64>,

    /// Force IPv4 only
    #[arg(short = '4', long, conflicts_with = "inet6_only")]
    inet4_only: bool,

    /// Force IPv6 only
    #[arg(short = '6', long, conflicts_with = "inet4_only")]
    inet6_only: bool,
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
    force_ipv4: bool,
    force_ipv6: bool,
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
struct DownloadState {
    total_files: usize,
    finished_files: std::sync::atomic::AtomicUsize,
    total_pb: ProgressBar,
}


struct FileDownloader {
    client: Client,
    config: Arc<DownloadConfig>,
    limiter: Option<Arc<BandwidthLimiter>>,
    multi_progress: indicatif::MultiProgress,
    state: Arc<DownloadState>,
}

impl FileDownloader {
    fn new(config: DownloadConfig, multi_progress: indicatif::MultiProgress, limiter: Option<Arc<BandwidthLimiter>>, state: Arc<DownloadState>) -> Self {
        let mut builder = Client::builder()
            .user_agent(&config.user_agent)
            .connect_timeout(config.timeout);

        if config.force_ipv4 {
            builder = builder.local_address(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
        } else if config.force_ipv6 {
            builder = builder.local_address(IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED));
        }

        let client = builder.build()
            .expect("Failed to create HTTP client");

        Self { 
            client, 
            config: Arc::new(config), 
            limiter,
            multi_progress,
            state,
        }
    }

    async fn download(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let url = &self.config.url;
        let output_path = &self.config.output_path;
        let filename = Path::new(output_path).file_name().and_then(|n| n.to_str()).unwrap_or("file");

        let response = self.client.head(url).send().await?;
        let total_size = response
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|ct_len| ct_len.to_str().ok())
            .and_then(|ct_len| ct_len.parse().ok())
            .unwrap_or(0);

        if total_size > 0 {
            self.state.total_pb.inc_length(total_size);
        }

        let pb = self.multi_progress.insert(0, ProgressBar::new(total_size));
        pb.set_style(
            ProgressStyle::default_bar()
                .template(&format!(" {{prefix:<16}} {{bytes:>10}}/{{total_bytes:<10}} {{bytes_per_sec:>12}} {{eta:>6}} [{{wide_bar}}] {{percent:>3}}%"))
                .unwrap()
                .progress_chars("---c  o "),
        );
        pb.set_prefix(filename.to_string());

        if total_size == 0 {
            return self.download_single_threaded(0, pb).await;
        }

        let supports_range = response
            .headers()
            .get(reqwest::header::ACCEPT_RANGES)
            .map(|h| h == "bytes")
            .unwrap_or(false);

        let mut already_downloaded = 0u64;
        let file_exists = Path::new(output_path).exists();

        if self.config.resume && file_exists {
            if let Ok(meta) = metadata(output_path).await {
                already_downloaded = meta.len();
                if already_downloaded >= total_size {
                    pb.finish_with_message("Completed");
                    return Ok(())
                }
                pb.set_position(already_downloaded);
                self.state.total_pb.inc(already_downloaded);
            }
        } else if file_exists {
            File::create(output_path).await?;
        }

        let res = if supports_range && !self.config.resume && total_size > self.config.chunk_size {
            self.download_multi_threaded(total_size, pb).await
        } else {
            self.download_single_threaded(already_downloaded, pb).await
        };

        let finished = self.state.finished_files.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        self.state.total_pb.set_message(format!("({}/{})", finished, self.state.total_files));
        res
    }

    async fn download_single_threaded(&self, start_pos: u64, pb: ProgressBar) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut headers = HeaderMap::new();
        if start_pos > 0 {
            headers.insert(RANGE, format!("bytes={}-", start_pos).parse().unwrap());
        }

        let mut response = tokio::time::timeout(
            self.config.timeout,
            self.client.get(&self.config.url).headers(headers).send()
        ).await??;

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
            self.state.total_pb.inc(chunk.len() as u64);
            if let Some(ref limiter) = self.limiter {
                limiter.throttle(chunk.len() as u64).await;
            }
        }

        pb.finish();
        Ok(())
    }

    async fn download_multi_threaded(&self, total_size: u64, pb: ProgressBar) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let num_chunks = std::cmp::min(
            self.config.concurrent_chunks,
            (total_size / self.config.chunk_size + 1) as usize,
        );

        let semaphore = Arc::new(Semaphore::new(num_chunks));
        let pb = Arc::new(pb);
        let mut handles = Vec::new();

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
            let total_pb = self.state.total_pb.clone();
            let handle = tokio::spawn(async move {
                let _permit = semaphore_clone.acquire().await.unwrap();
                download_chunk(client, url, output_path, start, end, pb_clone, timeout, limiter, total_pb).await
            });

            handles.push(handle);
        }

        for handle in handles {
            handle.await??;
        }

        pb.finish();
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
    total_pb: ProgressBar,
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
        total_pb.inc(chunk.len() as u64);
        if let Some(ref lim) = limiter {
            lim.throttle(chunk.len() as u64).await;
        }
    }

    Ok(())
}


#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut args = Args::parse();
    
    // Read from stdin if no URLs provided
    if args.urls.is_empty() {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            use tokio::io::AsyncBufReadExt;
            let stdin = tokio::io::stdin();
            let mut reader = tokio::io::BufReader::new(stdin).lines();
            while let Some(line) = reader.next_line().await? {
                let line = line.trim();
                if !line.is_empty() {
                    args.urls.push(line.to_string());
                }
            }
        }
    }

    if args.urls.is_empty() {
        use clap::CommandFactory;
        Args::command().print_help()?;
        println!();
        return Ok(());
    }

    let multi_progress = indicatif::MultiProgress::new();
    let semaphore = Arc::new(Semaphore::new(args.parallel_downloads));
    let limiter = args.limit_rate.map(|limit| Arc::new(BandwidthLimiter::new(limit)));
    
    // Total progress bar
    let total_pb = multi_progress.add(ProgressBar::new(0));
    total_pb.set_style(
        ProgressStyle::default_bar()
            .template("Total {msg} {bytes:>10}/{total_bytes:<10} {bytes_per_sec:>12} {eta:>6} [ {wide_bar} ] {percent}%")
            .unwrap()
            .progress_chars("---c  o "),
    );
    total_pb.set_message(format!("(0/{})", args.urls.len()));

    let state = Arc::new(DownloadState {
        total_files: args.urls.len(),
        finished_files: std::sync::atomic::AtomicUsize::new(0),
        total_pb: total_pb.clone(),
    });

    let mut handles = Vec::new();

    for url in args.urls {
        let output_path = if args.output.is_some() && handles.is_empty() {
            args.output.clone().unwrap()
        } else {
            url.split('/')
                .last()
                .filter(|s| !s.is_empty())
                .unwrap_or("index.html")
                .to_string()
        };

        let config = DownloadConfig {
            url,
            output_path,
            concurrent_chunks: args.threads,
            chunk_size: args.chunk_size,
            resume: args.resume,
            user_agent: args.user_agent.clone(),
            timeout: args.timeout,
            force_ipv4: args.inet4_only,
            force_ipv6: args.inet6_only,
        };

        let downloader = Arc::new(FileDownloader::new(config, multi_progress.clone(), limiter.clone(), state.clone()));
        let sem = semaphore.clone();

        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            downloader.download().await
        });
        handles.push(handle);
    }

    for handle in handles {
        let _ = handle.await?;
    }

    total_pb.finish();

    Ok(())
}