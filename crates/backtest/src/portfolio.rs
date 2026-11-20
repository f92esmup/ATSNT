//! The bar route on several instruments: one account, one cash balance, one position per
//! instrument.
//!
//! [`PortfolioEngine`] runs the rules of [`crate::BacktestEngine`] per instrument, each on its own
//! [`ExecutionCore`] over the shared cash balance, and the account rules once: the marked equity
//! sums every open position at its instrument's last price, the daily and monthly breakers watch
//! that equity, and the monthly kill switch closes every position. An entry is admitted by the
//! shared [`EntryGate`] against the whole book (positions and pending entries), so the portfolio
//! limits of [`domain::PortfolioLimits`] and the instrument specs apply. On one instrument it
//! reproduces [`crate::BacktestEngine`] exactly (`tests/portfolio_engine.rs`).

use std::collections::BTreeMap;

use domain::accounting::{utc_month_key, DailyBreaker};
use domain::{
    AccountState, Book, BookEntry, Decision, EntryGate, EntryRefusal, InstrumentId,
    InstrumentPosition, InstrumentSpec, OrderIntent, PositionSide, PositionView, Side,
    StrategyView, Trade, SIGNAL_EXIT,
};
use rust_decimal::Decimal;
use strategies::{MarketEvent, Strategy, StrategyScope};

use crate::config::BacktestConfig;
use crate::execution_core::{market_fill_price, ExecutionCore, PendingEntry, PendingEntryOutcome};
use crate::metrics::{BacktestMetrics, ClosedTrade, TradeExcursion};
use crate::price_event::PriceEvent;

/// The side of the market order that closes a position on `side`.
fn closing_side(side: PositionSide) -> Side {
    match side {
        PositionSide::Long => Side::Sell,
        PositionSide::Short => Side::Buy,
    }
}

/// One instrument's share of the account: its position, its pending orders and its last price.
#[derive(Debug, Clone, Default)]
struct Lane {
    core: ExecutionCore,
    /// End of the instrument's previous event, for funding settlements.
    last_end_ms: Option<i64>,
    /// Close of the instrument's latest event: its mark while other instruments trade.
    last_close: Option<Decimal>,
    open_excursion: Option<TradeExcursion>,
    pending_exit_due_ms: Option<i64>,
    /// Closed trades of the core already copied to the account's list.
    reported_trades: usize,
}

impl Lane {
    fn view(&self) -> (Option<PositionView>, bool) {
        (
            self.core.position().map(|position| PositionView {
                side: position.side,
                quantity: position.quantity,
                entry_price: position.entry_price,
            }),
            self.core.pending().is_some(),
        )
    }
}

/// Deterministic bar-route simulator of one account trading several instruments.
#[derive(Debug, Clone)]
pub struct PortfolioEngine {
    config: BacktestConfig,
    specs: Vec<InstrumentSpec>,
    equity: Decimal,
    peak_equity: Decimal,
    max_drawdown_amount: Decimal,
    max_drawdown_pct: Decimal,
    lanes: BTreeMap<InstrumentId, Lane>,
    current_day_utc: Option<i64>,
    daily: DailyBreaker,
    current_month_key: Option<i64>,
    monthly_open_equity: Decimal,
    halted: bool,
    rejected_orders: BTreeMap<String, usize>,
    /// Closed trades of every instrument, in the order they closed.
    closed: Vec<ClosedTrade>,
    decisions: Vec<Decision>,
    others: Vec<InstrumentPosition>,
}

impl PortfolioEngine {
    /// An engine for `config`, sizing each instrument with its spec in `specs` when it has one.
    pub fn new(config: BacktestConfig, specs: Vec<InstrumentSpec>) -> Self {
        let initial = config.initial_capital;
        Self {
            config,
            specs,
            equity: initial,
            peak_equity: initial,
            max_drawdown_amount: Decimal::ZERO,
            max_drawdown_pct: Decimal::ZERO,
            lanes: BTreeMap::new(),
            current_day_utc: None,
            daily: {
                let mut breaker = DailyBreaker::new(config.max_daily_drawdown_pct);
                breaker.observe(0, initial);
                breaker
            },
            current_month_key: None,
            monthly_open_equity: initial,
            halted: false,
            rejected_orders: BTreeMap::new(),
            closed: Vec::new(),
            decisions: Vec::new(),
            others: Vec::new(),
        }
    }

    /// Whether the monthly kill switch has halted the account for good.
    pub fn is_halted(&self) -> bool {
        self.halted
    }

    /// The closed trades so far, in the order they closed.
    pub fn closed_trades(&self) -> &[ClosedTrade] {
        &self.closed
    }

    /// The open position on `instrument`, if any.
    pub fn position(&self, instrument: &InstrumentId) -> Option<PositionView> {
        self.lanes.get(instrument).and_then(|lane| lane.view().0)
    }

    /// The strategy's view of `instrument` as owned parts: its position there, whether an entry
    /// is pending there, and the account's positions on the other instruments.
    pub fn view_parts(
        &self,
        instrument: &InstrumentId,
    ) -> (Option<PositionView>, bool, Vec<InstrumentPosition>) {
        let (position, entry_pending) =
            self.lanes.get(instrument).map_or((None, false), Lane::view);
        let others = self
            .lanes
            .iter()
            .filter(|(id, _)| *id != instrument)
            .filter_map(|(id, lane)| {
                Some(InstrumentPosition {
                    instrument: id.clone(),
                    position: lane.view().0?,
                })
            })
            .collect();
        (position, entry_pending, others)
    }

    /// Cash balance, without unrealized PnL.
    pub fn cash_equity(&self) -> Decimal {
        self.equity
    }

    /// Marked equity: cash plus every open position at its instrument's last price, with
    /// `instrument` marked at `price`.
    fn total_equity_with(&self, instrument: &InstrumentId, price: Decimal) -> Decimal {
        self.equity
            + self
                .lanes
                .iter()
                .filter_map(|(id, lane)| {
                    let position = lane.core.position()?;
                    let mark = if id == instrument {
                        price
                    } else {
                        lane.last_close.unwrap_or(position.entry_price)
                    };
                    Some(position.unrealized_pnl(mark))
                })
                .sum::<Decimal>()
    }

    /// Feeds one price event of `instrument`.
    ///
    /// The steps and their order are those of [`crate::BacktestEngine::process_event`], run on the
    /// instrument's position with the account's equity. `decide` receives the strategy's view of
    /// the instrument and of the account's other positions; a decision naming another instrument
    /// applies to that instrument, and an entry there is marked at its last price.
    pub fn process_event(
        &mut self,
        instrument: &InstrumentId,
        event: &PriceEvent,
        decide: &mut dyn FnMut(&StrategyView, &mut Vec<Decision>),
    ) {
        let bar = *event;
        let previous_end = {
            let lane = self.lanes.entry(instrument.clone()).or_default();
            lane.last_end_ms.replace(bar.end_ms)
        };
        if self.halted {
            return;
        }
        let day_key = bar.end_ms / 86_400_000;
        let mark_before = self.total_equity_with(instrument, bar.open);
        if self.current_day_utc != Some(day_key) {
            self.current_day_utc = Some(day_key);
            self.daily.observe(bar.end_ms, mark_before);
        }
        let month_key = utc_month_key(bar.end_ms);
        if self.current_month_key != Some(month_key) {
            self.current_month_key = Some(month_key);
            self.monthly_open_equity = mark_before;
        }

        let config = self.config;
        let Some(lane) = self.lanes.get_mut(instrument) else {
            return;
        };
        if let Some(previous_end) = previous_end {
            lane.core
                .settle_funding(previous_end, bar, &config, &mut self.equity);
        }
        let trades_before = lane.core.ledger().trades().len();
        match lane.core.step_pending_entry(bar, &config, &mut self.equity) {
            PendingEntryOutcome::ExpiredLimit => {
                count(&mut self.rejected_orders, "LimitTimeoutExpired")
            }
            PendingEntryOutcome::Refused(reason) => count(&mut self.rejected_orders, reason),
            _ => {}
        }
        if let Some(due_ms) = lane.pending_exit_due_ms {
            match lane.core.position().map(|position| position.side) {
                None => lane.pending_exit_due_ms = None,
                Some(side) if bar.start_ms >= due_ms => {
                    lane.pending_exit_due_ms = None;
                    let (price, slip) =
                        market_fill_price(closing_side(side), bar.open, config.slippage_pct);
                    lane.core.execute_close(
                        bar.start_ms,
                        price,
                        slip,
                        true,
                        SIGNAL_EXIT,
                        &config,
                        &mut self.equity,
                        &mut lane.open_excursion,
                    );
                }
                Some(_) => {}
            }
        }
        if lane.core.position().is_some() {
            lane.core
                .evaluate_exits(bar, &config, &mut self.equity, &mut lane.open_excursion);
        }

        // The monthly kill switch flattens every instrument and halts for good.
        let marked_close = self.total_equity_with(instrument, bar.close);
        if self.monthly_drawdown_pct(marked_close) >= config.max_monthly_drawdown_pct {
            self.halted = true;
            for (id, lane) in &mut self.lanes {
                let Some(side) = lane.core.position().map(|position| position.side) else {
                    continue;
                };
                let mark = if id == instrument {
                    bar.close
                } else {
                    lane.last_close.unwrap_or(bar.close)
                };
                let (price, slip) =
                    market_fill_price(closing_side(side), mark, config.slippage_pct);
                lane.core.execute_close(
                    bar.end_ms,
                    price,
                    slip,
                    true,
                    "MonthlyCircuitBreaker",
                    &config,
                    &mut self.equity,
                    &mut lane.open_excursion,
                );
            }
        }

        if !self.halted {
            self.decide(instrument, &bar, trades_before, decide);
        }

        let total = self.total_equity_with(instrument, bar.close);
        if total > self.peak_equity {
            self.peak_equity = total;
        }
        self.daily.observe(bar.end_ms, total);
        let dd_amount = self.peak_equity - total;
        if dd_amount > self.max_drawdown_amount {
            self.max_drawdown_amount = dd_amount;
        }
        let dd_pct = if self.peak_equity > Decimal::ZERO {
            dd_amount / self.peak_equity
        } else {
            Decimal::ZERO
        };
        if dd_pct > self.max_drawdown_pct {
            self.max_drawdown_pct = dd_pct;
        }

        if let Some(lane) = self.lanes.get_mut(instrument) {
            if lane.core.position().is_some() {
                let excursion = lane
                    .open_excursion
                    .get_or_insert_with(|| TradeExcursion::start(bar.end_ms, total));
                excursion.observe_equity(bar.end_ms, total);
                excursion.observe_drawdown(dd_amount, dd_pct);
            } else if lane.core.ledger().trades().len() > trades_before {
                if let Some(trade) = lane.core.ledger_mut().last_mut() {
                    trade.excursion.observe_drawdown(dd_amount, dd_pct);
                }
            }
            lane.last_close = Some(bar.close);
        }
        self.collect_closed_trades();
    }

    /// The decision step of [`Self::process_event`].
    fn decide(
        &mut self,
        instrument: &InstrumentId,
        bar: &PriceEvent,
        trades_before: usize,
        decide: &mut dyn FnMut(&StrategyView, &mut Vec<Decision>),
    ) {
        self.others.clear();
        for (id, lane) in &self.lanes {
            if id == instrument {
                continue;
            }
            if let (Some(position), _) = lane.view() {
                self.others.push(InstrumentPosition {
                    instrument: id.clone(),
                    position,
                });
            }
        }
        let (position, entry_pending) =
            self.lanes.get(instrument).map_or((None, false), Lane::view);
        let view = StrategyView {
            position,
            entry_pending,
            others: &self.others,
        };
        let mut decisions = std::mem::take(&mut self.decisions);
        decisions.clear();
        decide(&view, &mut decisions);

        let closed_by_time_barrier = self.lanes.get(instrument).is_some_and(|lane| {
            let trades = lane.core.ledger().trades();
            trades.len() > trades_before
                && trades
                    .last()
                    .is_some_and(|trade| trade.exit_reason == "TimeBarrier")
        });
        let due_ms = bar.end_ms.saturating_add(self.config.latency_ms);
        for decision in decisions.drain(..) {
            let target = decision.instrument_or(instrument).clone();
            match decision {
                Decision::Enter(intent) => {
                    let same = &target == instrument;
                    let lane = self.lanes.entry(target.clone()).or_default();
                    if lane.core.position().is_none()
                        && lane.core.pending().is_none()
                        && !(same && closed_by_time_barrier)
                    {
                        let mark = if same {
                            bar.close
                        } else {
                            lane.last_close.unwrap_or(intent.price)
                        };
                        self.submit_entry(&target, intent, mark, due_ms);
                    }
                }
                Decision::Exit { .. } => {
                    if let Some(lane) = self.lanes.get_mut(&target) {
                        if lane.core.position().is_some() && lane.pending_exit_due_ms.is_none() {
                            lane.pending_exit_due_ms = Some(due_ms);
                        }
                    }
                }
            }
        }
        self.decisions = decisions;
    }

    /// The account's book as the gate sees it: open positions and pending entries.
    fn book(&self) -> Book {
        let mut book = Book::new();
        let leverage = self.config.max_leverage.max(Decimal::ONE);
        for (id, lane) in &self.lanes {
            if let Some(position) = lane.core.position() {
                book.open(
                    id.clone(),
                    BookEntry {
                        position: PositionView {
                            side: position.side,
                            quantity: position.quantity,
                            entry_price: position.entry_price,
                        },
                        margin: lane.core.margin(),
                    },
                );
            } else if let Some(pending) = lane.core.pending() {
                let side = match pending.intent.side {
                    Side::Buy => PositionSide::Long,
                    Side::Sell => PositionSide::Short,
                };
                book.open(
                    id.clone(),
                    BookEntry {
                        position: PositionView {
                            side,
                            quantity: pending.quantity,
                            entry_price: pending.intent.price,
                        },
                        margin: pending.quantity * pending.intent.price / leverage,
                    },
                );
            }
        }
        book
    }

    /// Gates and sizes an entry on `instrument` against the whole book and queues it.
    fn submit_entry(
        &mut self,
        instrument: &InstrumentId,
        intent: OrderIntent,
        mark: Decimal,
        due_ms: i64,
    ) {
        let book = self.book();
        let gross = book.gross_exposure(|id| self.lanes.get(id).and_then(|lane| lane.last_close));
        let equity = self.total_equity_with(instrument, mark);
        let admitted = self
            .config
            .risk_policy()
            .map_err(EntryRefusal::from)
            .and_then(|policy| {
                EntryGate::new(
                    policy,
                    self.config.risk_per_trade_pct,
                    self.config.market_type,
                )
                .with_margin(self.config.margin_rule())
                .with_portfolio_limits(self.config.portfolio_limits())
                .with_instrument_specs(self.specs.iter().cloned())
                .admit_to_book(
                    &intent,
                    instrument,
                    &AccountState {
                        equity,
                        day_peak: self.daily.peak(),
                        breaker_tripped: self.daily.is_tripped(),
                        book: &book,
                        gross_exposure: gross,
                    },
                )
            });
        match admitted {
            Ok(quantity) => {
                if let Some(lane) = self.lanes.get_mut(instrument) {
                    lane.core
                        .submit_due(PendingEntry::new(intent, quantity), due_ms);
                }
            }
            Err(error) => count(&mut self.rejected_orders, error.kind()),
        }
    }

    fn monthly_drawdown_pct(&self, marked: Decimal) -> Decimal {
        if self.monthly_open_equity > Decimal::ZERO {
            (self.monthly_open_equity - marked).max(Decimal::ZERO) / self.monthly_open_equity
        } else {
            Decimal::ZERO
        }
    }

    /// Copies the trades the lanes closed since the last call to the account's list, labelled
    /// with their instrument, in instrument order within one event.
    fn collect_closed_trades(&mut self) {
        for (id, lane) in &mut self.lanes {
            let trades = lane.core.ledger().trades();
            for trade in &trades[lane.reported_trades..] {
                self.closed.push(ClosedTrade {
                    instrument: Some(id.clone()),
                    ..trade.clone()
                });
            }
            lane.reported_trades = trades.len();
        }
    }

    /// Closes what is still open at each instrument's last price as `EndOfSimulation` (when
    /// `close_open` is set) and computes the metrics of the whole account.
    pub fn finish_with_trades(mut self, close_open: bool) -> (BacktestMetrics, Vec<ClosedTrade>) {
        if close_open {
            let config = self.config;
            for lane in self.lanes.values_mut() {
                let (Some(mark), Some(exit_time)) = (lane.last_close, lane.last_end_ms) else {
                    continue;
                };
                if lane.core.position().is_some() {
                    lane.core.execute_close(
                        exit_time,
                        mark,
                        Decimal::ZERO,
                        true,
                        "EndOfSimulation",
                        &config,
                        &mut self.equity,
                        &mut lane.open_excursion,
                    );
                }
            }
            self.collect_closed_trades();
        }
        let mut metrics = BacktestMetrics::calculate(
            self.config.initial_capital,
            &self.closed,
            self.max_drawdown_amount,
            self.max_drawdown_pct,
        );
        metrics.halted = self.halted;
        metrics.rejected_orders = self.rejected_orders;
        (metrics, self.closed)
    }
}

fn count(rejected: &mut BTreeMap<String, usize>, reason: &str) {
    *rejected.entry(reason.to_string()).or_insert(0) += 1;
}

/// Net PnL and number of closed trades per instrument, in instrument order.
pub fn results_by_instrument(trades: &[ClosedTrade]) -> BTreeMap<InstrumentId, (usize, Decimal)> {
    let mut by_instrument: BTreeMap<InstrumentId, (usize, Decimal)> = BTreeMap::new();
    for trade in trades {
        let id = trade
            .instrument
            .clone()
            .unwrap_or_else(|| domain::UNNAMED_INSTRUMENT.clone());
        let entry = by_instrument.entry(id).or_default();
        entry.0 += 1;
        entry.1 += trade.pnl_net;
    }
    by_instrument
}

/// The strategies of a portfolio run: one instance per instrument for a
/// [`StrategyScope::PerInstrument`] strategy, one for all of them for a
/// [`StrategyScope::CrossSectional`] one.
pub enum PortfolioStrategies<S> {
    /// One instance per instrument; each decides on its own instrument only.
    PerInstrument(BTreeMap<InstrumentId, S>),
    /// One instance that sees every instrument and decides on any.
    CrossSectional(S),
}

impl<S: Strategy> PortfolioStrategies<S> {
    /// Builds the strategies for `instruments`, asking `build` for as many instances as the
    /// strategy's scope needs.
    pub fn new<E>(
        instruments: &[InstrumentId],
        mut build: impl FnMut() -> Result<S, E>,
    ) -> Result<Self, E> {
        let first = build()?;
        match first.scope() {
            StrategyScope::CrossSectional => Ok(Self::CrossSectional(first)),
            StrategyScope::PerInstrument => {
                let mut instances = BTreeMap::new();
                let mut first = Some(first);
                for instrument in instruments {
                    let instance = match first.take() {
                        Some(instance) => instance,
                        None => build()?,
                    };
                    instances.insert(instrument.clone(), instance);
                }
                Ok(Self::PerInstrument(instances))
            }
        }
    }

    /// Feeds one trade of `instrument` to `engine` and to the strategy that decides on it. A
    /// per-instrument strategy acts only on its own instrument: a decision naming another is
    /// dropped. A trade of an instrument without a strategy is still marked and settled.
    pub fn on_trade(
        &mut self,
        engine: &mut PortfolioEngine,
        instrument: &InstrumentId,
        trade: &Trade,
    ) {
        let event = MarketEvent::trade(instrument, trade);
        let price = PriceEvent::from(trade);
        match self {
            Self::CrossSectional(strategy) => {
                engine.process_event(instrument, &price, &mut |view, out| {
                    strategy.on_event(&event, view, out)
                });
            }
            Self::PerInstrument(instances) => {
                let mut strategy = instances.get_mut(instrument);
                engine.process_event(instrument, &price, &mut |view, out| {
                    if let Some(strategy) = strategy.as_deref_mut() {
                        strategy.on_event(&event, view, out);
                        out.retain(|decision| {
                            decision
                                .instrument()
                                .map_or(true, |named| named == instrument)
                        });
                    }
                });
            }
        }
    }

    /// Tells the strategy that decides on `instrument` of a funding settlement there. It informs:
    /// whatever the strategy decides on it is ignored.
    pub fn on_funding(
        &mut self,
        engine: &PortfolioEngine,
        instrument: &InstrumentId,
        time_ms: i64,
        rate: Decimal,
    ) {
        let (position, entry_pending, others) = engine.view_parts(instrument);
        let view = StrategyView {
            position,
            entry_pending,
            others: &others,
        };
        let event = MarketEvent::funding(instrument, time_ms, rate);
        let mut ignored = Vec::new();
        let strategy = match self {
            Self::CrossSectional(strategy) => Some(strategy),
            Self::PerInstrument(instances) => instances.get_mut(instrument),
        };
        if let Some(strategy) = strategy {
            strategy.on_event(&event, &view, &mut ignored);
        }
    }

    /// The audits of the instances, by instrument for per-instrument strategies.
    pub fn audit(&self) -> Option<serde_json::Value> {
        match self {
            Self::CrossSectional(strategy) => strategy.audit(),
            Self::PerInstrument(instances) => {
                let audits: serde_json::Map<String, serde_json::Value> = instances
                    .iter()
                    .filter_map(|(id, strategy)| Some((id.to_string(), strategy.audit()?)))
                    .collect();
                (!audits.is_empty()).then_some(serde_json::Value::Object(audits))
            }
        }
    }
}
