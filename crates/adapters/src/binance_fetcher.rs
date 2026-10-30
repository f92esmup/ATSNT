use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use indicatif::{ProgressBar, ProgressStyle};
use reqwest::blocking::Client;
use zip::ZipArchive;

use crate::error::AdapterError;
use crate::storage::tape_parquet::TapeWriter;

#[cfg(test)]
mod checksum_tests;

/// Origin of the public Binance Vision archives.
const BINANCE_VISION_BASE_URL: &str = "https://data.binance.vision";

/// Parameters defining which Binance historical dataset to fetch.
#[derive(Debug, Clone)]
pub struct FetchParams {
    pub symbol: String,
    pub year: u32,
    pub month: u32,
    pub day: Option<u32>,
    pub output_dir: PathBuf,
}

impl FetchParams {
    /// Path of the archive on the Binance Vision host, without the origin.
    pub fn archive_path(&self) -> String {
        let sym = self.symbol.to_uppercase();
        if let Some(day) = self.day {
            format!(
                "/data/futures/um/daily/aggTrades/{sym}/{sym}-aggTrades-{:04}-{:02}-{:02}.zip",
                self.year, self.month, day
            )
        } else {
            format!(
                "/data/futures/um/monthly/aggTrades/{sym}/{sym}-aggTrades-{:04}-{:02}.zip",
                self.year, self.month
            )
        }
    }

    /// Constructs the official Binance Vision public download URL.
    pub fn download_url(&self) -> String {
        format!("{BINANCE_VISION_BASE_URL}{}", self.archive_path())
    }

    /// Computes the target Parquet file path on disk.
    pub fn target_parquet_path(&self) -> PathBuf {
        let sym = self.symbol.to_uppercase();
        let file_name = if let Some(day) = self.day {
            format!(
                "{sym}-aggTrades-{:04}-{:02}-{:02}.parquet",
                self.year, self.month, day
            )
        } else {
            format!("{sym}-aggTrades-{:04}-{:02}.parquet", self.year, self.month)
        };
        self.output_dir.join(&sym).join(file_name)
    }
}

/// Summary metrics returned upon successful download and conversion.
#[derive(Debug, Clone)]
pub struct FetchSummary {
    pub symbol: String,
    pub url: String,
    pub output_path: PathBuf,
    pub total_trades: usize,
    pub duration_secs: f64,
    pub parquet_size_bytes: u64,
}

/// Feeds everything written to it into a running SHA-256 digest.
struct DigestWriter(ring::digest::Context);

impl Write for DigestWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Checks `archive` against the SHA-256 digest in a Binance `.CHECKSUM` file
/// (`<hex digest>  <file name>`). Only the first token is read.
pub(crate) fn verify_archive_sha256(
    archive: &Path,
    checksum_file: &str,
) -> Result<(), AdapterError> {
    let expected = checksum_file
        .split_whitespace()
        .next()
        .filter(|digest| digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| {
            AdapterError::ChecksumUnreadable(format!("no SHA-256 digest for {}", archive.display()))
        })?;

    let mut file = File::open(archive)?;
    let mut digest = DigestWriter(ring::digest::Context::new(&ring::digest::SHA256));
    // `io::copy` retries reads interrupted by a signal, so a valid archive never fails on EINTR.
    std::io::copy(&mut file, &mut digest)?;
    let actual: String = digest
        .0
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();

    if actual == expected {
        Ok(())
    } else {
        Err(AdapterError::ChecksumMismatch {
            file: archive.display().to_string(),
            expected,
            actual,
        })
    }
}

/// Automated Binance historical market data ETL downloader and Parquet converter.
pub struct BinanceDataFetcher {
    client: Client,
    show_progress: bool,
    base_url: String,
}

impl Default for BinanceDataFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl BinanceDataFetcher {
    /// Creates a new fetcher instance with standard HTTP client configuration.
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .unwrap_or_else(|_| Client::new());
        Self {
            client,
            show_progress: true,
            base_url: BINANCE_VISION_BASE_URL.to_string(),
        }
    }

    /// Enables or disables terminal progress bars (disable when fetching concurrently).
    pub fn with_progress(mut self, show_progress: bool) -> Self {
        self.show_progress = show_progress;
        self
    }

    /// Points the fetcher at another origin serving the same layout (local test servers).
    pub fn with_base_url(mut self, base_url: String) -> Self {
        self.base_url = base_url;
        self
    }

    fn progress_bar(&self, len: Option<u64>) -> ProgressBar {
        match (self.show_progress, len) {
            (false, _) => ProgressBar::hidden(),
            (true, Some(len)) => ProgressBar::new(len),
            (true, None) => ProgressBar::new_spinner(),
        }
    }

    /// Downloads the official ZIP archive and streams it directly into a compressed Parquet file.
    pub fn fetch_and_convert(&self, params: &FetchParams) -> Result<FetchSummary, AdapterError> {
        let start_time = Instant::now();
        let url = format!("{}{}", self.base_url, params.archive_path());
        let target_parquet = params.target_parquet_path();

        if let Some(parent) = target_parquet.parent() {
            fs::create_dir_all(parent)?;
        }

        println!("Connecting to: {url}");

        // Step 0: the published digest comes first, so an archive is never converted unchecked
        let checksum_url = format!("{url}.CHECKSUM");
        let checksum_response = self.client.get(&checksum_url).send()?;
        if !checksum_response.status().is_success() {
            return Err(AdapterError::General(format!(
                "Failed to download checksum from {checksum_url} (HTTP {})",
                checksum_response.status()
            )));
        }
        let checksum = checksum_response.text()?;

        // Step 1: Stream download ZIP to a temporary file
        let mut response = self.client.get(&url).send()?;
        if !response.status().is_success() {
            return Err(AdapterError::General(format!(
                "Failed to download archive from {url} (HTTP {})",
                response.status()
            )));
        }

        let total_size = response.content_length().unwrap_or(0);
        let download_pb = self.progress_bar(Some(total_size));
        download_pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta}) {msg}")
                .map_err(|e| AdapterError::General(e.to_string()))?
                .progress_chars("#>-"),
        );
        download_pb.set_message("Downloading ZIP");

        let temp_zip_file = tempfile::NamedTempFile::new()?;
        let mut temp_zip_writer = File::create(temp_zip_file.path())?;

        let mut buffer = [0u8; 65536];
        loop {
            let bytes_read = std::io::Read::read(&mut response, &mut buffer)?;
            if bytes_read == 0 {
                break;
            }
            temp_zip_writer.write_all(&buffer[..bytes_read])?;
            download_pb.inc(bytes_read as u64);
        }
        temp_zip_writer.flush()?;
        download_pb.finish_with_message("Download complete");

        // Step 2: a corrupt or truncated archive stops here; the temporary file is removed on drop
        verify_archive_sha256(temp_zip_file.path(), &checksum)?;

        // Step 3: Open ZIP archive and stream CSV into Parquet
        let zip_reader = File::open(temp_zip_file.path())?;
        let mut archive = ZipArchive::new(zip_reader)?;

        // Find CSV file in ZIP
        let csv_index = (0..archive.len())
            .find(|&i| {
                archive
                    .by_index(i)
                    .map(|f| f.name().ends_with(".csv"))
                    .unwrap_or(false)
            })
            .ok_or_else(|| {
                AdapterError::General("No CSV file found inside the downloaded ZIP".to_string())
            })?;

        let csv_file = archive.by_index(csv_index)?;
        let mut buf_reader = BufReader::with_capacity(1024 * 1024, csv_file);

        let convert_pb = self.progress_bar(None);
        convert_pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} [{elapsed_precise}] Converting to Parquet: {pos} trades parsed")
                .map_err(|e| AdapterError::General(e.to_string()))?,
        );

        let mut parquet_writer = TapeWriter::new(&target_parquet)?;
        let mut line = String::with_capacity(256);
        let mut line_number = 0usize;

        loop {
            line.clear();
            let bytes_read = buf_reader.read_line(&mut line)?;
            if bytes_read == 0 {
                break;
            }
            line_number += 1;

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            // Detect and skip header row
            if trimmed.starts_with("agg_trade_id") || trimmed.starts_with("id") {
                continue;
            }

            let parts: Vec<&str> = trimmed.split(',').collect();
            if parts.len() < 7 {
                continue;
            }

            // Price: parts[1], Quantity: parts[2], Timestamp: parts[5], is_buyer_maker: parts[6]
            let price = parts[1].trim();
            let quantity = parts[2].trim();
            let timestamp: i64 =
                parts[5]
                    .trim()
                    .parse()
                    .map_err(|_| AdapterError::TimestampParseError {
                        line: line_number,
                        raw: parts[5].to_string(),
                    })?;

            let is_buyer_maker =
                matches!(parts[6].trim().to_lowercase().as_str(), "true" | "t" | "1");

            parquet_writer.write_raw(timestamp, price, quantity, is_buyer_maker)?;

            if line_number % 50_000 == 0 {
                convert_pb.set_position(line_number as u64);
            }
        }

        let total_trades = parquet_writer.finish()?;
        convert_pb.finish_with_message(format!("Converted {total_trades} trades to Parquet"));

        let file_meta = fs::metadata(&target_parquet)?;
        let elapsed = start_time.elapsed().as_secs_f64();

        Ok(FetchSummary {
            symbol: params.symbol.clone(),
            url,
            output_path: target_parquet,
            total_trades,
            duration_secs: elapsed,
            parquet_size_bytes: file_meta.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_monthly_url() {
        let params = FetchParams {
            symbol: "BTCUSDT".to_string(),
            year: 2024,
            month: 1,
            day: None,
            output_dir: PathBuf::from("data/historical"),
        };
        assert_eq!(
            params.download_url(),
            "https://data.binance.vision/data/futures/um/monthly/aggTrades/BTCUSDT/BTCUSDT-aggTrades-2024-01.zip"
        );
        assert_eq!(
            params.target_parquet_path(),
            PathBuf::from("data/historical/BTCUSDT/BTCUSDT-aggTrades-2024-01.parquet")
        );
    }

    #[test]
    fn build_daily_url() {
        let params = FetchParams {
            symbol: "ETHUSDT".to_string(),
            year: 2024,
            month: 3,
            day: Some(15),
            output_dir: PathBuf::from("data/historical"),
        };
        assert_eq!(
            params.download_url(),
            "https://data.binance.vision/data/futures/um/daily/aggTrades/ETHUSDT/ETHUSDT-aggTrades-2024-03-15.zip"
        );
        assert_eq!(
            params.target_parquet_path(),
            PathBuf::from("data/historical/ETHUSDT/ETHUSDT-aggTrades-2024-03-15.parquet")
        );
    }
}
