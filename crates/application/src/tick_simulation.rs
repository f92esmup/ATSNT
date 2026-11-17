//! Tick-level simulation: the live session core running on a simulated venue.
//!
//! Where the bar engine fills at bar granularity and is cheap enough for
//! hyper-parameter search, this driver replays every trade through a [`ports::TapeVenue`] and
//! the same [`crate::session::TradingSession`] that runs live: orders travel with latency,
//! market fills walk the tape and exchange-held exits fire on trigger prices. The simulated
//! exchange comes from the composition. It is meant for validating finalists
//! and for paper trading, not for search loops.
//!
//! A position still open when the data ends is left open (see [`TickSimulation::open_position`])
//! and is not counted in the metrics, unlike the bar engine, which marks it out.

use domain::futures::FundingSchedule;
use domain::{EntryGate, RiskError, Side, Trade};
use ports::TapeVenue;
use rust_decimal::Decimal;
use strategies::Strategy;

use crate::config::BacktestConfig;
use crate::driver::SessionDriver;
use crate::metrics::{BacktestMetrics, ClosedTrade};
use crate::session::{SessionConfig, SessionEvent, TradingSession};

/// Milliseconds a market entry may wait for a terminal status before the session cancels it.
const ENTRY_TIMEOUT_MS: i64 = 30_000;

/// Milliseconds between emergency-close attempts while halting.
const HALT_RETRY_MS: i64 = 5_000;

/// Parameters of a tick-level simulation.
#[derive(Debug, Clone)]
pub struct TickSimulationConfig {
    /// Capital, fees, risk and execution rules shared with the bar-level engine.
    pub backtest: BacktestConfig,
    /// Instrument traded.
    pub symbol: String,
    /// Milliseconds an order takes to reach the market. It configures the simulated venue that
    /// the composition builds; [`TickSimulation::new`] takes its venue as given.
    pub latency_ms: i64,
    /// Funding rates charged at each settlement; `FundingSchedule::constant` of the
    /// configuration's rate unless a recorded history is supplied. Like `latency_ms`, it
    /// configures the simulated venue built by the composition.
    pub funding: FundingSchedule,
}

/// Why a tick simulation could not be built.
#[derive(Debug, thiserror::Error)]
pub enum TickSimulationError {
    /// The risk limits are invalid.
    #[error("invalid risk policy: {0}")]
    Risk(#[from] RiskError),
}

/// What one replayed trade produced.
#[derive(Debug, Clone, PartialEq)]
pub struct TickStep {
    /// Everything the session did because of the trade.
    pub events: Vec<SessionEvent>,
}

/// One strategy replayed trade by trade against the account of a tape venue.
pub struct TickSimulation<S: Strategy, V: TapeVenue> {
    /// Signals the gate refused, per reason (see `BacktestMetrics::rejected_orders`).
    rejected_orders: std::collections::BTreeMap<String, usize>,
    initial_capital: Decimal,
    driver: SessionDriver<S>,
    venue: V,
}

impl<S: Strategy, V: TapeVenue> TickSimulation<S, V> {
    /// Creates a simulation of `strategy` on `venue`, which must start flat holding the configured
    /// capital. the composition builds both from one configuration.
    pub fn new(
        config: TickSimulationConfig,
        strategy: S,
        venue: V,
    ) -> Result<Self, TickSimulationError> {
        let backtest = &config.backtest;
        let gate = EntryGate::new(
            backtest.risk_policy()?,
            backtest.risk_per_trade_pct,
            backtest.market_type,
        );
        let session = TradingSession::new(
            SessionConfig {
                symbol: config.symbol.clone(),
                initial_equity: backtest.initial_capital,
                fallback_fee_pct: backtest.taker_fee_pct,
                max_monthly_drawdown_pct: backtest.max_monthly_drawdown_pct,
                halt_retry_ms: HALT_RETRY_MS,
                entry_timeout_ms: ENTRY_TIMEOUT_MS,
                gate,
            },
            strategy,
        );
        let driver = SessionDriver::new(session);
        Ok(Self {
            rejected_orders: std::collections::BTreeMap::new(),
            initial_capital: backtest.initial_capital,
            driver,
            venue,
        })
    }

    /// Replays one market trade and returns what the session did because of it.
    ///
    /// The venue executes the print first; the session absorbs the funding, executions and new
    /// balance it produced, and then the shared [`SessionDriver`] runs the loop live runs: the
    /// monthly breaker, the bar sampler and, last, the strategy.
    pub async fn on_trade(&mut self, trade: &Trade) -> TickStep {
        let mut events = Vec::new();
        let reports = self.venue.on_trade(trade);
        // Funding settled before the print traded, on the position held until then.
        let funding = self.venue.take_funding();
        for payment in &funding {
            self.driver.on_funding(payment.amount);
        }
        if !funding.is_empty() {
            self.driver.on_wallet_balance(self.venue.wallet_balance());
        }
        for report in reports {
            events.extend(self.driver.on_execution(&self.venue, &report).await);
            self.driver.on_wallet_balance(self.venue.wallet_balance());
        }
        let step = self.driver.on_market_trade(&self.venue, trade).await;
        events.extend(step.into_events());
        for event in &events {
            if let SessionEvent::EntryRefused(refusal) = event {
                *self
                    .rejected_orders
                    .entry(refusal.kind().to_string())
                    .or_insert(0) += 1;
            }
        }
        TickStep { events }
    }

    /// Warms the strategy up on historical trades before the replay starts; nothing trades.
    /// Returns how many trades the strategy saw.
    pub fn warm_up_trades(&mut self, trades: impl IntoIterator<Item = Trade>) -> usize {
        self.driver.warm_up_trades(trades)
    }

    /// Trades closed so far, oldest first.
    pub fn closed_trades(&self) -> &[ClosedTrade] {
        self.driver.session().closed_trades()
    }

    /// Side, quantity and average price of the position still open, if any.
    pub fn open_position(&self) -> Option<(Side, Decimal, Decimal)> {
        self.venue.position()
    }

    /// Trades replayed so far.
    pub fn ticks_processed(&self) -> u64 {
        self.driver.ticks()
    }

    /// Strategy being executed by the simulation.
    pub fn strategy(&self) -> &S {
        self.driver.strategy()
    }

    /// Strategy-specific audit report, if any.
    pub fn strategy_audit(&self) -> Option<serde_json::Value> {
        self.driver.strategy().audit()
    }

    /// Whether the monthly breaker or an emergency halt stopped the run.
    pub fn is_halted(&self) -> bool {
        self.driver.session().is_halted()
    }

    /// Wallet balance of the simulated account.
    pub fn wallet_balance(&self) -> Decimal {
        self.venue.wallet_balance()
    }

    /// Ends the run and compiles its metrics from the closed trades.
    pub fn finish(self) -> (BacktestMetrics, Vec<ClosedTrade>) {
        let halted = self.driver.session().is_halted();
        let trades = self.driver.session().closed_trades().to_vec();
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
