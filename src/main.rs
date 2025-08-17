use std::fs::{File, OpenOptions};
use std::io::{self, Write, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::header::{HeaderMap, RANGE};
use reqwest::Client;
use tokio::fs::metadata;
use tokio::sync::Semaphore;

#[derive(Debug)]
struct DownloadConfig {
    url: String,
    output_path: String,
    concurrent_chunks: usize,
    chunk_size: u64,
    resume: bool,
    user_agent: String,
    timeout: Duration,
}

#[derive(Debug)]
struct DownloadStats {
    total_size: u64,
    downloaded: u64,
    start_time: Instant,
}

impl DownloadStats {
    fn new(total_size: u64, already_downloaded: u64) -> Self {
        Self {
            total_size,
            downloaded: already_downloaded,
            start_time: Instant::now(),
        }
    }

    fn update(&mut self, bytes: u64) {
        self.downloaded += bytes;
    }

    fn speed(&self) -> f64 {
        let elapsed = self.start_time.elapsed().as_secs_f64();
        if elapsed > 0.0 {
            self.downloaded as f64 / elapsed
        } else {
            0.0
        }
    }

    fn eta(&self) -> Duration {
        let speed = self.speed();
        if speed > 0.0 {
            let remaining = self.total_size - self.downloaded;
            Duration::from_secs_f64(remaining as f64 / speed)
        } else {
            Duration::from_secs(0)
        }
    }
}

struct FileDownloader {
    client: Client,
    config: DownloadConfig,
}

impl FileDownloader {
    fn new(config: DownloadConfig) -> Self {
        let client = Client::builder()
            .user_agent(&config.user_agent)
            .timeout(config.timeout)
            .build()
            .expect("Failed to create HTTP client");

        Self { client, config }
    }

    async fn download(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        println!("🚀 Starting download from: {}", self.config.url);
        
        // Get file info
        let response = self.client.head(&self.config.url).send().await?;
        let total_size = response
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|ct_len| ct_len.to_str().ok())
            .and_then(|ct_len| ct_len.parse().ok())
            .unwrap_or(0);

        if total_size == 0 {
            return self.download_single_threaded().await;
        }

        let supports_range = response
            .headers()
            .get(reqwest::header::ACCEPT_RANGES)
            .map(|h| h == "bytes")
            .unwrap_or(false);

        println!("📊 File size: {} bytes", format_bytes(total_size));
        println!("🔄 Range requests supported: {}", supports_range);

        // Check if file exists and get current size
        let mut already_downloaded = 0u64;
        if self.config.resume && Path::new(&self.config.output_path).exists() {
            if let Ok(meta) = metadata(&self.config.output_path).await {
                already_downloaded = meta.len();
                if already_downloaded >= total_size {
                    println!("✅ File already fully downloaded!");
                    return Ok(());
                }
                println!("⏯️  Resuming download from {} bytes", already_downloaded);
            }
        }

        if supports_range && total_size > self.config.chunk_size {
            self.download_multi_threaded(total_size, already_downloaded).await
        } else {
            self.download_single_threaded().await
        }
    }

    async fn download_single_threaded(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut response = self.client.get(&self.config.url).send().await?;
        let total_size = response.content_length().unwrap_or(0);

        let pb = ProgressBar::new(total_size);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})")
                .unwrap()
                .progress_chars("#>-"),
        );

        let mut file = File::create(&self.config.output_path)?;
        let mut downloaded = 0u64;

        while let Some(chunk) = response.chunk().await? {
            file.write_all(&chunk)?;
            downloaded += chunk.len() as u64;
            pb.set_position(downloaded);
        }

        pb.finish_with_message("✅ Download completed!");
        println!("💾 File saved to: {}", self.config.output_path);
        Ok(())
    }

    async fn download_multi_threaded(
        &self,
        total_size: u64,
        already_downloaded: u64,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let remaining = total_size - already_downloaded;
        let num_chunks = std::cmp::min(
            self.config.concurrent_chunks,
            (remaining / self.config.chunk_size + 1) as usize,
        );

        println!("🧵 Using {} concurrent connections", num_chunks);

        let pb = ProgressBar::new(total_size);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})")
                .unwrap()
                .progress_chars("██▉▊▋▌▍▎▏  "),
        );
        pb.set_position(already_downloaded);

        let semaphore = Arc::new(Semaphore::new(num_chunks));
        let pb = Arc::new(pb);
        let mut handles = Vec::new();

        for i in 0..num_chunks {
            let start = already_downloaded + (i as u64 * remaining / num_chunks as u64);
            let end = if i == num_chunks - 1 {
                total_size - 1
            } else {
                already_downloaded + ((i + 1) as u64 * remaining / num_chunks as u64) - 1
            };

            let client = self.client.clone();
            let url = self.config.url.clone();
            let output_path = self.config.output_path.clone();
            let pb_clone = pb.clone();
            let semaphore_clone = semaphore.clone();

            let handle = tokio::spawn(async move {
                let _permit = semaphore_clone.acquire().await.unwrap();
                download_chunk(client, url, output_path, start, end, pb_clone).await
            });

            handles.push(handle);
        }

        // Wait for all chunks to complete
        for handle in handles {
            handle.await??;
        }

        pb.finish_with_message("✅ Download completed!");
        println!("💾 File saved to: {}", self.config.output_path);
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
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut headers = HeaderMap::new();
    headers.insert(RANGE, format!("bytes={}-{}", start, end).parse().unwrap());

    let mut response = client.get(&url).headers(headers).send().await?;
    
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .open(&output_path)?;
    
    file.seek(SeekFrom::Start(start))?;

    while let Some(chunk) = response.chunk().await? {
        file.write_all(&chunk)?;
        pb.inc(chunk.len() as u64);
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

fn get_user_input(prompt: &str) -> String {
    print!("{}", prompt);
    io::stdout().flush().unwrap();
    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    input.trim().to_string()
}

fn get_number_input<T>(prompt: &str, default: T) -> T
where
    T: std::str::FromStr + std::fmt::Display + Copy,
{
    loop {
        let input = get_user_input(&format!("{} (default: {}): ", prompt, default));
        if input.is_empty() {
            return default;
        }
        
        match input.parse::<T>() {
            Ok(value) => return value,
            Err(_) => {
                println!("❌ Invalid input. Please enter a valid number.");
                continue;
            }
        }
    }
}

fn get_yes_no_input(prompt: &str, default: bool) -> bool {
    loop {
        let default_str = if default { "Y/n" } else { "y/N" };
        let input = get_user_input(&format!("{} ({}): ", prompt, default_str));
        
        if input.is_empty() {
            return default;
        }
        
        match input.to_lowercase().as_str() {
            "y" | "yes" | "true" => return true,
            "n" | "no" | "false" => return false,
            _ => {
                println!("❌ Please enter 'y' for yes or 'n' for no.");
                continue;
            }
        }
    }
}

fn display_banner() {
    println!("╔══════════════════════════════════════════════╗");
    println!("║            🦀 Rust File Downloader           ║");
    println!("║                 Version 1.0.0                ║");
    println!("╚══════════════════════════════════════════════╝");
    println!();
}

fn display_config(config: &DownloadConfig) {
    println!("📋 Download Configuration:");
    println!("┌─────────────────────────────────────────────────────────────┐");
    println!("│ URL:              {:<40} │", truncate_string(&config.url, 40));
    println!("│ Output File:      {:<40} │", truncate_string(&config.output_path, 40));
    println!("│ Connections:      {:<40} │", config.concurrent_chunks);
    println!("│ Chunk Size:       {:<40} │", format_bytes(config.chunk_size));
    println!("│ Resume:           {:<40} │", if config.resume { "Yes" } else { "No" });
    println!("│ User Agent:       {:<40} │", truncate_string(&config.user_agent, 40));
    println!("│ Timeout:          {:<40} │", format!("{}s", config.timeout.as_secs()));
    println!("└─────────────────────────────────────────────────────────────┘");
    println!();
}

fn truncate_string(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len-3])
    }
}

fn create_interactive_config() -> DownloadConfig {
    println!("🔧 Let's configure your download!\n");
    
    // Get URL
    let url = loop {
        let input = get_user_input("🌐 Enter the download URL: ");
        if input.trim().is_empty() {
            println!("❌ URL cannot be empty. Please try again.");
            continue;
        }
        
        if !input.starts_with("http://") && !input.starts_with("https://") {
            println!("⚠️  Warning: URL should start with http:// or https://");
            if !get_yes_no_input("Continue anyway?", false) {
                continue;
            }
        }
        
        break input;
    };
    
    // Get output filename
    let default_filename = url.split('/').last().unwrap_or("downloaded_file").to_string();
    let output_path = loop {
        let input = get_user_input(&format!("💾 Output filename (default: {}): ", default_filename));
        if input.is_empty() {
            break default_filename.clone();
        }
        
        // Check if file already exists
        if Path::new(&input).exists() {
            println!("⚠️  File '{}' already exists.", input);
            if get_yes_no_input("Overwrite/resume this file?", true) {
                break input;
            }
            continue;
        }
        
        break input;
    };
    
    // Get number of concurrent connections
    let concurrent_chunks = get_number_input("🧵 Number of concurrent connections", 4usize);
    
    // Get chunk size
    println!("\n📦 Chunk size options:");
    println!("  1. 512 KB  (slow connection)");
    println!("  2. 1 MB    (recommended)");
    println!("  3. 2 MB    (fast connection)");
    println!("  4. 4 MB    (very fast connection)");
    println!("  5. Custom");
    
    let chunk_size = loop {
        let choice = get_number_input("Select chunk size", 2u32);
        match choice {
            1 => break 512 * 1024,      // 512 KB
            2 => break 1024 * 1024,     // 1 MB
            3 => break 2 * 1024 * 1024, // 2 MB
            4 => break 4 * 1024 * 1024, // 4 MB
            5 => {
                let custom = get_number_input("Enter chunk size in bytes", 1048576u64);
                break custom;
            }
            _ => {
                println!("❌ Please select a valid option (1-5).");
                continue;
            }
        }
    };
    
    // Resume option
    let resume = if Path::new(&output_path).exists() {
        get_yes_no_input("📂 Resume existing download?", true)
    } else {
        get_yes_no_input("📂 Enable resume capability?", true)
    };
    
    // User agent
    println!("\n🤖 User Agent options:");
    println!("  1. Default (RustDownloader/1.0)");
    println!("  2. Firefox");
    println!("  3. Chrome");
    println!("  4. Custom");
    
    let user_agent = loop {
        let choice = get_number_input("Select user agent", 1u32);
        match choice {
            1 => break "RustDownloader/1.0".to_string(),
            2 => break "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:91.0) Gecko/20100101 Firefox/91.0".to_string(),
            3 => break "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36".to_string(),
            4 => {
                let custom = get_user_input("Enter custom user agent: ");
                if custom.is_empty() {
                    println!("❌ User agent cannot be empty.");
                    continue;
                }
                break custom;
            }
            _ => {
                println!("❌ Please select a valid option (1-4).");
                continue;
            }
        }
    };
    
    // Timeout
    let timeout_secs = get_number_input("⏱️  Request timeout in seconds", 30u64);
    let timeout = Duration::from_secs(timeout_secs);
    
    DownloadConfig {
        url,
        output_path,
        concurrent_chunks,
        chunk_size,
        resume,
        user_agent,
        timeout,
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    display_banner();
    
    // Create configuration interactively
    let config = create_interactive_config();
    
    println!();
    display_config(&config);
    
    // Confirm before starting
    if !get_yes_no_input("🚀 Start download?", true) {
        println!("❌ Download cancelled.");
        return Ok(());
    }
    
    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("🚀 Starting download...");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    let downloader = FileDownloader::new(config);
    
    match downloader.download().await {
        Ok(()) => {
            println!("\n🎉 Download completed successfully!");
            println!("Press Enter to exit...");
            let _ = get_user_input("");
        }
        Err(e) => {
            println!("\n❌ Download failed: {}", e);
            println!("Press Enter to exit...");
            let _ = get_user_input("");
            return Err(e);
        }
    }

    Ok(())
}

// Cargo.toml dependencies needed:
/*
[dependencies]
reqwest = { version = "0.11", features = ["stream"] }
tokio = { version = "1.0", features = ["full"] }
indicatif = "0.17"
*/