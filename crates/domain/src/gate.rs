//! The pre-trade gate: the one place a signal becomes a sized order or is refused.
//!
//! Every runner (the bar engine, the trading session on a simulated or a live venue) asks this
//! gate, so a signal is refused for the same reason and sized to the same quantity wherever it
//! is decided.

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use std::collections::BTreeMap;

use crate::book::{Book, PortfolioLimits};
use crate::futures;
use crate::instrument::{InstrumentId, InstrumentSpec};
use crate::market::MarketType;
use crate::metrics::{DAILY_HALT_REJECTION, INSUFFICIENT_MARGIN, SPOT_SHORTING_DISALLOWED};
use crate::order::OrderIntent;
use crate::position::PositionSide;
use crate::risk::{RiskError, RiskPolicy};
use crate::trade::Side;

/// Why the gate did not turn a signal into an order.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EntryRefusal {
    /// The day's loss reached the daily drawdown limit: no new entries until the next UTC day.
    #[error("the daily drawdown limit is reached")]
    DailyHalt,
    /// A spot account cannot open a short.
    #[error("a spot account cannot open a short position")]
    SpotShortingDisallowed,
    /// The balance cannot margin even the smallest order the exchange accepts.
    #[error("the balance cannot margin the smallest order")]
    InsufficientMargin,
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
            Self::InsufficientMargin => INSUFFICIENT_MARGIN,
            Self::Risk(error) => error.kind(),
        }
    }
}

/// What the exchange asks to carry a position, as the gate needs it to size an entry the
/// balance can fund. The leverage cap is the risk policy's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarginRule {
    /// Maintenance margin as a fraction of notional.
    pub maintenance_rate: Decimal,
    /// Entry commission as a fraction of notional (the taker rate: the order may fill as taker).
    pub fee_rate: Decimal,
    /// Adverse slippage an entry may pay, as a fraction of the signal price.
    pub slippage_pct: Decimal,
}

/// The account as an entry is judged against it.
#[derive(Debug, Clone, Copy)]
pub struct AccountState<'a> {
    /// Marked equity of the account.
    pub equity: Decimal,
    /// Peak of the marked equity over the current UTC day.
    pub day_peak: Decimal,
    /// Whether the daily breaker has latched.
    pub breaker_tripped: bool,
    /// The positions already open.
    pub book: &'a Book,
    /// Gross notional of `book` at current marks.
    pub gross_exposure: Decimal,
}

/// The account rules a signal must pass to become an order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryGate {
    policy: RiskPolicy,
    risk_per_trade_pct: Decimal,
    market: MarketType,
    margin: Option<MarginRule>,
    limits: PortfolioLimits,
    specs: BTreeMap<InstrumentId, InstrumentSpec>,
}

impl EntryGate {
    /// A gate that sizes with `policy`, risking `risk_per_trade_pct` of equity when the signal
    /// carries no risk of its own, on `market`.
    pub fn new(policy: RiskPolicy, risk_per_trade_pct: Decimal, market: MarketType) -> Self {
        Self {
            policy,
            risk_per_trade_pct,
            market,
            margin: None,
            limits: PortfolioLimits::default(),
            specs: BTreeMap::new(),
        }
    }

    /// The same gate, refusing an entry that would break `limits` on the book.
    pub fn with_portfolio_limits(mut self, limits: PortfolioLimits) -> Self {
        self.limits = limits;
        self
    }

    /// The same gate, sizing an entry of each instrument with that instrument's market and order
    /// filters. An instrument without a spec uses the gate's market and the policy's filters.
    pub fn with_instrument_specs(
        mut self,
        specs: impl IntoIterator<Item = InstrumentSpec>,
    ) -> Self {
        self.specs = specs
            .into_iter()
            .map(|spec| (spec.id.clone(), spec))
            .collect();
        self
    }

    /// The portfolio limits the gate enforces.
    pub fn limits(&self) -> &PortfolioLimits {
        &self.limits
    }

    /// The same gate, sizing an entry down to what the balance can margin under `margin`.
    /// Without it an entry is sized by risk alone and the venue may refuse it.
    pub fn with_margin(mut self, margin: MarginRule) -> Self {
        self.margin = Some(margin);
        self
    }

    /// The risk policy the gate sizes with.
    pub fn policy(&self) -> &RiskPolicy {
        &self.policy
    }

    /// The quantity to trade for `intent`, or why it is refused.
    ///
    /// `equity` is the marked equity of the account and `day_peak` the peak of its UTC day;
    /// `breaker_tripped` is the state of the daily breaker. The checks run in this order: the
    /// daily limit, the market's rules, then sizing at the signal price with the policy's limits.
    ///
    /// The daily limit refuses under one name, [`EntryRefusal::DailyHalt`], whether the breaker
    /// already latched or this signal is the first to find the day's loss at the limit.
    pub fn admit(
        &self,
        intent: &OrderIntent,
        equity: Decimal,
        day_peak: Decimal,
        breaker_tripped: bool,
    ) -> Result<Decimal, EntryRefusal> {
        let book = Book::new();
        let instrument = intent
            .instrument
            .as_ref()
            .unwrap_or(&crate::instrument::UNNAMED_INSTRUMENT);
        self.admit_to_book(
            intent,
            instrument,
            &AccountState {
                equity,
                day_peak,
                breaker_tripped,
                book: &book,
                gross_exposure: Decimal::ZERO,
            },
        )
    }

    /// The quantity to trade for `intent` on `instrument`, or why it is refused, for an account
    /// that may already hold other positions.
    ///
    /// The checks run in this order: the daily limit, the instrument's market rules, the
    /// portfolio limits (open positions, positions on one side), sizing at the signal price with
    /// the policy's limits and the instrument's order filters, the gross exposure limit, and the
    /// margin the balance left free by the open positions can post.
    pub fn admit_to_book(
        &self,
        intent: &OrderIntent,
        instrument: &InstrumentId,
        account: &AccountState<'_>,
    ) -> Result<Decimal, EntryRefusal> {
        if account.breaker_tripped {
            return Err(EntryRefusal::DailyHalt);
        }
        let drawdown =
            RiskPolicy::calculate_session_drawdown_pct(account.day_peak, account.equity)?;
        if drawdown >= self.policy.max_daily_drawdown_pct {
            return Err(EntryRefusal::DailyHalt);
        }
        let spec = self.specs.get(instrument);
        let market = spec.map_or(self.market, |spec| spec.market);
        if market == MarketType::Spot && intent.side == Side::Sell {
            return Err(EntryRefusal::SpotShortingDisallowed);
        }
        let open = account.book.len();
        if open >= self.limits.max_open_positions {
            return Err(RiskError::MaxPositionsExceeded {
                current: open,
                limit: self.limits.max_open_positions,
            }
            .into());
        }
        if let Some(limit) = self.limits.max_same_direction {
            let side = match intent.side {
                Side::Buy => PositionSide::Long,
                Side::Sell => PositionSide::Short,
            };
            let current = account.book.count_side(side);
            if current >= limit {
                return Err(RiskError::DirectionalLimitExceeded { current, limit }.into());
            }
        }
        let policy = match spec {
            Some(spec) => std::borrow::Cow::Owned(self.policy.for_instrument(spec)),
            None => std::borrow::Cow::Borrowed(&self.policy),
        };
        let quantity = policy.size_order(
            intent,
            intent.price,
            account.equity,
            self.risk_per_trade_pct,
            Decimal::ZERO,
            drawdown,
        )?;
        if let Some(limit_pct) = self.limits.max_gross_exposure_pct {
            let limit = account.equity * limit_pct;
            let new_notional = account.gross_exposure + quantity * intent.price;
            if new_notional > limit {
                return Err(RiskError::PositionLimitExceeded {
                    new_notional: new_notional.to_string(),
                    limit: limit.to_string(),
                }
                .into());
            }
        }
        let free_balance = account.equity - account.book.committed_margin();
        match self.margin {
            Some(margin) => Self::fund(&policy, quantity, intent.price, free_balance, margin),
            None => Ok(quantity),
        }
    }

    /// `quantity`, or the most the `balance` can margin when that is less, floored to a lot.
    /// Refused when what the balance funds is below the smallest order the exchange accepts.
    fn fund(
        policy: &RiskPolicy,
        quantity: Decimal,
        price: Decimal,
        balance: Decimal,
        margin: MarginRule,
    ) -> Result<Decimal, EntryRefusal> {
        let leverage = policy.max_leverage.trunc().to_u32().unwrap_or(1);
        // The worst fill the signal may get: slippage moves the price against the entry.
        let fill_price = price * (Decimal::ONE + margin.slippage_pct);
        let fundable = |quantity: Decimal| {
            let notional = quantity * fill_price;
            futures::minimum_leverage(
                notional,
                notional * margin.maintenance_rate,
                notional * margin.fee_rate,
                balance,
                leverage,
            )
            .is_some()
        };
        if fundable(quantity) {
            return Ok(quantity);
        }
        let step = policy.quantity_step_size;
        let cap = futures::maximum_quantity(
            fill_price,
            margin.maintenance_rate,
            margin.fee_rate,
            balance,
            leverage,
        );
        let mut capped = (cap / step).floor() * step;
        // The closed form divides by the leverage, which may round: step down until it holds.
        while capped > Decimal::ZERO && !fundable(capped) {
            capped -= step;
        }
        if capped < policy.min_order_quantity || capped * price < policy.min_order_notional {
            return Err(EntryRefusal::InsufficientMargin);
        }
        Ok(capped)
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
    fn a_loss_from_the_day_peak_at_the_limit_is_a_daily_halt_before_the_breaker_latches() {
        let policy = RiskPolicy::default();
        let limit = policy.max_daily_drawdown_pct;
        let equity = dec!(1000) * (Decimal::ONE - limit);
        let refusal = gate(MarketType::UsdMPerpetual)
            .admit(&long(), equity, dec!(1000), false)
            .unwrap_err();
        assert_eq!(refusal, EntryRefusal::DailyHalt);
    }

    /// A spot account (1x), the exchange's lot rules and a 0.8 % taker fee.
    fn spot_gate() -> EntryGate {
        let policy = RiskPolicy::default()
            .with_execution_rules(dec!(1), dec!(0.001), dec!(0.001), dec!(5))
            .unwrap();
        EntryGate::new(policy, dec!(0.01), MarketType::Spot).with_margin(MarginRule {
            maintenance_rate: Decimal::ZERO,
            fee_rate: dec!(0.008),
            slippage_pct: dec!(0.0005),
        })
    }

    fn long_with_stop(stop: Decimal) -> OrderIntent {
        OrderIntent::new(1, Side::Buy, dec!(100), stop, dec!(110), 5).unwrap()
    }

    #[test]
    fn an_entry_the_balance_funds_is_not_resized() {
        // A 5 % stop risks 1 % of 1000 on 2 units: 200 of notional, far inside the balance.
        let quantity = spot_gate()
            .admit(&long_with_stop(dec!(95)), dec!(1000), dec!(1000), false)
            .unwrap();
        assert_eq!(quantity, dec!(2));
    }

    #[test]
    fn an_entry_that_needs_the_whole_balance_is_sized_down_to_leave_its_fee() {
        // A 1 % stop risks 1 % of 1000 on 10 units: the whole balance, with nothing left for the
        // 0.8 % fee or the slippage.
        let quantity = spot_gate()
            .admit(&long_with_stop(dec!(99)), dec!(1000), dec!(1000), false)
            .unwrap();
        assert!(quantity < dec!(10), "sized down, got {quantity}");
        let fill = dec!(100) * dec!(1.0005);
        let cost = quantity * fill * dec!(1.008);
        assert!(cost <= dec!(1000), "the cost {cost} fits the balance");
        // One more lot would not fit.
        let more = (quantity + dec!(0.001)) * fill * dec!(1.008);
        assert!(more > dec!(1000));
    }

    #[test]
    fn an_entry_the_balance_cannot_fund_even_sized_down_is_refused_by_name() {
        // 5.02 funds 0.049 units after the fee: 4.9 of notional, under the 5 minimum.
        let refusal = spot_gate()
            .admit(&long_with_stop(dec!(99)), dec!(5.02), dec!(5.02), false)
            .unwrap_err();
        assert_eq!(refusal, EntryRefusal::InsufficientMargin);
        assert_eq!(refusal.kind(), "InsufficientMargin");
    }

    #[test]
    fn without_a_margin_rule_the_gate_sizes_by_risk_alone() {
        let policy = RiskPolicy::default()
            .with_execution_rules(dec!(1), dec!(0.001), dec!(0.001), dec!(5))
            .unwrap();
        let quantity = EntryGate::new(policy, dec!(0.01), MarketType::Spot)
            .admit(&long_with_stop(dec!(99)), dec!(1000), dec!(1000), false)
            .unwrap();
        assert_eq!(quantity, dec!(10));
    }

    fn held(side: PositionSide, notional: Decimal) -> crate::book::BookEntry {
        crate::book::BookEntry {
            position: crate::decision::PositionView {
                side,
                quantity: notional / dec!(100),
                entry_price: dec!(100),
            },
            margin: notional,
        }
    }

    fn account(book: &Book, gross: Decimal) -> AccountState<'_> {
        AccountState {
            equity: dec!(1000),
            day_peak: dec!(1000),
            breaker_tripped: false,
            book,
            gross_exposure: gross,
        }
    }

    #[test]
    fn the_portfolio_limits_refuse_one_position_too_many() {
        let eth = InstrumentId::from_static("ETHUSDT");
        let mut book = Book::new();
        book.open(
            InstrumentId::from_static("BTCUSDT"),
            held(PositionSide::Long, dec!(100)),
        );

        let single = gate(MarketType::UsdMPerpetual);
        assert_eq!(
            single.admit_to_book(&long(), &eth, &account(&book, dec!(100))),
            Err(RiskError::MaxPositionsExceeded {
                current: 1,
                limit: 1
            }
            .into())
        );

        let two = gate(MarketType::UsdMPerpetual).with_portfolio_limits(PortfolioLimits {
            max_open_positions: 2,
            ..PortfolioLimits::default()
        });
        assert!(two
            .admit_to_book(&long(), &eth, &account(&book, dec!(100)))
            .is_ok());

        let one_long = two.clone().with_portfolio_limits(PortfolioLimits {
            max_open_positions: 2,
            max_same_direction: Some(1),
            max_gross_exposure_pct: None,
        });
        assert_eq!(
            one_long
                .admit_to_book(&long(), &eth, &account(&book, dec!(100)))
                .map_err(|e| e.kind()),
            Err("DirectionalLimitExceeded")
        );
        assert!(one_long
            .admit_to_book(&short(), &eth, &account(&book, dec!(100)))
            .is_ok());
    }

    #[test]
    fn the_gross_exposure_limit_counts_the_open_positions() {
        let eth = InstrumentId::from_static("ETHUSDT");
        let book = Book::new();
        let capped = gate(MarketType::UsdMPerpetual).with_portfolio_limits(PortfolioLimits {
            max_open_positions: 3,
            max_same_direction: None,
            max_gross_exposure_pct: Some(dec!(1)),
        });
        // 1 % of 1000 over a 2-point stop is 5 units at 100: 500 of notional.
        assert!(capped
            .admit_to_book(&long(), &eth, &account(&book, dec!(500)))
            .is_ok());
        assert_eq!(
            capped
                .admit_to_book(&long(), &eth, &account(&book, dec!(501)))
                .map_err(|e| e.kind()),
            Err("PositionLimitExceeded")
        );
    }

    #[test]
    fn an_instrument_spec_sets_the_market_and_the_order_filters() {
        let eth = InstrumentId::from_static("ETHEUR");
        let spec = InstrumentSpec {
            id: eth.clone(),
            market: MarketType::Spot,
            min_quantity: dec!(10),
            quantity_step: dec!(1),
            min_notional: dec!(1),
        };
        let gate = gate(MarketType::UsdMPerpetual).with_instrument_specs([spec]);
        let book = Book::new();

        assert_eq!(
            gate.admit_to_book(&short(), &eth, &account(&book, dec!(0))),
            Err(EntryRefusal::SpotShortingDisallowed)
        );
        // Sized at 5 units, under the spec's minimum of 10.
        assert_eq!(
            gate.admit_to_book(&long(), &eth, &account(&book, dec!(0)))
                .map_err(|e| e.kind()),
            Err("OrderBelowMinLotSize")
        );
        // Another instrument keeps the gate's market and the policy's filters.
        let btc = InstrumentId::from_static("BTCUSDT");
        assert!(gate
            .admit_to_book(&short(), &btc, &account(&book, dec!(0)))
            .is_ok());
    }

    #[test]
    fn the_margin_is_posted_from_the_balance_the_open_positions_leave_free() {
        let margin = MarginRule {
            maintenance_rate: dec!(0),
            fee_rate: dec!(0),
            slippage_pct: dec!(0),
        };
        let policy = RiskPolicy::default()
            .with_execution_rules(dec!(1), dec!(0.001), dec!(0.001), dec!(1))
            .unwrap();
        let gate = EntryGate::new(policy, dec!(0.01), MarketType::Spot)
            .with_margin(margin)
            .with_portfolio_limits(PortfolioLimits {
                max_open_positions: 2,
                ..PortfolioLimits::default()
            });
        let mut book = Book::new();
        book.open(
            InstrumentId::from_static("BTCEUR"),
            held(PositionSide::Long, dec!(800)),
        );
        // 5 units at 100 ask for 500; only 200 is free, so 2 units are bought.
        let quantity = gate
            .admit_to_book(
                &long(),
                &InstrumentId::from_static("ETHEUR"),
                &account(&book, dec!(800)),
            )
            .unwrap();
        assert_eq!(quantity, dec!(2));
    }
}
