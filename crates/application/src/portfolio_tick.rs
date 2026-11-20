//! The tick route on several instruments: one trading session per instrument on one venue
//! account.
//!
//! Each instrument runs the live [`TradingSession`] with its own instance of a per-instrument
//! strategy. The sessions share the venue's wallet balance, and before each trade the session of
//! the trade's instrument is told the state of the whole account ([`AccountContext`]): its marked
//! equity, which the breakers watch, and the other instruments' positions and pending entries,
//! which the gate judges a new entry against with the portfolio limits.

use std::collections::BTreeMap;

use domain::{Book, EntryGate, InstrumentId, InstrumentPosition, Side, Trade};
use ports::MultiTapeVenue;
use rust_decimal::Decimal;
use strategies::Strategy;

use crate::driver::SessionDriver;
use crate::metrics::{BacktestMetrics, ClosedTrade};
use crate::session::{AccountContext, SessionConfig, SessionEvent, TradingSession};
use crate::tick_simulation::{TickSimulationConfig, TickSimulationError, TickStep};

/// Milliseconds a market entry may wait for a terminal status before the session cancels it.
const ENTRY_TIMEOUT_MS: i64 = 30_000;

/// Milliseconds between emergency-close attempts while halting.
const HALT_RETRY_MS: i64 = 5_000;

/// Per-instrument strategies replayed trade by trade against one multi-symbol venue account.
pub struct PortfolioTickSimulation<S: Strategy, V: MultiTapeVenue> {
    drivers: BTreeMap<InstrumentId, SessionDriver<S>>,
    venue: V,
    /// Last price of every instrument seen so far.
    marks: BTreeMap<InstrumentId, Decimal>,
    rejected_orders: BTreeMap<String, usize>,
    initial_capital: Decimal,
    max_leverage: Decimal,
}

impl<S: Strategy, V: MultiTapeVenue> PortfolioTickSimulation<S, V> {
    /// A simulation of `strategies`, one per instrument, on `venue`, which must start flat
    /// holding the configured capital. The configuration's `symbol` is not used: each session
    /// trades its instrument.
    pub fn new(
        config: TickSimulationConfig,
        strategies: BTreeMap<InstrumentId, S>,
        venue: V,
    ) -> Result<Self, TickSimulationError> {
        let backtest = &config.backtest;
        let gate = EntryGate::new(
            backtest.risk_policy()?,
            backtest.risk_per_trade_pct,
            backtest.market_type,
        )
        .with_margin(backtest.margin_rule())
        .with_portfolio_limits(backtest.portfolio_limits());
        let drivers = strategies
            .into_iter()
            .map(|(instrument, strategy)| {
                let session = TradingSession::new(
                    SessionConfig {
                        symbol: instrument.to_string(),
                        initial_equity: backtest.initial_capital,
                        fallback_fee_pct: backtest.taker_fee_pct,
                        max_monthly_drawdown_pct: backtest.max_monthly_drawdown_pct,
                        halt_retry_ms: HALT_RETRY_MS,
                        entry_timeout_ms: ENTRY_TIMEOUT_MS,
                        gate: gate.clone(),
                    },
                    strategy,
                );
                (instrument, SessionDriver::new(session))
            })
            .collect();
        Ok(Self {
            drivers,
            venue,
            marks: BTreeMap::new(),
            rejected_orders: BTreeMap::new(),
            initial_capital: backtest.initial_capital,
            max_leverage: backtest.max_leverage,
        })
    }

    /// The state of the account as the session of `instrument` must see it.
    fn account_for(&self, instrument: &InstrumentId) -> AccountContext {
        let mut context = AccountContext {
            marked_equity: self.venue.wallet_balance(),
            ..AccountContext::default()
        };
        let mut book = Book::new();
        for (id, driver) in &self.drivers {
            let session = driver.session();
            let mark = self.marks.get(id).copied();
            if let Some((side, entry_price, quantity)) = session.open_position() {
                let mark = mark.unwrap_or(entry_price);
                let direction = match side {
                    Side::Buy => Decimal::ONE,
                    Side::Sell => -Decimal::ONE,
                };
                context.marked_equity += (mark - entry_price) * quantity * direction;
            }
            if id == instrument {
                continue;
            }
            let Some(entry) = session.book_entry(self.max_leverage) else {
                continue;
            };
            if entry.position.quantity > Decimal::ZERO {
                context.gross_exposure +=
                    entry.position.quantity * mark.unwrap_or(entry.position.entry_price);
                context.others.push(InstrumentPosition {
                    instrument: id.clone(),
                    position: entry.position,
                });
            }
            book.open(id.clone(), entry);
        }
        context.book = book;
        context
    }

    /// Replays one market trade of `instrument`.
    ///
    /// The venue executes the print on that instrument's book; the instrument's session absorbs
    /// the funding and executions it produced, every session learns the new wallet balance, and
    /// then the instrument's session runs the live loop with the account's state. A trade of an
    /// instrument without a session still moves the venue's market.
    pub async fn on_trade(&mut self, instrument: &InstrumentId, trade: &Trade) -> TickStep {
        let symbol = instrument.as_str();
        let reports = self.venue.on_trade_of(symbol, trade);
        let funding = self.venue.take_funding_of(symbol);
        self.marks.insert(instrument.clone(), trade.price);
        let mut events = Vec::new();
        let wallet_moved = !funding.is_empty() || !reports.is_empty();
        if let Some(driver) = self.drivers.get_mut(instrument) {
            // The same order as the single-instrument simulation: funding, then each execution,
            // each followed by the balance it left.
            for payment in &funding {
                driver.on_funding(payment.amount);
            }
            if !funding.is_empty() {
                driver.on_wallet_balance(self.venue.wallet_balance());
            }
            for report in &reports {
                events.extend(driver.on_execution(&self.venue, report).await);
                driver.on_wallet_balance(self.venue.wallet_balance());
            }
        }
        if wallet_moved {
            let wallet = self.venue.wallet_balance();
            for (id, driver) in &mut self.drivers {
                if id != instrument {
                    driver.on_wallet_balance(wallet);
                }
            }
        }
        let context = self.account_for(instrument);
        if let Some(driver) = self.drivers.get_mut(instrument) {
            driver.session_mut().set_account(context);
            let step = driver.on_market_trade(&self.venue, trade).await;
            events.extend(step.into_events());
        }
        for event in &events {
            let reason = match event {
                SessionEvent::EntryRefused(refusal) => refusal.kind(),
                SessionEvent::EntryFailed(_) => domain::metrics::VENUE_REJECTED,
                _ => continue,
            };
            *self.rejected_orders.entry(reason.to_string()).or_insert(0) += 1;
        }
        TickStep { events }
    }

    /// Side, quantity and average price of the position open on `instrument`, if any.
    pub fn open_position(&self, instrument: &InstrumentId) -> Option<(Side, Decimal, Decimal)> {
        self.venue.position_of(instrument.as_str())
    }

    /// Whether the session of any instrument has halted.
    pub fn is_halted(&self) -> bool {
        self.drivers
            .values()
            .any(|driver| driver.session().is_halted())
    }

    /// Wallet balance of the simulated account.
    pub fn wallet_balance(&self) -> Decimal {
        self.venue.wallet_balance()
    }

    /// The audits of the strategies, by instrument.
    pub fn strategy_audits(&self) -> BTreeMap<InstrumentId, serde_json::Value> {
        self.drivers
            .iter()
            .filter_map(|(id, driver)| Some((id.clone(), driver.strategy().audit()?)))
            .collect()
    }

    /// Ends the run and compiles its metrics from every instrument's closed trades, in the
    /// order they closed. Positions still open are left out, as on the single-instrument route.
    pub fn finish(self) -> (BacktestMetrics, Vec<ClosedTrade>) {
        let halted = self.is_halted();
        let mut trades: Vec<ClosedTrade> = self
            .drivers
            .iter()
            .flat_map(|(id, driver)| {
                driver
                    .session()
                    .closed_trades()
                    .iter()
                    .map(move |trade| ClosedTrade {
                        instrument: Some(id.clone()),
                        ..trade.clone()
                    })
            })
            .collect();
        trades.sort_by(|a, b| (a.exit_time, &a.instrument).cmp(&(b.exit_time, &b.instrument)));
        let max_drawdown_amount = trades
            .iter()
            .map(|trade| trade.excursion.max_drawdown_amount)
            .max()
            .unwrap_or(Decimal::ZERO);
        let max_drawdown_pct = trades
            .iter()
            .map(|trade| trade.excursion.max_drawdown_pct)
            .max()
            .unwrap_or(Decimal::ZERO);
        let mut metrics = BacktestMetrics::calculate(
            self.initial_capital,
            &trades,
            max_drawdown_amount,
            max_drawdown_pct,
        );
        metrics.halted = halted;
        metrics.rejected_orders = self.rejected_orders;
        (metrics, trades)
    }
}
