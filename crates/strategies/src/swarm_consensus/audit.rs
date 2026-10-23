//! The swarm audit (strategy spec §16): what the swarm did, counted over every agent evaluation
//! and every emitted sample.
//!
//! This holds the Stage A part of the audit (votes, conflicts, noise, rule firings, clamp hits,
//! consensus strength, polarisation). Level activation and execution counters belong to the
//! policy and the execution core.

use rust_decimal::Decimal;
use serde_json::{json, Map, Value};

use super::genome::{Swarm, GENE_COUNT, GENE_PRIORS};
use super::rules::{ConflictKind, Evaluation, RuleId, RULE_COUNT};
use super::sample::ConsensusSample;

/// Counters of the swarm's behaviour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwarmAudit {
    agents: u32,
    clamp_hits: [u32; GENE_COUNT],
    evaluations: u64,
    cold: u64,
    noise: u64,
    /// Indexed `[long + 1][short + 1]`.
    votes: [[u64; 3]; 3],
    /// Indexed by [`ConflictKind`] in declaration order.
    conflicts: [u64; 4],
    rule_firings: [u64; RULE_COUNT],
    samples: u64,
    sum_abs_long_net: Decimal,
    sum_abs_short_net: Decimal,
    polarised_long: u64,
    polarised_short: u64,
}

fn conflict_index(kind: ConflictKind) -> usize {
    match kind {
        ConflictKind::None => 0,
        ConflictKind::WithinAxis => 1,
        ConflictKind::Directional => 2,
        ConflictKind::Both => 3,
    }
}

fn vote_index(vote: i8) -> usize {
    match vote {
        v if v < 0 => 0,
        0 => 1,
        _ => 2,
    }
}

fn rate(count: u64, total: u64) -> Decimal {
    if total == 0 {
        Decimal::ZERO
    } else {
        (Decimal::from(count) / Decimal::from(total)).normalize()
    }
}

impl SwarmAudit {
    /// An empty audit for `swarm`, carrying its clamp hits.
    pub fn new(swarm: &Swarm) -> Self {
        Self {
            agents: swarm.agents().len() as u32,
            clamp_hits: *swarm.clamp_hits(),
            evaluations: 0,
            cold: 0,
            noise: 0,
            votes: [[0; 3]; 3],
            conflicts: [0; 4],
            rule_firings: [0; RULE_COUNT],
            samples: 0,
            sum_abs_long_net: Decimal::ZERO,
            sum_abs_short_net: Decimal::ZERO,
            polarised_long: 0,
            polarised_short: 0,
        }
    }

    /// Records one agent evaluation.
    pub fn record(&mut self, evaluation: &Evaluation) {
        self.evaluations += 1;
        if evaluation.cold {
            self.cold += 1;
        } else if evaluation.noise {
            self.noise += 1;
        }
        let vote = evaluation.vote;
        self.votes[vote_index(vote.long)][vote_index(vote.short)] += 1;
        self.conflicts[conflict_index(vote.conflict)] += 1;
        for (index, count) in self.rule_firings.iter_mut().enumerate() {
            if evaluation.fired_rules & (1 << index) != 0 {
                *count += 1;
            }
        }
    }

    /// Records one emitted sample (consensus strength and polarisation).
    pub fn record_sample(&mut self, sample: &ConsensusSample) {
        self.samples += 1;
        self.sum_abs_long_net += sample.long_net().abs();
        self.sum_abs_short_net += sample.short_net().abs();
        // Both sides above 10% of N.
        let tenth = |count: u32| u64::from(count) * 10 > u64::from(sample.agents);
        if tenth(sample.long_up) && tenth(sample.long_down) {
            self.polarised_long += 1;
        }
        if tenth(sample.short_up) && tenth(sample.short_down) {
            self.polarised_short += 1;
        }
    }

    /// Number of agent evaluations recorded.
    pub fn evaluations(&self) -> u64 {
        self.evaluations
    }

    /// Evaluations of agents whose feed had no indicator snapshot.
    pub fn cold_evaluations(&self) -> u64 {
        self.cold
    }

    /// Evaluations vetoed by an empty micro set.
    pub fn noise_evaluations(&self) -> u64 {
        self.noise
    }

    /// Evaluation counts per `(long, short)` vote, indexed `[long + 1][short + 1]`.
    pub fn vote_counts(&self) -> &[[u64; 3]; 3] {
        &self.votes
    }

    /// Evaluation count of a conflict kind.
    pub fn conflict_count(&self, kind: ConflictKind) -> u64 {
        self.conflicts[conflict_index(kind)]
    }

    /// How many evaluations fired a rule (before the noise veto).
    pub fn rule_firings(&self, id: RuleId) -> u64 {
        self.rule_firings[id.index()]
    }

    /// Samples recorded.
    pub fn samples(&self) -> u64 {
        self.samples
    }

    /// The audit as JSON: counts and rates (rates are fractions in `[0, 1]`, as decimal strings).
    pub fn to_json(&self) -> Value {
        let total = self.evaluations;
        // Sum of the vote counts over the given long and short vote indices.
        let axis_count = |longs: &[usize], shorts: &[usize]| -> u64 {
            longs
                .iter()
                .flat_map(|l| shorts.iter().map(move |s| self.votes[*l][*s]))
                .sum()
        };
        const ANY: &[usize] = &[0, 1, 2];
        let distribution = json!({
            "long_up": rate(axis_count(&[2], ANY), total),
            "long_down": rate(axis_count(&[0], ANY), total),
            "short_up": rate(axis_count(ANY, &[2]), total),
            "short_down": rate(axis_count(ANY, &[0]), total),
            "wait": rate(axis_count(&[1], &[1]), total),
        });
        let within =
            self.conflict_count(ConflictKind::WithinAxis) + self.conflict_count(ConflictKind::Both);
        let directional = self.conflict_count(ConflictKind::Directional)
            + self.conflict_count(ConflictKind::Both);
        let mut rules = Map::new();
        for id in RuleId::all() {
            rules.insert(id.to_string(), json!(self.rule_firings(id)));
        }
        let mut clamps = Map::new();
        for (prior, hits) in GENE_PRIORS.iter().zip(self.clamp_hits) {
            clamps.insert(
                prior.name.to_string(),
                json!(rate(u64::from(hits), u64::from(self.agents))),
            );
        }
        let mean = |sum: Decimal| {
            if self.samples == 0 {
                Decimal::ZERO
            } else {
                (sum / Decimal::from(self.samples)).normalize()
            }
        };
        json!({
            "agents": self.agents,
            "evaluations": total,
            "cold_evaluations": self.cold,
            "vote_distribution": distribution,
            "conflict_rate": {
                "any": rate(total - self.conflict_count(ConflictKind::None), total),
                "within_axis": rate(within, total),
                "directional": rate(directional, total),
            },
            "noise_rate": rate(self.noise, total),
            "rule_firings": rules,
            "clamp_hit_rate": clamps,
            "samples": self.samples,
            "mean_abs_net_vote": {
                "long": mean(self.sum_abs_long_net),
                "short": mean(self.sum_abs_short_net),
            },
            "polarisation": {
                "long": rate(self.polarised_long, self.samples),
                "short": rate(self.polarised_short, self.samples),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::genome::{sample_swarm, FEED_COUNT};
    use super::super::rules::AgentVote;
    use super::super::sample::FeedVote;
    use super::*;
    use rust_decimal_macros::dec;

    fn evaluation(long: i8, short: i8, conflict: ConflictKind, fired: u32) -> Evaluation {
        Evaluation {
            vote: AgentVote {
                long,
                short,
                conflict,
            },
            fired_rules: fired,
            noise: false,
            cold: false,
        }
    }

    #[test]
    fn counters_partition_the_evaluations_and_rates_follow() {
        let swarm = sample_swarm(1, 1).unwrap();
        let mut audit = SwarmAudit::new(&swarm);
        audit.record(&evaluation(1, 0, ConflictKind::None, 0b1));
        audit.record(&evaluation(0, 0, ConflictKind::Directional, 0b11));
        audit.record(&evaluation(-1, 1, ConflictKind::None, 0));
        audit.record(&Evaluation {
            cold: true,
            ..Evaluation::default()
        });
        assert_eq!(audit.evaluations(), 4);
        let votes: u64 = audit.vote_counts().iter().flatten().sum();
        assert_eq!(votes, 4);
        let json = audit.to_json();
        assert_eq!(json["vote_distribution"]["long_up"], json!(dec!(0.25)));
        assert_eq!(json["vote_distribution"]["wait"], json!(dec!(0.5)));
        assert_eq!(json["conflict_rate"]["directional"], json!(dec!(0.25)));
        assert_eq!(json["rule_firings"]["E1"], json!(2));
        assert_eq!(json["rule_firings"]["E2"], json!(1));
        assert_eq!(json["cold_evaluations"], json!(1));
    }

    #[test]
    fn samples_record_consensus_strength_and_polarisation() {
        let swarm = sample_swarm(1, 1).unwrap();
        let mut audit = SwarmAudit::new(&swarm);
        let sample = ConsensusSample {
            timestamp: 0,
            agents: 16,
            agents_per_feed: 1,
            long_up: 8,
            long_down: 2,
            short_up: 1,
            short_down: 1,
            waits: 6,
            conflicts: 0,
            feed_long: [FeedVote::default(); FEED_COUNT],
            feed_short: [FeedVote::default(); FEED_COUNT],
            atr_stop_reference: dec!(1),
            macro_trend_slope: Decimal::ZERO,
            price: dec!(1),
        };
        audit.record_sample(&sample);
        let json = audit.to_json();
        assert_eq!(json["mean_abs_net_vote"]["long"], json!(dec!(0.375)));
        assert_eq!(json["polarisation"]["long"], json!(dec!(1)));
        assert_eq!(json["polarisation"]["short"], json!(dec!(0)));
    }
}
