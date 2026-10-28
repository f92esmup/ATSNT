//! Universe selection and cross-sectional asset ranking domain abstractions.
//!
//! Provides deterministic scoring, ranking, and Top-K asset filtering with hysteresis
//! to control portfolio turnover and transaction costs. Zero I/O, 100% decimal math.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Breakdown of scores assigned to an individual candidate asset in cross-sectional evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetRankingScore {
    /// Instrument symbol (e.g. "BTCUSDT", "ETHUSDT").
    pub symbol: String,
    /// Normalized relative momentum score against benchmark asset (e.g. BTC).
    pub momentum_z: Decimal,
    /// Normalized quantum potential well compression / restorative force score.
    pub quantum_z: Decimal,
    /// Normalized volume anomaly score relative to rolling historical volume.
    pub volume_z: Decimal,
    /// Multi-factor composite weighted score.
    pub composite_score: Decimal,
    /// Final ordinal rank assigned in the candidate universe (1 = highest score).
    pub rank: usize,
}

/// Configuration policy governing dynamic universe ranking and allocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UniverseSelectionPolicy {
    /// Number of top-ranked assets eligible to receive new capital allocations.
    pub top_k: usize,
    /// Hysteresis rank buffer: an active holding is kept as long as rank <= top_k + hysteresis_tolerance.
    pub hysteresis_tolerance: usize,
    /// Minimum composite score threshold required to be considered eligible (e.g. Decimal::ZERO).
    pub min_score_threshold: Option<Decimal>,
    /// Weight assigned to relative momentum vs benchmark (default: 0.40).
    pub momentum_weight: Decimal,
    /// Weight assigned to quantum potential well quality (default: 0.40).
    pub quantum_weight: Decimal,
    /// Weight assigned to volume anomaly (default: 0.20).
    pub volume_weight: Decimal,
    /// Rebalance period in milliseconds (default: 24h = 86_400_000 ms).
    pub rebalance_interval_ms: i64,
}

impl Default for UniverseSelectionPolicy {
    fn default() -> Self {
        Self {
            top_k: 3,
            hysteresis_tolerance: 2, // With top_k=3, holds up to rank 5 (3 + 2)
            min_score_threshold: None,
            momentum_weight: Decimal::new(40, 2),        // 0.40
            quantum_weight: Decimal::new(40, 2),         // 0.40
            volume_weight: Decimal::new(20, 2),          // 0.20
            rebalance_interval_ms: 24 * 60 * 60 * 1_000, // 24 hours
        }
    }
}

impl UniverseSelectionPolicy {
    /// Maximum rank permitted to retain an existing position without forced liquidation.
    #[inline]
    pub fn max_retention_rank(&self) -> usize {
        self.top_k + self.hysteresis_tolerance
    }

    /// Evaluates composite score from constituent Z-scores:
    /// composite = w_mom * momentum_z + w_quant * quantum_z + w_vol * volume_z
    pub fn compute_composite_score(
        &self,
        momentum_z: Decimal,
        quantum_z: Decimal,
        volume_z: Decimal,
    ) -> Decimal {
        self.momentum_weight * momentum_z
            + self.quantum_weight * quantum_z
            + self.volume_weight * volume_z
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn policy_defaults_and_max_retention() {
        let policy = UniverseSelectionPolicy::default();
        assert_eq!(policy.top_k, 3);
        assert_eq!(policy.hysteresis_tolerance, 2);
        assert_eq!(policy.max_retention_rank(), 5);
        assert_eq!(policy.momentum_weight, dec!(0.40));
        assert_eq!(policy.quantum_weight, dec!(0.40));
        assert_eq!(policy.volume_weight, dec!(0.20));
        assert_eq!(policy.rebalance_interval_ms, 86_400_000);
    }

    #[test]
    fn composite_score_computation() {
        let policy = UniverseSelectionPolicy::default();
        // 0.40 * 1.5 + 0.40 * 2.0 + 0.20 * 0.5 = 0.60 + 0.80 + 0.10 = 1.50
        let score = policy.compute_composite_score(dec!(1.5), dec!(2.0), dec!(0.5));
        assert_eq!(score, dec!(1.50));
    }
}
