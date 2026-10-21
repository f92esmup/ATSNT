//! Calibrates the tick, volume, and range thresholds of the swarm's feeds (strategy spec §5.4).
//!
//! It reads Binance aggTrades (`.parquet` file or directory, or `.csv`), keeps the trades of the
//! UTC period `[--from, --to)`, and writes a swarm `SharedConfig` ([`SwarmConfig`], spec §14.1)
//! whose thresholds and `calibration_end_ms` come from [`domain::calibrate`]. The evaluated data
//! of a later run must start strictly after `calibration_end_ms` (no look-ahead).

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use adapters::{BinanceCsvReader, BinanceParquetReader, MarketDataStream};
use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use clap::Parser;
use domain::{
    calibrate, calibrate_empirical_priors, BarAggregator, FeedThresholds, TimeBarAggregator, Trade,
};
use strategies::swarm_consensus::ensemble::TIME_HORIZONS_MS;
use strategies::{SwarmConfig, SwarmPriorsConfig};

#[derive(Parser, Debug)]
#[command(
    name = "calibrate_feeds",
    about = "ATSNT - Calibrates the swarm's tick, volume, and range feed thresholds on aggTrades"
)]
struct Args {
    /// Historical aggTrades: a .parquet file, a directory of .parquet files, or a .csv file
    #[arg(long)]
    data: PathBuf,

    /// First UTC day of the calibration period, inclusive (YYYY-MM-DD)
    #[arg(long)]
    from: String,

    /// UTC day that ends the calibration period, exclusive (YYYY-MM-DD)
    #[arg(long)]
    to: String,

    /// Output path of the calibrated swarm SharedConfig (JSON)
    #[arg(short, long)]
    output: PathBuf,

    /// Optional swarm SharedConfig (JSON) to start from; defaults to the built-in defaults
    #[arg(long)]
    base: Option<PathBuf>,

    /// Optional flag to calibrate empirical gene priors from the calibration trades
    #[arg(long, default_value_t = false)]
    calibrate_priors: bool,
}

/// The calibration period `[start_ms, end_ms)` in Unix milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CalibrationWindow {
    start_ms: i64,
    end_ms: i64,
}

impl CalibrationWindow {
    /// Builds the window from two `YYYY-MM-DD` UTC days; `from` is inclusive, `to` exclusive.
    fn from_days(from: &str, to: &str) -> Result<Self> {
        let start_ms = parse_day_start_ms(from).with_context(|| "invalid --from")?;
        let end_ms = parse_day_start_ms(to).with_context(|| "invalid --to")?;
        if start_ms >= end_ms {
            bail!("--from ({from}) must be earlier than --to ({to})");
        }
        Ok(Self { start_ms, end_ms })
    }

    /// Whether a trade timestamp falls inside the window.
    fn contains(&self, timestamp_ms: i64) -> bool {
        self.start_ms <= timestamp_ms && timestamp_ms < self.end_ms
    }
}

/// Unix milliseconds of `00:00:00.000 UTC` of a `YYYY-MM-DD` day.
fn parse_day_start_ms(day: &str) -> Result<i64> {
    let date = NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .with_context(|| format!("expected a YYYY-MM-DD day, got '{day}'"))?;
    let midnight = date
        .and_hms_opt(0, 0, 0)
        .with_context(|| format!("no midnight for '{day}'"))?;
    Ok(midnight.and_utc().timestamp_millis())
}

/// Keeps the trades of `stream` that fall inside `window`, in stream order.
fn collect_in_window<S>(stream: &mut S, window: CalibrationWindow) -> Result<(Vec<Trade>, usize)>
where
    S: MarketDataStream + ?Sized,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    let mut kept = Vec::new();
    let mut read = 0_usize;
    while let Some(trade) = stream.next_trade()? {
        read += 1;
        if window.contains(trade.timestamp) {
            kept.push(trade);
        }
    }
    Ok((kept, read))
}

/// Calibrates the thresholds on `trades` and applies them to `base`.
fn build_config(
    base: SwarmConfig,
    trades: &[Trade],
    calibrate_priors: bool,
) -> Result<(SwarmConfig, FeedThresholds)> {
    if trades.is_empty() {
        bail!("no trades in the calibration period");
    }
    let thresholds = calibrate(trades, TIME_HORIZONS_MS)
        .map_err(|e| anyhow::anyhow!("calibration failed: {e}"))?;
    let mut config = base.with_thresholds(&thresholds);

    if calibrate_priors {
        let mut aggregator = TimeBarAggregator::new(3_600_000)
            .map_err(|e| anyhow::anyhow!("failed to create 1h bar aggregator: {e}"))?;
        let mut bars = Vec::new();
        for trade in trades {
            if let Some(bar) = aggregator.process_trade(trade) {
                bars.push(bar);
            }
        }
        if bars.len() >= 64 {
            let empirical = calibrate_empirical_priors(&bars)
                .map_err(|e| anyhow::anyhow!("empirical prior calibration failed: {e}"))?;
            let priors = SwarmPriorsConfig::from_empirical(&empirical);
            config = config.with_priors(priors);
        } else {
            eprintln!(
                "warning: only {} 1h bars generated (need 64); keeping base priors",
                bars.len()
            );
        }
    }

    config
        .validate()
        .map_err(|e| anyhow::anyhow!("calibrated configuration is invalid: {e}"))?;
    Ok((config, thresholds))
}

/// Opens `data` with the same reader selection as `run_hpo`: directories and `.parquet` files
/// use the Parquet reader, anything else the CSV reader.
fn open_stream(data: &Path) -> Result<Box<dyn MarketDataStream<Error = adapters::AdapterError>>> {
    if data.is_dir() || data.extension().and_then(|s| s.to_str()) == Some("parquet") {
        Ok(Box::new(BinanceParquetReader::open(data).with_context(
            || format!("Failed to open {}", data.display()),
        )?))
    } else {
        let file =
            File::open(data).with_context(|| format!("Failed to open {}", data.display()))?;
        Ok(Box::new(BinanceCsvReader::new(BufReader::new(file))))
    }
}

/// Loads the base configuration, or the defaults when no path is given.
fn load_base(path: Option<&Path>) -> Result<SwarmConfig> {
    match path {
        None => Ok(SwarmConfig::default()),
        Some(path) => {
            let raw = std::fs::read_to_string(path)
                .with_context(|| format!("Failed to read {}", path.display()))?;
            serde_json::from_str(&raw)
                .with_context(|| format!("{} is not a swarm SharedConfig", path.display()))
        }
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let window = CalibrationWindow::from_days(&args.from, &args.to)?;
    let base = load_base(args.base.as_deref())?;

    let mut stream = open_stream(&args.data)?;
    let (trades, read) = collect_in_window(stream.as_mut(), window)?;
    if trades.is_empty() {
        bail!(
            "no trades in the calibration period [{}, {}) of {} ({read} trades read)",
            args.from,
            args.to,
            args.data.display()
        );
    }
    let (config, thresholds) = build_config(base, &trades, args.calibrate_priors)?;

    let json = serde_json::to_string_pretty(&config)?;
    std::fs::write(&args.output, json + "\n")
        .with_context(|| format!("Failed to write {}", args.output.display()))?;

    println!("Calibration period: [{}, {}) UTC", args.from, args.to);
    println!("Trades:             {} kept of {read} read", trades.len());
    println!("Horizons (ms):      {TIME_HORIZONS_MS:?}");
    println!("Tick thresholds:    {:?}", thresholds.tick);
    println!("Volume thresholds:  {:?}", thresholds.volume);
    println!("Range thresholds:   {:?}", thresholds.range);
    println!("calibration_end_ms: {}", thresholds.calibration_end_ms);
    println!("Written:            {}", args.output.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::Side;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;

    /// 2024-01-01T00:00:00Z and 2024-01-02T00:00:00Z in Unix ms.
    const DAY_1: i64 = 1_704_067_200_000;
    const DAY_2: i64 = 1_704_153_600_000;

    struct VecStream(std::vec::IntoIter<Trade>);

    impl MarketDataStream for VecStream {
        type Error = std::io::Error;
        fn next_trade(&mut self) -> Result<Option<Trade>, Self::Error> {
            Ok(self.0.next())
        }
    }

    fn trade(timestamp: i64, i: i64) -> Trade {
        let price = dec!(40000) + Decimal::from(i % 7) - Decimal::from(i % 5);
        Trade::new(timestamp, price, dec!(0.5), Side::Buy).unwrap()
    }

    #[test]
    fn a_day_parses_to_utc_midnight() {
        assert_eq!(parse_day_start_ms("2024-01-01").unwrap(), DAY_1);
        assert_eq!(parse_day_start_ms("2024-01-02").unwrap(), DAY_2);
    }

    #[test]
    fn a_malformed_day_is_rejected() {
        assert!(parse_day_start_ms("2024-13-01").is_err());
        assert!(parse_day_start_ms("01/01/2024").is_err());
    }

    #[test]
    fn from_is_inclusive_and_to_is_exclusive() {
        let window = CalibrationWindow::from_days("2024-01-01", "2024-01-02").unwrap();
        assert!(!window.contains(DAY_1 - 1));
        assert!(window.contains(DAY_1));
        assert!(window.contains(DAY_2 - 1));
        assert!(!window.contains(DAY_2));
    }

    #[test]
    fn from_not_before_to_is_refused() {
        let same = CalibrationWindow::from_days("2024-01-02", "2024-01-02").unwrap_err();
        assert!(same.to_string().contains("must be earlier than --to"));
        assert!(CalibrationWindow::from_days("2024-01-03", "2024-01-02").is_err());
    }

    #[test]
    fn only_trades_inside_the_window_are_kept() {
        let window = CalibrationWindow::from_days("2024-01-01", "2024-01-02").unwrap();
        let stamps = [DAY_1 - 1, DAY_1, DAY_1 + 10, DAY_2 - 1, DAY_2, DAY_2 + 5];
        let trades: Vec<Trade> = stamps
            .iter()
            .enumerate()
            .map(|(i, ts)| trade(*ts, i as i64))
            .collect();
        let mut stream = VecStream(trades.into_iter());
        let (kept, read) = collect_in_window(&mut stream, window).unwrap();
        assert_eq!(read, 6);
        let kept_stamps: Vec<i64> = kept.iter().map(|t| t.timestamp).collect();
        assert_eq!(kept_stamps, vec![DAY_1, DAY_1 + 10, DAY_2 - 1]);
    }

    #[test]
    fn the_config_carries_the_thresholds_and_round_trips() {
        let window = CalibrationWindow::from_days("2024-01-01", "2024-01-02").unwrap();
        // One trade per minute over two days; the last one inside the window is at DAY_2 - 60 s.
        let trades: Vec<Trade> = (0..2 * 1_440)
            .map(|i| trade(DAY_1 + i * 60_000, i))
            .collect();
        let mut stream = VecStream(trades.into_iter());
        let (kept, _) = collect_in_window(&mut stream, window).unwrap();

        let base = SwarmConfig {
            genome_seed: 7,
            allow_short: false,
            ..SwarmConfig::default()
        };
        let (config, thresholds) = build_config(base, &kept, false).unwrap();

        assert_eq!(thresholds.calibration_end_ms, DAY_2 - 60_000);
        assert_eq!(config.calibration_end_ms, DAY_2 - 60_000);
        assert_eq!(config.tick_thresholds, thresholds.tick);
        assert_eq!(config.volume_thresholds, thresholds.volume);
        assert_eq!(config.range_thresholds, thresholds.range);
        // The base's structural fields are kept.
        assert_eq!(config.genome_seed, 7);
        assert!(!config.allow_short);

        let json = serde_json::to_string_pretty(&config).unwrap();
        let parsed: SwarmConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, config);
    }

    #[test]
    fn an_empty_period_is_refused() {
        let err = build_config(SwarmConfig::default(), &[], false).unwrap_err();
        assert!(err
            .to_string()
            .contains("no trades in the calibration period"));
    }

    #[test]
    fn no_base_gives_the_defaults() {
        assert_eq!(load_base(None).unwrap(), SwarmConfig::default());
    }
}
