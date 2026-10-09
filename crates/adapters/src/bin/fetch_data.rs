use std::path::PathBuf;

use adapters::{BinanceDataFetcher, FetchParams};
use anyhow::{bail, Result};
use chrono::{Datelike, Utc};
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

    /// Year to fetch (e.g. 2024, 2026)
    #[arg(short, long, default_value_t = 2024)]
    year: u32,

    /// Optional month to fetch (1-12). If omitted, downloads and converts the entire year up to today.
    #[arg(short, long)]
    month: Option<u32>,

    /// Optional specific day to fetch (1-31). Requires --month to be specified.
    #[arg(short, long)]
    day: Option<u32>,

    /// Destination root directory for historical Parquet storage
    #[arg(short, long, default_value = "data/historical")]
    output_dir: PathBuf,

    /// Force re-download and re-conversion even if the Parquet file already exists
    #[arg(short, long, default_value_t = false)]
    force: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let now = Utc::now();
    let current_year = now.year() as u32;
    let current_month = now.month();
    let current_day = now.day();

    if args.year > current_year {
        bail!(
            "Cannot fetch future year {} (current year is {})",
            args.year,
            current_year
        );
    }

    if let Some(m) = args.month {
        if !(1..=12).contains(&m) {
            bail!("Month must be between 1 and 12, got {}", m);
        }
        if args.year == current_year && m > current_month {
            bail!("Month {:02} is in the future for year {}", m, current_year);
        }
    }

    if let Some(d) = args.day {
        if args.month.is_none() {
            bail!("--day requires --month to be specified");
        }
        if !(1..=31).contains(&d) {
            bail!("Day must be between 1 and 31, got {}", d);
        }
    }

    println!("============================================================");
    println!(" ATSNT - Binance Historical ETL & Parquet Converter");
    println!("============================================================");
    println!("Symbol:     {}", args.symbol.to_uppercase());
    println!("Year:       {}", args.year);
    if let Some(day) = args.day {
        println!(
            "Period:     {:04}-{:02}-{:02} (Daily)",
            args.year,
            args.month.unwrap_or(1),
            day
        );
    } else if let Some(m) = args.month {
        if args.year == current_year && m == current_month {
            println!(
                "Period:     {:04}-{:02} (Current Month YTD up to Day {:02})",
                args.year, m, current_day
            );
        } else {
            println!("Period:     {:04}-{:02} (Single Month)", args.year, m);
        }
    } else if args.year == current_year {
        println!(
            "Period:     Year-to-Date {:04} (Months 01..{:02} + Daily up to Day {:02})",
            args.year,
            current_month.saturating_sub(1),
            current_day
        );
    } else {
        println!("Period:     Full Year {:04} (12 Months)", args.year);
    }
    println!(
        "Target Dir: {}",
        args.output_dir.join(&args.symbol).display()
    );
    println!("------------------------------------------------------------");

    let fetcher = BinanceDataFetcher::new();
    let mut total_trades_all = 0usize;
    let mut total_bytes_all = 0u64;
    let start_all = std::time::Instant::now();

    // Work items to process: (month, Option<day>)
    let mut tasks: Vec<(u32, Option<u32>)> = Vec::new();

    if let Some(day) = args.day {
        tasks.push((args.month.unwrap(), Some(day)));
    } else if let Some(m) = args.month {
        if args.year == current_year && m == current_month {
            // Current month: download daily files up to current day
            for d in 1..=current_day {
                tasks.push((m, Some(d)));
            }
        } else {
            tasks.push((m, None));
        }
    } else if args.year == current_year {
        // Complete months YTD
        for m in 1..current_month {
            tasks.push((m, None));
        }
        // Current month: daily files up to current day
        for d in 1..=current_day {
            tasks.push((current_month, Some(d)));
        }
    } else {
        // Historical full year
        for m in 1..=12 {
            tasks.push((m, None));
        }
    }

    for (month, day) in tasks {
        let params = FetchParams {
            symbol: args.symbol.to_uppercase(),
            year: args.year,
            month,
            day,
            output_dir: args.output_dir.clone(),
        };

        let target_path = params.target_parquet_path();

        if target_path.exists() && !args.force {
            let meta = std::fs::metadata(&target_path)?;
            let size_mb = meta.len() as f64 / (1024.0 * 1024.0);
            if let Some(d) = day {
                println!(
                    "[SKIP] Day {:04}-{:02}-{:02} already exists: {} ({:.2} MB)",
                    args.year,
                    month,
                    d,
                    target_path.display(),
                    size_mb
                );
            } else {
                println!(
                    "[SKIP] Month {:04}-{:02} already exists: {} ({:.2} MB)",
                    args.year,
                    month,
                    target_path.display(),
                    size_mb
                );
            }
            total_bytes_all += meta.len();
            continue;
        }

        if let Some(d) = day {
            println!(
                "\n[*] Processing Day {:04}-{:02}-{:02}...",
                args.year, month, d
            );
        } else {
            println!("\n[*] Processing Month {:04}-{:02}...", args.year, month);
        }

        match fetcher.fetch_and_convert(&params) {
            Ok(summary) => {
                let size_mb = summary.parquet_size_bytes as f64 / (1024.0 * 1024.0);
                if let Some(d) = day {
                    println!(
                        "    [OK] Day {:02}: Converted {} trades in {:.2}s ({:.2} MB)",
                        d, summary.total_trades, summary.duration_secs, size_mb
                    );
                } else {
                    println!(
                        "    [OK] Month {:02}: Converted {} trades in {:.2}s ({:.2} MB)",
                        month, summary.total_trades, summary.duration_secs, size_mb
                    );
                }
                total_trades_all += summary.total_trades;
                total_bytes_all += summary.parquet_size_bytes;
            }
            Err(e) => {
                let err_str = e.to_string();
                if let Some(d) = day {
                    if err_str.contains("HTTP 404") {
                        println!(
                            "    [INFO] Day {:02} not available on Binance Vision (in progress or not yet archived).",
                            d
                        );
                        break;
                    }
                }
                return Err(e.into());
            }
        }
    }

    let elapsed = start_all.elapsed().as_secs_f64();
    let total_mb = total_bytes_all as f64 / (1024.0 * 1024.0);

    println!("\n============================================================");
    println!(" [SUCCESS] All datasets processed!");
    if total_trades_all > 0 {
        println!(" New Trades Converted:  {}", total_trades_all);
    }
    println!(" Total Elapsed Time:    {:.2}s", elapsed);
    println!(" Total Dataset Size:    {:.2} MB", total_mb);
    println!(
        " Parquet Directory:     {}",
        args.output_dir.join(&args.symbol).display()
    );
    println!("============================================================");

    Ok(())
}
