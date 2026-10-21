//! Evaluates an expanded candidate swarm and prunes agents by Information Coefficient (IC)
//! / predictive score on their feed's bar-to-bar returns (strategy spec §7.1).
//!
//! Preserves stratified feed balance: out of `--candidates-per-feed` agents per feed, the top
//! `--keep-per-feed` are selected for each of the 16 feeds. The resulting elite population is
//! stored in `SwarmConfig.custom_agents` and saved to `--output`.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use adapters::{BinanceCsvReader, BinanceParquetReader, MarketDataStream};
use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use clap::Parser;
use domain::Trade;
use rust_decimal::Decimal;
use strategies::swarm_consensus::genome::FEED_COUNT;
use strategies::{Agent, SwarmConfig, SwarmEnsemble};

#[derive(Parser, Debug)]
#[command(
    name = "prune_swarm",
    about = "ATSNT - Prunes noisy swarm agents by predictive score and exports an elite population"
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

    /// Path to base swarm SharedConfig (JSON)
    #[arg(short, long)]
    shared_config: PathBuf,

    /// Candidate agents sampled per feed (default: 12, yielding 192 total candidates)
    #[arg(long, default_value_t = 12)]
    candidates_per_feed: u32,

    /// Elite agents to keep per feed (default: 6, yielding 96 final agents)
    #[arg(long, default_value_t = 6)]
    keep_per_feed: u32,

    /// Output path for the pruned swarm SharedConfig (JSON)
    #[arg(short, long)]
    output: PathBuf,
}

/// The calibration period `[start_ms, end_ms)` in Unix milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CalibrationWindow {
    start_ms: i64,
    end_ms: i64,
}

impl CalibrationWindow {
    fn from_days(from: &str, to: &str) -> Result<Self> {
        let start_ms = parse_day_start_ms(from).with_context(|| "invalid --from")?;
        let end_ms = parse_day_start_ms(to).with_context(|| "invalid --to")?;
        if start_ms >= end_ms {
            bail!("--from ({from}) must be earlier than --to ({to})");
        }
        Ok(Self { start_ms, end_ms })
    }

    fn contains(&self, timestamp_ms: i64) -> bool {
        self.start_ms <= timestamp_ms && timestamp_ms < self.end_ms
    }
}

fn parse_day_start_ms(day: &str) -> Result<i64> {
    let date = NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .with_context(|| format!("expected a YYYY-MM-DD day, got '{day}'"))?;
    let midnight = date
        .and_hms_opt(0, 0, 0)
        .with_context(|| format!("no midnight for '{day}'"))?;
    Ok(midnight.and_utc().timestamp_millis())
}

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

fn load_config(path: &Path) -> Result<SwarmConfig> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("{} is not a valid swarm SharedConfig", path.display()))
}

/// Runs candidate evaluation and performs stratified pruning.
pub fn prune_candidates(
    base: &SwarmConfig,
    trades: &[Trade],
    candidates_per_feed: u32,
    keep_per_feed: u32,
) -> Result<SwarmConfig> {
    if candidates_per_feed == 0 || keep_per_feed == 0 {
        bail!("candidates_per_feed and keep_per_feed must be strictly positive");
    }
    if candidates_per_feed < keep_per_feed {
        bail!(
            "candidates_per_feed ({candidates_per_feed}) must be >= keep_per_feed ({keep_per_feed})"
        );
    }
    if trades.is_empty() {
        bail!("no trades to evaluate agents against");
    }

    let mut candidate_config = base.clone();
    candidate_config.agents_per_feed = candidates_per_feed;
    candidate_config.custom_agents = None;
    candidate_config
        .validate()
        .map_err(|e| anyhow::anyhow!("candidate configuration is invalid: {e}"))?;

    let mut ensemble = SwarmEnsemble::new(&candidate_config)
        .map_err(|e| anyhow::anyhow!("failed to build candidate ensemble: {e}"))?;

    let total_candidates = (candidates_per_feed as usize) * FEED_COUNT;
    let mut scores = vec![Decimal::ZERO; total_candidates];
    let mut eval_counts = vec![0u64; total_candidates];

    let mut last_bar_price: [Option<Decimal>; FEED_COUNT] = [None; FEED_COUNT];
    let mut last_votes: Vec<Vec<Decimal>> =
        vec![vec![Decimal::ZERO; candidates_per_feed as usize]; FEED_COUNT];

    for trade in trades {
        ensemble.on_trade(trade);

        // Check which feeds closed a bar on this trade
        for (f, closed) in ensemble.last_closed().iter().enumerate() {
            if !*closed {
                continue;
            }

            // Score previous votes on this feed's bar-to-bar return
            if let Some(prev_price) = last_bar_price[f] {
                if prev_price > Decimal::ZERO {
                    let ret = (trade.price - prev_price) / prev_price;
                    let per_feed = candidates_per_feed as usize;
                    for (k, &vote) in last_votes[f].iter().enumerate().take(per_feed) {
                        let agent_id = f * per_feed + k;
                        scores[agent_id] += vote * ret;
                        eval_counts[agent_id] += 1;
                    }
                }
            }

            last_bar_price[f] = Some(trade.price);

            // Record current votes for the next bar
            let per_feed = candidates_per_feed as usize;
            for (k, slot) in last_votes[f].iter_mut().enumerate().take(per_feed) {
                let agent_id = f * per_feed + k;
                let agent_vote = ensemble.votes()[agent_id];
                *slot = Decimal::from(agent_vote.long) - Decimal::from(agent_vote.short);
            }
        }
    }

    // Stratified selection: pick top `keep_per_feed` agents for each feed
    let candidate_agents = ensemble.swarm().agents();
    let mut selected_agents = Vec::with_capacity((keep_per_feed as usize) * FEED_COUNT);
    let per_feed_cand = candidates_per_feed as usize;
    let per_feed_keep = keep_per_feed as usize;

    let mut new_id = 0u32;
    for f in 0..FEED_COUNT {
        let feed_start = f * per_feed_cand;
        let mut feed_agent_indices: Vec<usize> = (0..per_feed_cand).collect();

        // Sort descending by score, tie-breaking by original id
        feed_agent_indices.sort_by(|&a, &b| {
            let id_a = feed_start + a;
            let id_b = feed_start + b;
            scores[id_b]
                .cmp(&scores[id_a])
                .then_with(|| id_a.cmp(&id_b))
        });

        // Retain top `keep_per_feed`
        for &k in feed_agent_indices.iter().take(per_feed_keep) {
            let orig_agent = &candidate_agents[feed_start + k];
            selected_agents.push(Agent {
                id: new_id,
                feed: f,
                genome: orig_agent.genome,
            });
            new_id += 1;
        }
    }

    let mut output_config = base.clone();
    output_config.agents_per_feed = keep_per_feed;
    output_config.custom_agents = Some(selected_agents);
    output_config
        .validate()
        .map_err(|e| anyhow::anyhow!("pruned configuration is invalid: {e}"))?;

    Ok(output_config)
}

fn main() -> Result<()> {
    let args = Args::parse();
    let window = CalibrationWindow::from_days(&args.from, &args.to)?;
    let base = load_config(&args.shared_config)?;

    let mut stream = open_stream(&args.data)?;
    let mut trades = Vec::new();
    let mut read = 0_usize;
    while let Some(trade) = stream.next_trade()? {
        read += 1;
        if window.contains(trade.timestamp) {
            trades.push(trade);
        }
    }

    if trades.is_empty() {
        bail!(
            "no trades in the calibration period [{}, {}) of {} ({read} trades read)",
            args.from,
            args.to,
            args.data.display()
        );
    }

    println!(
        "Evaluating {} candidate agents ({} per feed over 16 feeds)...",
        args.candidates_per_feed * (FEED_COUNT as u32),
        args.candidates_per_feed
    );

    let pruned = prune_candidates(&base, &trades, args.candidates_per_feed, args.keep_per_feed)?;

    let json = serde_json::to_string_pretty(&pruned)?;
    std::fs::write(&args.output, json + "\n")
        .with_context(|| format!("Failed to write {}", args.output.display()))?;

    println!(
        "Successfully pruned swarm! Selected {} elite agents ({} per feed).",
        args.keep_per_feed * (FEED_COUNT as u32),
        args.keep_per_feed
    );
    println!("Output written to: {}", args.output.display());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::Side;
    use rust_decimal_macros::dec;

    fn dummy_trade(ts: i64, price: Decimal) -> Trade {
        Trade::new(ts, price, dec!(1), Side::Buy).unwrap()
    }

    #[test]
    fn prune_candidates_validation_and_stratified_counts() {
        let base = SwarmConfig::default();
        let mut trades = Vec::new();
        for i in 1..=500 {
            trades.push(dummy_trade(i * 60_000, dec!(40000) + Decimal::from(i % 20)));
        }

        // Candidates must be >= keep
        assert!(prune_candidates(&base, &trades, 4, 6).is_err());

        // Zero counts rejected
        assert!(prune_candidates(&base, &trades, 0, 0).is_err());

        // Valid pruning: 12 candidates down to 6
        let pruned = prune_candidates(&base, &trades, 12, 6).unwrap();
        assert_eq!(pruned.agents_per_feed, 6);
        let agents = pruned.custom_agents.as_ref().unwrap();
        assert_eq!(agents.len(), 96);

        // Every agent has the right feed mapping
        for (i, agent) in agents.iter().enumerate() {
            assert_eq!(agent.id, i as u32);
            assert_eq!(agent.feed, i / 6);
        }
    }
}
