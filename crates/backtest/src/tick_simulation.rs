//! Tick-level simulation: the live session core running on a simulated venue.
//!
//! Where [`crate::BacktestEngine`] fills at bar granularity and is cheap enough for
//! hyper-parameter search, this driver replays every trade through [`adapters::SimulatedVenue`]
//! and the same [`crate::session::TradingSession`] that runs live: orders travel with latency,
//! market fills walk the tape and exchange-held exits fire on trigger prices. It is meant for
//! validating finalists and for paper trading, not for search loops.
//!
//! A position still open when the data ends is left open (see [`TickSimulation::open_position`])
//! and is not counted in the metrics, unlike the bar engine, which marks it out.

use adapters::{SimulatedVenue, SimulatedVenueConfig};
use domain::{DollarBar, DollarBarAggregator, DomainError, RiskError, Side, Trade};
use rust_decimal::Decimal;
use strategies::Strategy;

use crate::config::BacktestConfig;
use crate::metrics::{BacktestMetrics, ClosedTrade};
use crate::session::{SessionConfig, SessionEvent, TradingSession};

/// Milliseconds between emergency-close attempts while halting.
const HALT_RETRY_MS: i64 = 5_000;

/// Parameters of a tick-level simulation.
#[derive(Debug, Clone)]
pub struct TickSimulationConfig {
    /// Capital, fees, risk and execution rules shared with the bar-level engine.
    pub backtest: BacktestConfig,
    /// Instrument traded.
    pub symbol: String,
    /// Dollar-bar threshold the strategy samples at.
    pub dollar_bar_threshold: Decimal,
    /// Milliseconds an order takes to reach the market.
    pub latency_ms: i64,
}

/// Why a tick simulation could not be built.
#[derive(Debug, thiserror::Error)]
pub enum TickSimulationError {
    /// The dollar-bar threshold is invalid.
    #[error("invalid dollar bar threshold: {0}")]
    Threshold(#[from] DomainError),
    /// The risk limits are invalid.
    #[error("invalid risk policy: {0}")]
    Risk(#[from] RiskError),
}

/// What one replayed trade produced.
#[derive(Debug, Clone, PartialEq)]
pub struct TickStep {
    /// The dollar bar this trade completed, if any.
    pub bar: Option<DollarBar>,
    /// Everything the session did because of the trade.
    pub events: Vec<SessionEvent>,
}

/// One strategy replayed trade by trade against a simulated exchange account.
pub struct TickSimulation<S: Strategy> {
    config: TickSimulationConfig,
    session: TradingSession<S>,
    venue: SimulatedVenue,
    aggregator: DollarBarAggregator,
    ticks: u64,
    bars: u64,
}

impl<S: Strategy> TickSimulation<S> {
    /// Creates a flat simulation holding the configured capital.
    pub fn new(config: TickSimulationConfig, strategy: S) -> Result<Self, TickSimulationError> {
        let backtest = &config.backtest;
        let venue = SimulatedVenue::new(SimulatedVenueConfig {
            initial_equity: backtest.initial_capital,
            taker_fee_pct: backtest.taker_fee_pct,
            latency_ms: config.latency_ms,
            risk_per_trade_pct: backtest.risk_per_trade_pct,
            risk_policy: backtest.risk_policy()?,
        });
        let session = TradingSession::new(
            SessionConfig {
                symbol: config.symbol.clone(),
                initial_equity: backtest.initial_capital,
                fallback_fee_pct: backtest.taker_fee_pct,
                max_monthly_drawdown_pct: backtest.max_monthly_drawdown_pct,
                halt_retry_ms: HALT_RETRY_MS,
            },
            strategy,
        );
        let aggregator = DollarBarAggregator::new(config.dollar_bar_threshold)?;
        Ok(Self {
            config,
            session,
            venue,
            aggregator,
            ticks: 0,
            bars: 0,
        })
    }

    /// Replays one market trade and returns what the session did because of it.
    ///
    /// The order mirrors the live loop: the venue executes first, the session absorbs the
    /// executions and the new balance, then the monthly breaker runs and finally a finished
    /// bar reaches the strategy.
    pub async fn on_trade(&mut self, trade: &Trade) -> TickStep {
        self.ticks += 1;
        let mut events = Vec::new();
        for report in self.venue.on_trade(trade) {
            events.extend(self.session.on_execution(&self.venue, &report).await);
            self.session.on_wallet_balance(self.venue.wallet_balance());
        }
        events.extend(
            self.session
                .on_trade(&self.venue, trade.timestamp, trade.price)
                .await,
        );
        let bar = self.aggregator.process_trade(trade);
        if let Some(bar) = &bar {
            self.bars += 1;
            events.extend(self.session.on_bar(&self.venue, bar).await);
        }
        TickStep { bar, events }
    }

    /// Trades closed so far, oldest first.
    pub fn closed_trades(&self) -> &[ClosedTrade] {
        self.session.closed_trades()
    }

    /// Side, quantity and average price of the position still open, if any.
    pub fn open_position(&self) -> Option<(Side, Decimal, Decimal)> {
        self.venue.position()
    }

    /// Trades replayed so far.
    pub fn ticks_processed(&self) -> u64 {
        self.ticks
    }

    /// Dollar bars the strategy has seen so far.
    pub fn bars_processed(&self) -> u64 {
        self.bars
    }

    /// Whether the monthly breaker or an emergency halt stopped the run.
    pub fn is_halted(&self) -> bool {
        self.session.is_halted()
    }

    /// Wallet balance of the simulated account.
    pub fn wallet_balance(&self) -> Decimal {
        self.venue.wallet_balance()
    }

    /// Ends the run and compiles its metrics from the closed trades.
    pub fn finish(self) -> (BacktestMetrics, Vec<ClosedTrade>) {
        let halted = self.session.is_halted();
        let trades = self.session.closed_trades().to_vec();
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
            self.config.backtest.initial_capital,
            &trades,
            max_drawdown_amount,
            max_drawdown_pct,
        );
        metrics.halted = halted;
        (metrics, trades)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::OrderIntent;
    use rust_decimal_macros::dec;
    use std::collections::VecDeque;
    use strategies::MarketType;

    /// Emits one scripted decision per bar.
    struct Scripted {
        decisions: VecDeque<Option<OrderIntent>>,
    }

    impl Strategy for Scripted {
        fn name(&self) -> &str {
            "scripted"
        }

        fn compatible_market_types(&self) -> &'static [MarketType] {
            &[MarketType::UsdMPerpetual]
        }

        fn on_bar(&mut self, _: &DollarBar) -> Option<OrderIntent> {
            self.decisions.pop_front().flatten()
        }

        fn reset(&mut self) {}
    }

    fn long_signal() -> Option<OrderIntent> {
        Some(OrderIntent::new(1, Side::Buy, dec!(100), dec!(98), dec!(104), 5).unwrap())
    }

    fn simulation(decisions: Vec<Option<OrderIntent>>) -> TickSimulation<Scripted> {
        let config = TickSimulationConfig {
            backtest: BacktestConfig {
                initial_capital: dec!(1000),
                taker_fee_pct: dec!(0.001),
                ..BacktestConfig::default()
            },
            symbol: "BTCUSDT".to_string(),
            dollar_bar_threshold: dec!(2000),
            latency_ms: 0,
        };
        TickSimulation::new(
            config,
            Scripted {
                decisions: decisions.into(),
            },
        )
        .unwrap()
    }

    fn print(timestamp: i64, price: Decimal, quantity: Decimal, side: Side) -> Trade {
        Trade::new(timestamp, price, quantity, side).unwrap()
    }

    /// Two $1000 prints close the first bar; the entry fills on the next buyer print.
    async fn replay(simulation: &mut TickSimulation<Scripted>, ticks: &[Trade]) {
        for tick in ticks {
            simulation.on_trade(tick).await;
        }
    }

    fn entry_ticks() -> Vec<Trade> {
        vec![
            print(1_000, dec!(100), dec!(10), Side::Sell),
            print(1_001, dec!(100), dec!(10), Side::Sell),
            print(1_002, dec!(100.2), dec!(10), Side::Buy),
        ]
    }

    #[tokio::test]
    async fn a_signal_becomes_a_position_filled_on_the_tape() {
        let mut simulation = simulation(vec![long_signal()]);

        replay(&mut simulation, &entry_ticks()).await;

        assert_eq!(simulation.bars_processed(), 1);
        let (side, quantity, entry) = simulation.open_position().unwrap();
        assert_eq!(side, Side::Buy);
        assert_eq!(quantity, dec!(5));
        assert_eq!(entry, dec!(100.2));
        // 5 units at 100.2 with a 0.1% taker fee.
        assert_eq!(simulation.wallet_balance(), dec!(999.499));
    }

    #[tokio::test]
    async fn a_take_profit_closes_the_trade_with_real_fills_and_fees() {
        let mut simulation = simulation(vec![long_signal()]);
        let mut ticks = entry_ticks();
        ticks.extend([
            print(1_003, dec!(103), dec!(10), Side::Buy),
            print(1_004, dec!(104), dec!(1), Side::Buy),
            print(1_005, dec!(103.9), dec!(10), Side::Sell),
        ]);

        replay(&mut simulation, &ticks).await;

        assert!(simulation.open_position().is_none());
        let (metrics, trades) = simulation.finish();
        assert_eq!(trades.len(), 1);
        let trade = &trades[0];
        assert_eq!(trade.exit_reason, "TakeProfit");
        assert_eq!(trade.entry_price, dec!(100.2));
        assert_eq!(trade.exit_price, dec!(103.9));
        assert_eq!(trade.quantity, dec!(5));
        // Commissions on both legs: 5 * 100.2 * 0.001 + 5 * 103.9 * 0.001.
        assert_eq!(trade.fees_paid, dec!(1.0205));
        assert_eq!(metrics.total_trades, 1);
        assert_eq!(metrics.exit_reason_counts.get("TakeProfit"), Some(&1));
    }

    #[tokio::test]
    async fn a_stop_loss_pays_the_slippage_the_tape_charged_after_it_armed() {
        let mut simulation = simulation(vec![long_signal()]);
        let mut ticks = entry_ticks();
        ticks.extend([
            print(1_003, dec!(97.9), dec!(10), Side::Sell),
            print(1_004, dec!(97.4), dec!(10), Side::Sell),
        ]);

        replay(&mut simulation, &ticks).await;

        let (_, trades) = simulation.finish();
        assert_eq!(trades[0].exit_reason, "StopLoss");
        // The stop armed at 97.9 and filled on the next sell print, 0.6 below the trigger.
        assert_eq!(trades[0].exit_price, dec!(97.4));
        assert!(trades[0].pnl_net < Decimal::ZERO);
    }

    #[tokio::test]
    async fn the_time_barrier_closes_at_market_and_is_labelled_as_such() {
        let intent = OrderIntent::new(1, Side::Buy, dec!(100), dec!(98), dec!(104), 2).unwrap();
        let mut simulation = simulation(vec![Some(intent)]);
        let mut ticks = entry_ticks();
        ticks.extend([
            print(1_003, dec!(100.5), dec!(10), Side::Buy),
            print(1_004, dec!(100.5), dec!(10), Side::Buy),
            print(1_005, dec!(100.5), dec!(10), Side::Buy),
            print(1_006, dec!(100.4), dec!(10), Side::Sell),
        ]);

        replay(&mut simulation, &ticks).await;

        assert!(simulation.open_position().is_none());
        let (_, trades) = simulation.finish();
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].exit_reason, "TimeBarrier");
        assert_eq!(trades[0].exit_price, dec!(100.4));
    }

    #[tokio::test]
    async fn the_wallet_reported_by_the_venue_matches_the_closed_trades() {
        let mut simulation = simulation(vec![long_signal()]);
        let mut ticks = entry_ticks();
        ticks.extend([
            print(1_003, dec!(97.9), dec!(10), Side::Sell),
            print(1_004, dec!(97.4), dec!(10), Side::Sell),
        ]);

        replay(&mut simulation, &ticks).await;

        let wallet = simulation.wallet_balance();
        let (_, trades) = simulation.finish();
        let net: Decimal = trades.iter().map(|trade| trade.pnl_net).sum();
        assert_eq!(wallet, dec!(1000) + net);
    }

    #[tokio::test]
    async fn without_signals_nothing_trades() {
        let mut simulation = simulation(vec![None, None]);

        replay(&mut simulation, &entry_ticks()).await;

        assert_eq!(simulation.ticks_processed(), 3);
        let (metrics, trades) = simulation.finish();
        assert!(trades.is_empty());
        assert_eq!(metrics.total_trades, 0);
    }

    #[test]
    fn an_invalid_threshold_is_rejected() {
        let config = TickSimulationConfig {
            backtest: BacktestConfig::default(),
            symbol: "BTCUSDT".to_string(),
            dollar_bar_threshold: Decimal::ZERO,
            latency_ms: 0,
        };

        let result = TickSimulation::new(
            config,
            Scripted {
                decisions: VecDeque::new(),
            },
        );

        assert!(matches!(result, Err(TickSimulationError::Threshold(_))));
    }
}
