//! Instruments: what a trade, a position and an order are about, and the exchange's rules for
//! trading each one.

use std::borrow::Cow;
use std::fmt;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::error::DomainError;
use crate::market::MarketType;
use crate::risk::RiskPolicy;

/// The name of a tradable instrument, such as `BTCUSDT`.
///
/// Non-empty ASCII letters, digits and `-`, `_`, `/`, `.`. It orders and compares as text, so maps
/// keyed by it iterate in a deterministic order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct InstrumentId(Cow<'static, str>);

/// The instrument of a single-instrument run whose data does not name one.
pub static UNNAMED_INSTRUMENT: InstrumentId = InstrumentId::from_static("UNNAMED");

impl InstrumentId {
    /// A validated instrument name.
    pub fn new(name: impl Into<String>) -> Result<Self, DomainError> {
        let name = name.into();
        if Self::is_valid(&name) {
            Ok(Self(Cow::Owned(name)))
        } else {
            Err(DomainError::InvalidInstrument(name))
        }
    }

    /// An instrument named by a literal. The name is not checked: use it for constants only.
    pub const fn from_static(name: &'static str) -> Self {
        Self(Cow::Borrowed(name))
    }

    /// The name as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn is_valid(name: &str) -> bool {
        !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/' | '.'))
    }
}

impl fmt::Display for InstrumentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for InstrumentId {
    type Error = DomainError;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        Self::new(name)
    }
}

impl From<InstrumentId> for String {
    fn from(id: InstrumentId) -> Self {
        id.0.into_owned()
    }
}

/// The exchange's rules for one instrument: its market and its order filters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstrumentSpec {
    /// The instrument.
    pub id: InstrumentId,
    /// The market it trades on.
    pub market: MarketType,
    /// Smallest order quantity, in base units (LOT_SIZE `minQty`).
    pub min_quantity: Decimal,
    /// Quantity increment, in base units (LOT_SIZE `stepSize`).
    pub quantity_step: Decimal,
    /// Smallest order notional, in quote units (MIN_NOTIONAL).
    pub min_notional: Decimal,
}

impl InstrumentSpec {
    /// The spec of `id` with the order filters `policy` carries: the one global set the
    /// single-instrument runners use.
    pub fn from_policy(id: InstrumentId, market: MarketType, policy: &RiskPolicy) -> Self {
        Self {
            id,
            market,
            min_quantity: policy.min_order_quantity,
            quantity_step: policy.quantity_step_size,
            min_notional: policy.min_order_notional,
        }
    }
}

impl RiskPolicy {
    /// This policy with the order filters of `spec` in place of its own.
    pub fn for_instrument(&self, spec: &InstrumentSpec) -> Self {
        Self {
            min_order_quantity: spec.min_quantity,
            quantity_step_size: spec.quantity_step,
            min_order_notional: spec.min_notional,
            ..self.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn an_instrument_name_is_validated_and_round_trips() {
        let id = InstrumentId::new("BTCUSDT").unwrap();
        assert_eq!(id.as_str(), "BTCUSDT");
        assert_eq!(id.to_string(), "BTCUSDT");
        assert_eq!(id, InstrumentId::from_static("BTCUSDT"));

        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"BTCUSDT\"");
        assert_eq!(serde_json::from_str::<InstrumentId>(&json).unwrap(), id);

        for bad in ["", "BTC USDT", "BTC;"] {
            assert!(InstrumentId::new(bad).is_err(), "{bad:?}");
            assert!(serde_json::from_str::<InstrumentId>(&format!("\"{bad}\"")).is_err());
        }
    }

    #[test]
    fn instruments_order_as_text() {
        let mut ids = [
            InstrumentId::from_static("ETHUSDT"),
            InstrumentId::from_static("BTCUSDT"),
        ];
        ids.sort();
        assert_eq!(ids[0].as_str(), "BTCUSDT");
    }

    #[test]
    fn a_spec_replaces_the_policy_order_filters() {
        let policy = RiskPolicy::default();
        let spec = InstrumentSpec {
            id: InstrumentId::from_static("ETHUSDT"),
            market: MarketType::Spot,
            min_quantity: dec!(0.01),
            quantity_step: dec!(0.01),
            min_notional: dec!(10),
        };
        let narrowed = policy.for_instrument(&spec);
        assert_eq!(narrowed.min_order_quantity, dec!(0.01));
        assert_eq!(narrowed.min_order_notional, dec!(10));
        assert_eq!(narrowed.max_order_notional, policy.max_order_notional);

        let round_trip = InstrumentSpec::from_policy(spec.id.clone(), spec.market, &narrowed);
        assert_eq!(round_trip, spec);
    }
}
