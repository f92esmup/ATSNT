//! The pre-trade gate: the one place a signal becomes a sized order or is refused.
//!
//! Every runner (the bar engine, the trading session on a simulated or a live venue) asks this
//! gate, so a signal is refused for the same reason and sized to the same quantity wherever it
//! is decided.

use rust_decimal::Decimal;

use crate::market::MarketType;
use crate::metrics::{DAILY_HALT_REJECTION, SPOT_SHORTING_DISALLOWED};
use crate::order::OrderIntent;
use crate::risk::{RiskError, RiskPolicy};
use crate::trade::Side;

/// Why the gate did not turn a signal into an order.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EntryRefusal {
    /// The daily drawdown breaker is tripped: no new entries until the next UTC day.
    #[error("the daily drawdown breaker is tripped")]
    DailyHalt,
    /// A spot account cannot open a short.
    #[error("a spot account cannot open a short position")]
    SpotShortingDisallowed,
    /// Sizing or a limit of the risk policy refused the order.
    #[error(transparent)]
    Risk(#[from] RiskError),
}

impl EntryRefusal {
    /// Stable name of the reason, used to count refusals without their values.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::DailyHalt => DAILY_HALT_REJECTION,
            Self::SpotShortingDisallowed => SPOT_SHORTING_DISALLOWED,
            Self::Risk(error) => error.kind(),
        }
    }
}

/// The account rules a signal must pass to become an order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryGate {
    policy: RiskPolicy,
    risk_per_trade_pct: Decimal,
    market: MarketType,
}

impl EntryGate {
    /// A gate that sizes with `policy`, risking `risk_per_trade_pct` of equity when the signal
    /// carries no risk of its own, on `market`.
    pub fn new(policy: RiskPolicy, risk_per_trade_pct: Decimal, market: MarketType) -> Self {
        Self {
            policy,
            risk_per_trade_pct,
            market,
        }
    }

    /// The risk policy the gate sizes with.
    pub fn policy(&self) -> &RiskPolicy {
        &self.policy
    }

    /// The quantity to trade for `intent`, or why it is refused.
    ///
    /// `equity` is the marked equity of the account and `day_peak` the peak of its UTC day;
    /// `breaker_tripped` is the state of the daily breaker. The checks run in this order: the
    /// breaker, the market's rules, then sizing at the signal price with the policy's limits.
    pub fn admit(
        &self,
        intent: &OrderIntent,
        equity: Decimal,
        day_peak: Decimal,
        breaker_tripped: bool,
    ) -> Result<Decimal, EntryRefusal> {
        if breaker_tripped {
            return Err(EntryRefusal::DailyHalt);
        }
        if self.market == MarketType::Spot && intent.side == Side::Sell {
            return Err(EntryRefusal::SpotShortingDisallowed);
        }
        let drawdown = RiskPolicy::calculate_session_drawdown_pct(day_peak, equity)?;
        Ok(self.policy.size_order(
            intent,
            intent.price,
            equity,
            self.risk_per_trade_pct,
            Decimal::ZERO,
            drawdown,
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn long() -> OrderIntent {
        OrderIntent::new(1, Side::Buy, dec!(100), dec!(98), dec!(104), 5).unwrap()
    }

    fn short() -> OrderIntent {
        OrderIntent::new(1, Side::Sell, dec!(100), dec!(102), dec!(96), 5).unwrap()
    }

    fn gate(market: MarketType) -> EntryGate {
        EntryGate::new(RiskPolicy::default(), dec!(0.01), market)
    }

    #[test]
    fn an_admitted_entry_is_sized_by_the_policy() {
        let gate = gate(MarketType::UsdMPerpetual);
        let expected = RiskPolicy::default()
            .size_order(&long(), dec!(100), dec!(1000), dec!(0.01), dec!(0), dec!(0))
            .unwrap();

        assert_eq!(
            gate.admit(&long(), dec!(1000), dec!(1000), false),
            Ok(expected)
        );
        // Risking 1 % of 1000 over a stop 2 away: 5 units.
        assert_eq!(expected, dec!(5));
        assert!(gate.admit(&short(), dec!(1000), dec!(1000), false).is_ok());
    }

    #[test]
    fn a_tripped_breaker_refuses_before_anything_else() {
        let refusal = gate(MarketType::Spot)
            .admit(&short(), dec!(1000), dec!(1000), true)
            .unwrap_err();
        assert_eq!(refusal, EntryRefusal::DailyHalt);
        assert_eq!(refusal.kind(), "DailyHalt");
    }

    #[test]
    fn spot_refuses_shorts_and_admits_longs() {
        let gate = gate(MarketType::Spot);
        let refusal = gate
            .admit(&short(), dec!(1000), dec!(1000), false)
            .unwrap_err();
        assert_eq!(refusal, EntryRefusal::SpotShortingDisallowed);
        assert_eq!(refusal.kind(), "SpotShortingDisallowed");
        assert!(gate.admit(&long(), dec!(1000), dec!(1000), false).is_ok());
    }

    #[test]
    fn a_policy_refusal_keeps_its_kind() {
        // A stop 0.01 away asks for a notional far above the default order cap.
        let tight = OrderIntent::new(1, Side::Buy, dec!(100), dec!(99.99), dec!(104), 5).unwrap();
        let refusal = gate(MarketType::UsdMPerpetual)
            .admit(&tight, dec!(1_000_000), dec!(1_000_000), false)
            .unwrap_err();
        assert!(matches!(refusal, EntryRefusal::Risk(_)));
        assert_eq!(refusal.kind(), "OrderSizeExceeded");
    }

    #[test]
    fn a_loss_from_the_day_peak_at_the_policy_limit_is_refused_by_the_policy() {
        let policy = RiskPolicy::default();
        let limit = policy.max_daily_drawdown_pct;
        let equity = dec!(1000) * (Decimal::ONE - limit);
        let refusal = gate(MarketType::UsdMPerpetual)
            .admit(&long(), equity, dec!(1000), false)
            .unwrap_err();
        assert_eq!(refusal.kind(), "CircuitBreakerTriggered");
    }
}
