use std::path::PathBuf;

use adapters::{
    is_not_archived, plan_fetch_tasks, run_bounded, AdapterError, BinanceDataFetcher, FetchParams,
    FetchSummary, FetchTask,
};
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

    /// Maximum number of archives downloaded and converted concurrently
    #[arg(short, long, default_value_t = 4)]
    jobs: usize,
}

/// Result of processing one planned archive.
enum TaskOutcome {
    /// The Parquet file already existed and `--force` was not set.
    Skipped { path: PathBuf, bytes: u64 },
    /// The archive was downloaded and converted.
    Converted(FetchSummary),
}

fn task_label(year: u32, task: &FetchTask) -> String {
    match task.day {
        Some(d) => format!("Day {:04}-{:02}-{:02}", year, task.month, d),
        None => format!("Month {:04}-{:02}", year, task.month),
    }
}

/// Short label used by the per-archive result lines (`Day 01`, `Month 01`).
fn short_label(task: &FetchTask) -> String {
    match task.day {
        Some(d) => format!("Day {:02}", d),
        None => format!("Month {:02}", task.month),
    }
}

/// Prints the result line for one archive, keeping the sequential output format.
fn print_outcome(year: u32, task: &FetchTask, outcome: &Result<TaskOutcome, AdapterError>) {
    match outcome {
        Ok(TaskOutcome::Skipped { path, bytes }) => println!(
            "[SKIP] {} already exists: {} ({:.2} MB)",
            task_label(year, task),
            path.display(),
            *bytes as f64 / (1024.0 * 1024.0)
        ),
        Ok(TaskOutcome::Converted(summary)) => println!(
            "    [OK] {}: Converted {} trades in {:.2}s ({:.2} MB)",
            short_label(task),
            summary.total_trades,
            summary.duration_secs,
            summary.parquet_size_bytes as f64 / (1024.0 * 1024.0)
        ),
        Err(e) if task.day.is_some() && is_not_archived(e) => println!(
            "    [INFO] {} not available on Binance Vision (in progress or not yet archived).",
            short_label(task)
        ),
        Err(e) => println!("    [FAIL] {}: {e}", short_label(task)),
    }
}

fn fetch_one(
    fetcher: &BinanceDataFetcher,
    args: &Args,
    task: &FetchTask,
) -> Result<TaskOutcome, AdapterError> {
    let params = FetchParams {
        symbol: args.symbol.to_uppercase(),
        year: args.year,
        month: task.month,
        day: task.day,
        output_dir: args.output_dir.clone(),
    };
    let target_path = params.target_parquet_path();
    if target_path.exists() && !args.force {
        let bytes = std::fs::metadata(&target_path)?.len();
        return Ok(TaskOutcome::Skipped {
            path: target_path,
            bytes,
        });
    }
    println!("\n[*] Processing {}...", task_label(args.year, task));
    fetcher
        .fetch_and_convert(&params)
        .map(TaskOutcome::Converted)
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

    if args.jobs == 0 {
        bail!("--jobs must be at least 1");
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

    let fetcher = BinanceDataFetcher::new().with_progress(args.jobs == 1);
    let mut total_trades_all = 0usize;
    let mut total_bytes_all = 0u64;
    let start_all = std::time::Instant::now();

    let today = now.date_naive();
    let tasks = plan_fetch_tasks(args.year, args.month, args.day, today);
    println!(
        "Jobs:       {} archive(s), up to {} concurrent download(s)",
        tasks.len(),
        args.jobs
    );

    // Sequential runs print each result right after its "Processing" line; concurrent
    // runs print all results afterwards in task (month) order.
    let sequential = args.jobs == 1;
    let outcomes = run_bounded(&tasks, args.jobs, |task| {
        let outcome = fetch_one(&fetcher, &args, task);
        if sequential {
            print_outcome(args.year, task, &outcome);
        }
        outcome
    })?;

    let mut failures = Vec::new();
    for (task, outcome) in tasks.iter().zip(&outcomes) {
        if !sequential {
            print_outcome(args.year, task, outcome);
        }
        match outcome {
            Ok(TaskOutcome::Skipped { bytes, .. }) => total_bytes_all += bytes,
            Ok(TaskOutcome::Converted(summary)) => {
                total_trades_all += summary.total_trades;
                total_bytes_all += summary.parquet_size_bytes;
            }
            Err(e) if task.day.is_some() && is_not_archived(e) => {}
            Err(_) => failures.push(task_label(args.year, task)),
        }
    }

    if !failures.is_empty() {
        bail!(
            "{} archive(s) failed to download or convert: {}",
            failures.len(),
            failures.join(", ")
        );
    }

    let elapsed = start_all.elapsed().as_secs_f64();
    let total_mb = total_bytes_all as f64 / (1024.0 * 1024.0);

    println!("\n============================================================");
    println!(" [SUCCESS] All datasets processed!");
    if total_trades_all > 0 {
        println!(" New Trades Converted:  {}", total_trades_all);
    }
    println!(" Total Elapsed Time:    {:.2}s", elapsed);
    println!("[TIMING] phase=fetch seconds={:.2}", elapsed);
    println!(" Total Dataset Size:    {:.2} MB", total_mb);
    println!(
        " Parquet Directory:     {}",
        args.output_dir.join(&args.symbol).display()
    );
    println!("============================================================");

    Ok(())
}
