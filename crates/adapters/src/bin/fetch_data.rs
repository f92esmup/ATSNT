use std::path::PathBuf;

use adapters::{BinanceDataFetcher, FetchParams};
use anyhow::{bail, Result};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "fetch_data",
    about = "Automated Binance historical market data ETL downloader & Parquet converter"
)]
struct Args {
    /// Trading pair symbol (e.g. BTCUSDT, ETHUSDT)
    #[arg(short, long, default_value = "BTCUSDT")]
    symbol: String,

    /// Year to fetch (e.g. 2024)
    #[arg(short, long, default_value_t = 2024)]
    year: u32,

    /// Month to fetch (1-12)
    #[arg(short, long, default_value_t = 1)]
    month: u32,

    /// Optional specific day to fetch (1-31). If omitted, downloads the full monthly archive.
    #[arg(short, long)]
    day: Option<u32>,

    /// Destination root directory for historical Parquet storage
    #[arg(short, long, default_value = "data/historical")]
    output_dir: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();

    if !(1..=12).contains(&args.month) {
        bail!("Month must be between 1 and 12, got {}", args.month);
    }

    if let Some(day) = args.day {
        if !(1..=31).contains(&day) {
            bail!("Day must be between 1 and 31, got {}", day);
        }
    }

    let params = FetchParams {
        symbol: args.symbol.to_uppercase(),
        year: args.year,
        month: args.month,
        day: args.day,
        output_dir: args.output_dir,
    };

    println!("============================================================");
    println!(" ATSNT - Binance Historical ETL & Parquet Converter");
    println!("============================================================");
    println!("Symbol:     {}", params.symbol);
    if let Some(day) = params.day {
        println!(
            "Period:     {:04}-{:02}-{:02} (Daily)",
            params.year, params.month, day
        );
    } else {
        println!(
            "Period:     {:04}-{:02} (Full Month)",
            params.year, params.month
        );
    }
    println!("Target:     {}", params.target_parquet_path().display());
    println!("------------------------------------------------------------");

    let fetcher = BinanceDataFetcher::new();
    let summary = fetcher.fetch_and_convert(&params)?;

    let size_mb = summary.parquet_size_bytes as f64 / (1024.0 * 1024.0);
    let throughput = if summary.duration_secs > 0.0 {
        summary.total_trades as f64 / summary.duration_secs
    } else {
        0.0
    };

    println!("------------------------------------------------------------");
    println!(" [SUCCESS] Conversion complete!");
    println!(" Total Trades Converted: {}", summary.total_trades);
    println!(
        " Execution Time:         {:.2}s ({:.0} trades/sec)",
        summary.duration_secs, throughput
    );
    println!(" Parquet File Size:      {:.2} MB", size_mb);
    println!(" Parquet File Path:      {}", summary.output_path.display());
    println!("============================================================");

    Ok(())
}
