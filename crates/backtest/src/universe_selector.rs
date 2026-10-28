//! Cross-Sectional Universe Ranking and Dynamic Top-K Selector for Multi-Asset Portfolios.
//!
//! Evaluates candidate assets on a daily synchronized physical time horizon (00:00 UTC):
//! 1. Relative momentum vs benchmark (BTC) normalized by local volatility.
//! 2. Quantum potential well compression and restorative force.
//! 3. Normalized volume anomaly.
//!
//! Applies hysteresis rank buffering to avoid excessive portfolio churning and transaction fees.

use std::collections::{HashMap, HashSet, VecDeque};

use domain::{AssetRankingScore, DollarBar, UniverseSelectionPolicy};
use rust_decimal::Decimal;
use rust_decimal::MathematicalOps;
use rust_decimal_macros::dec;

/// Per-asset rolling tracking state.
#[derive(Debug, Clone)]
struct AssetState {
    /// Rolling window of (timestamp_ms, close_price, quote_volume)
    history: VecDeque<(i64, Decimal, Decimal)>,
    /// Latest observed close price
    last_close: Decimal,
    /// Latest observed quantum compression score (e.g. depth in potential well)
    last_quantum_metric: Decimal,
    /// Rolling 24h accumulated quote volume
    volume_24h: Decimal,
    /// Rolling baseline average 24h volume
    baseline_volume: Decimal,
}

impl AssetState {
    fn new() -> Self {
        Self {
            history: VecDeque::with_capacity(500),
            last_close: Decimal::ZERO,
            last_quantum_metric: Decimal::ZERO,
            volume_24h: Decimal::ZERO,
            baseline_volume: dec!(10_000_000), // Default 10M baseline until warm
        }
    }

    fn push_bar(
        &mut self,
        timestamp_ms: i64,
        close: Decimal,
        volume: Decimal,
        quantum_metric: Decimal,
    ) {
        if close <= Decimal::ZERO {
            return;
        }
        self.last_close = close;
        self.last_quantum_metric = quantum_metric;
        self.history.push_back((timestamp_ms, close, volume));

        // Purge data older than 48 hours to prevent unbounded memory growth
        let cutoff = timestamp_ms.saturating_sub(48 * 3_600_000);
        while let Some(&(t, _, _)) = self.history.front() {
            if t < cutoff {
                self.history.pop_front();
            } else {
                break;
            }
        }

        // Calculate 24h volume
        let horizon_24h = timestamp_ms.saturating_sub(24 * 3_600_000);
        let vol_24h: Decimal = self
            .history
            .iter()
            .filter(|(t, _, _)| *t >= horizon_24h)
            .map(|(_, _, v)| *v)
            .sum();
        self.volume_24h = vol_24h;

        // Exponential smoothing update of baseline volume
        if self.volume_24h > Decimal::ZERO {
            if self.baseline_volume == dec!(10_000_000) {
                self.baseline_volume = self.volume_24h;
            } else {
                // alpha = 0.05
                self.baseline_volume =
                    self.baseline_volume * dec!(0.95) + self.volume_24h * dec!(0.05);
            }
        }
    }

    /// Calculates return over the last 24 physical hours: (P(t) - P(t - 24h)) / P(t - 24h)
    fn return_24h(&self, current_ts: i64) -> Option<Decimal> {
        if self.last_close <= Decimal::ZERO {
            return None;
        }
        let target_ts = current_ts - 24 * 3_600_000;
        let past_close = self
            .history
            .iter()
            .rev()
            .find(|(t, _, _)| *t <= target_ts)
            .map(|(_, p, _)| *p)?;

        if past_close > Decimal::ZERO {
            Some((self.last_close - past_close) / past_close)
        } else {
            None
        }
    }
}

/// Dynamic Cross-Sectional Universe Selector.
#[derive(Debug, Clone)]
pub struct CrossSectionalUniverseSelector {
    policy: UniverseSelectionPolicy,
    benchmark_symbol: String,
    states: HashMap<String, AssetState>,
    last_rebalance_ts: i64,
    current_ranking: Vec<AssetRankingScore>,
    authorized_for_entry: HashSet<String>,
}

impl CrossSectionalUniverseSelector {
    pub fn new(policy: UniverseSelectionPolicy, benchmark_symbol: impl Into<String>) -> Self {
        Self {
            policy,
            benchmark_symbol: benchmark_symbol.into(),
            states: HashMap::new(),
            last_rebalance_ts: 0,
            current_ranking: Vec::new(),
            authorized_for_entry: HashSet::new(),
        }
    }

    /// Registers a candidate asset symbol into the tracked universe pool.
    pub fn register_asset(&mut self, symbol: impl Into<String>) {
        self.states
            .entry(symbol.into())
            .or_insert_with(AssetState::new);
    }

    /// Records a new incoming DollarBar for an asset and checks if daily rebalance is due.
    pub fn process_bar(&mut self, symbol: &str, bar: &DollarBar, quantum_metric: Option<Decimal>) {
        let state = self
            .states
            .entry(symbol.to_string())
            .or_insert_with(AssetState::new);
        state.push_bar(
            bar.end_time,
            bar.close,
            bar.volume,
            quantum_metric.unwrap_or(Decimal::ZERO),
        );

        if self.last_rebalance_ts == 0 {
            self.last_rebalance_ts = bar.end_time;
            self.rebalance(bar.end_time);
        } else if bar.end_time.saturating_sub(self.last_rebalance_ts)
            >= self.policy.rebalance_interval_ms
        {
            self.rebalance(bar.end_time);
            self.last_rebalance_ts = bar.end_time;
        }
    }

    /// Evaluates cross-sectional scores and updates rankings and authorized entry set.
    pub fn rebalance(&mut self, current_ts: i64) {
        if self.states.is_empty() {
            return;
        }

        // 1. Gather benchmark return (BTC)
        let benchmark_ret = self
            .states
            .get(&self.benchmark_symbol)
            .and_then(|s| s.return_24h(current_ts))
            .unwrap_or(Decimal::ZERO);

        // 2. Compute raw metrics per symbol
        let mut raw_momentums = Vec::new();
        let mut raw_quantums = Vec::new();
        let mut raw_volumes = Vec::new();

        for (sym, state) in &self.states {
            let ret = state.return_24h(current_ts).unwrap_or(Decimal::ZERO);
            let rel_mom = ret - benchmark_ret;
            raw_momentums.push((sym.clone(), rel_mom));
            raw_quantums.push((sym.clone(), state.last_quantum_metric));

            let vol_ratio = if state.baseline_volume > Decimal::ZERO {
                state.volume_24h / state.baseline_volume
            } else {
                Decimal::ONE
            };
            raw_volumes.push((sym.clone(), vol_ratio));
        }

        // 3. Normalize into cross-sectional Z-Scores
        let z_mom = compute_z_scores(&raw_momentums);
        let z_quant = compute_z_scores(&raw_quantums);
        let z_vol = compute_z_scores(&raw_volumes);

        // 4. Compute composite scores
        let mut candidate_scores = Vec::new();
        for sym in self.states.keys() {
            let zm = z_mom.get(sym).copied().unwrap_or(Decimal::ZERO);
            let zq = z_quant.get(sym).copied().unwrap_or(Decimal::ZERO);
            let zv = z_vol.get(sym).copied().unwrap_or(Decimal::ZERO);
            let comp = self.policy.compute_composite_score(zm, zq, zv);

            candidate_scores.push(AssetRankingScore {
                symbol: sym.clone(),
                momentum_z: zm,
                quantum_z: zq,
                volume_z: zv,
                composite_score: comp,
                rank: 0,
            });
        }

        // 5. Sort descending by composite_score
        candidate_scores.sort_by_key(|a| std::cmp::Reverse(a.composite_score));

        // Assign ranks 1..N
        for (idx, item) in candidate_scores.iter_mut().enumerate() {
            item.rank = idx + 1;
        }

        // 6. Update authorized entries: rank <= top_k
        let mut new_authorized = HashSet::new();
        for item in &candidate_scores {
            if item.rank <= self.policy.top_k {
                if let Some(min_s) = self.policy.min_score_threshold {
                    if item.composite_score < min_s {
                        continue;
                    }
                }
                new_authorized.insert(item.symbol.clone());
            }
        }

        self.authorized_for_entry = new_authorized;
        self.current_ranking = candidate_scores;
    }

    /// Checks if a symbol is currently permitted to open new positions (Rank <= Top-K).
    pub fn is_symbol_authorized_for_entry(&self, symbol: &str) -> bool {
        self.authorized_for_entry.contains(symbol)
    }

    /// Checks if an existing holding can be maintained under hysteresis rules (Rank <= Top-K + tolerance).
    pub fn is_symbol_eligible_for_retention(&self, symbol: &str) -> bool {
        let max_rank = self.policy.max_retention_rank();
        if let Some(score) = self.current_ranking.iter().find(|s| s.symbol == symbol) {
            score.rank <= max_rank
        } else {
            true // If unranked yet, do not force-close
        }
    }

    /// Returns the current rank of a symbol if present.
    pub fn current_rank(&self, symbol: &str) -> Option<usize> {
        self.current_ranking
            .iter()
            .find(|s| s.symbol == symbol)
            .map(|s| s.rank)
    }

    /// Returns the full current ranking snapshot.
    pub fn current_ranking(&self) -> &[AssetRankingScore] {
        &self.current_ranking
    }

    /// Active policy reference.
    pub fn policy(&self) -> &UniverseSelectionPolicy {
        &self.policy
    }
}

/// Normalizes a collection of values into cross-sectional Z-Scores: (X - mean) / std_dev.
fn compute_z_scores(values: &[(String, Decimal)]) -> HashMap<String, Decimal> {
    if values.is_empty() {
        return HashMap::new();
    }
    if values.len() == 1 {
        let mut map = HashMap::new();
        map.insert(values[0].0.clone(), Decimal::ZERO);
        return map;
    }

    let n = Decimal::from(values.len());
    let sum: Decimal = values.iter().map(|(_, v)| *v).sum();
    let mean = sum / n;

    let n_minus_1 = Decimal::from(values.len() - 1);
    let var_sum: Decimal = values
        .iter()
        .map(|(_, v)| {
            let diff = *v - mean;
            diff * diff
        })
        .sum();
    let variance = var_sum / n_minus_1;

    let mut map = HashMap::new();
    if variance <= Decimal::ZERO {
        for (sym, _) in values {
            map.insert(sym.clone(), Decimal::ZERO);
        }
        return map;
    }

    let std_dev = variance.sqrt().unwrap_or(Decimal::ONE);
    if std_dev.is_zero() {
        for (sym, _) in values {
            map.insert(sym.clone(), Decimal::ZERO);
        }
        return map;
    }

    for (sym, val) in values {
        let z = (*val - mean) / std_dev;
        map.insert(sym.clone(), z);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_compute_z_scores_symmetry() {
        let values = vec![
            ("A".to_string(), dec!(10)),
            ("B".to_string(), dec!(20)),
            ("C".to_string(), dec!(30)),
        ];
        let scores = compute_z_scores(&values);
        assert_eq!(scores.len(), 3);
        // Mean is 20, B is 0.0
        assert_eq!(*scores.get("B").unwrap(), Decimal::ZERO);
        assert!(*scores.get("C").unwrap() > Decimal::ZERO);
        assert!(*scores.get("A").unwrap() < Decimal::ZERO);
        // C and A should be symmetric opposites
        assert_eq!(*scores.get("C").unwrap(), -*scores.get("A").unwrap());
    }

    #[test]
    fn test_universe_selector_ranking_and_hysteresis() {
        let policy = UniverseSelectionPolicy {
            top_k: 2,
            hysteresis_tolerance: 1, // Max retention rank: 3
            ..UniverseSelectionPolicy::default()
        };

        let mut selector = CrossSectionalUniverseSelector::new(policy, "BTCUSDT");

        // Feed synthetic bars for 4 assets: BTC, ETH, SOL, AVAX
        let t0 = 1_000_000_000;
        let t24 = t0 + 24 * 3_600_000;

        let make_bar = |ts, close| DollarBar {
            start_time: ts,
            end_time: ts,
            open: close,
            high: close,
            low: close,
            close,
            volume: dec!(100_000_000),
            dollar_volume: dec!(100_000_000),
            trade_count: 1000,
            buy_volume: dec!(50_000_000),
            sell_volume: dec!(50_000_000),
            buy_dollar_volume: dec!(50_000_000),
            sell_dollar_volume: dec!(50_000_000),
        };

        // Day 0 bars
        selector.process_bar("BTCUSDT", &make_bar(t0, dec!(100)), Some(dec!(0.5)));
        selector.process_bar("ETHUSDT", &make_bar(t0, dec!(100)), Some(dec!(0.5)));
        selector.process_bar("SOLUSDT", &make_bar(t0, dec!(100)), Some(dec!(0.5)));
        selector.process_bar("AVAXUSDT", &make_bar(t0, dec!(100)), Some(dec!(0.5)));

        // Day 1 bars: ETH performs best (+20%), SOL (+10%), BTC (0%), AVAX (-10%)
        selector.process_bar("BTCUSDT", &make_bar(t24, dec!(100)), Some(dec!(0.5)));
        selector.process_bar("ETHUSDT", &make_bar(t24, dec!(120)), Some(dec!(1.5)));
        selector.process_bar("SOLUSDT", &make_bar(t24, dec!(110)), Some(dec!(1.0)));
        selector.process_bar("AVAXUSDT", &make_bar(t24, dec!(90)), Some(dec!(-0.5)));

        // Rebalance
        selector.rebalance(t24);

        let ranking = selector.current_ranking();
        assert_eq!(ranking.len(), 4);

        // ETH should be rank 1, SOL rank 2
        assert_eq!(ranking[0].symbol, "ETHUSDT");
        assert_eq!(ranking[0].rank, 1);
        assert_eq!(ranking[1].symbol, "SOLUSDT");
        assert_eq!(ranking[1].rank, 2);

        // Top-2 are authorized for entry
        assert!(selector.is_symbol_authorized_for_entry("ETHUSDT"));
        assert!(selector.is_symbol_authorized_for_entry("SOLUSDT"));
        assert!(!selector.is_symbol_authorized_for_entry("BTCUSDT"));
        assert!(!selector.is_symbol_authorized_for_entry("AVAXUSDT"));

        // Hysteresis test: Rank 3 (BTC) is eligible for retention (rank <= 3)
        assert!(selector.is_symbol_eligible_for_retention("BTCUSDT"));
        // Rank 4 (AVAX) is NOT eligible for retention (rank 4 > 3)
        assert!(!selector.is_symbol_eligible_for_retention("AVAXUSDT"));
    }
}
