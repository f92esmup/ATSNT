//! Multi-asset portfolio execution and backtesting engine.
//!
//! Orchestrates multiple isolated execution cores across distinct market streams
//! (e.g. BTCUSDT, ETHUSDT, SOLUSDT) with a unified shared cash ledger and strict
//! pre-trade portfolio risk limits (max concurrent positions, max directional exposure,
//! risk-budget sizing, and isolated margin caps).

use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap, HashMap};

use domain::{OrderIntent, PortfolioRiskPolicy, PositionSide, Side};
use rust_decimal::Decimal;
use strategies::Strategy;

use crate::config::BacktestConfig;
use crate::execution_core::{ExecutionCore, PendingEntry, PendingEntryOutcome};
use crate::metrics::{ClosedTrade, TradeExcursion};
use crate::price_event::PriceEvent;

/// Configuration for the multi-asset portfolio engine.
#[derive(Debug, Clone)]
pub struct PortfolioEngineConfig {
    /// Initial shared equity in quote currency (USDT).
    pub initial_capital: Decimal,
    /// Exchange fee for limit maker fills.
    pub maker_fee_pct: Decimal,
    /// Exchange fee for market taker fills.
    pub taker_fee_pct: Decimal,
    /// Slippage penalty for aggressive market fills.
    pub slippage_pct: Decimal,
    /// Pre-trade portfolio risk policy.
    pub risk_policy: PortfolioRiskPolicy,
    /// Order latency in milliseconds.
    pub latency_ms: i64,
}

impl Default for PortfolioEngineConfig {
    fn default() -> Self {
        Self {
            initial_capital: Decimal::new(10_000, 0),
            maker_fee_pct: Decimal::new(2, 4), // 0.0002 = 0.02%
            taker_fee_pct: Decimal::new(5, 4), // 0.0005 = 0.05%
            slippage_pct: Decimal::new(5, 4),  // 0.0005 = 0.05%
            risk_policy: PortfolioRiskPolicy::default(),
            latency_ms: 0,
        }
    }
}

impl PortfolioEngineConfig {
    /// Builds a per-symbol `BacktestConfig` matching this portfolio's fees and execution parameters.
    pub fn per_symbol_config(&self) -> BacktestConfig {
        BacktestConfig {
            initial_capital: self.initial_capital,
            maker_fee_pct: self.maker_fee_pct,
            taker_fee_pct: self.taker_fee_pct,
            slippage_pct: self.slippage_pct,
            risk_per_trade_pct: self.risk_policy.risk_per_trade_pct,
            latency_ms: self.latency_ms,
            resting_maker_exits: true,
            ..BacktestConfig::default()
        }
    }
}

/// An event tagged with its instrument symbol for chronological multi-asset streaming.
#[derive(Debug, Clone)]
pub struct TaggedPriceEvent {
    pub symbol: String,
    pub event: PriceEvent,
}

impl PartialEq for TaggedPriceEvent {
    fn eq(&self, other: &Self) -> bool {
        self.event.end_ms == other.event.end_ms && self.symbol == other.symbol
    }
}

impl Eq for TaggedPriceEvent {}

impl PartialOrd for TaggedPriceEvent {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TaggedPriceEvent {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse ordering so BinaryHeap acts as a min-heap by end timestamp
        other
            .event
            .end_ms
            .cmp(&self.event.end_ms)
            .then_with(|| self.symbol.cmp(&other.symbol))
    }
}

/// Chronological event stream merger for multiple asynchronous asset feeds.
#[derive(Default)]
pub struct EventMerger {
    heap: BinaryHeap<TaggedPriceEvent>,
}

impl EventMerger {
    pub fn new() -> Self {
        Self {
            heap: BinaryHeap::new(),
        }
    }

    /// Pushes a single tagged event into the time priority queue.
    pub fn push(&mut self, symbol: impl Into<String>, event: impl Into<PriceEvent>) {
        self.heap.push(TaggedPriceEvent {
            symbol: symbol.into(),
            event: event.into(),
        });
    }

    /// Pops the chronologically earliest event across all registered assets.
    pub fn pop(&mut self) -> Option<TaggedPriceEvent> {
        self.heap.pop()
    }

    /// Number of remaining events across all asset queues.
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }
}

/// Multi-asset algorithmic portfolio backtesting engine.
pub struct PortfolioEngine {
    config: PortfolioEngineConfig,
    equity: Decimal,
    peak_equity: Decimal,
    max_drawdown_amount: Decimal,
    max_drawdown_pct: Decimal,
    cores: BTreeMap<String, ExecutionCore>,
    last_prices: BTreeMap<String, Decimal>,
    open_excursions: BTreeMap<String, Option<TradeExcursion>>,
    rejected_orders: BTreeMap<String, usize>,
    closed_trades: Vec<ClosedTrade>,
}

impl PortfolioEngine {
    /// Creates a new portfolio engine initialized with the given configuration.
    pub fn new(config: PortfolioEngineConfig) -> Self {
        let initial_capital = config.initial_capital;
        Self {
            config,
            equity: initial_capital,
            peak_equity: initial_capital,
            max_drawdown_amount: Decimal::ZERO,
            max_drawdown_pct: Decimal::ZERO,
            cores: BTreeMap::new(),
            last_prices: BTreeMap::new(),
            open_excursions: BTreeMap::new(),
            rejected_orders: BTreeMap::new(),
            closed_trades: Vec::new(),
        }
    }

    /// Total marked equity across the shared wallet balance and all open isolated positions.
    pub fn total_marked_equity(&self) -> Decimal {
        let mut total = self.equity;
        for (symbol, core) in &self.cores {
            if let Some(pos) = core.position() {
                if let Some(mark_price) = self.last_prices.get(symbol) {
                    total += pos.unrealized_pnl(*mark_price);
                }
            }
        }
        total
    }

    /// Number of currently open positions across all tracked symbols.
    pub fn open_positions_count(&self) -> usize {
        self.cores
            .values()
            .filter(|core| core.position().is_some())
            .count()
    }

    /// Number of open positions matching a given orientation (Long vs Short).
    pub fn directional_positions_count(&self, side: PositionSide) -> usize {
        self.cores
            .values()
            .filter(|core| core.position().is_some_and(|pos| pos.side == side))
            .count()
    }

    /// Tracks a rejection event counter.
    fn count_rejection(&mut self, reason: &str) {
        *self.rejected_orders.entry(reason.to_string()).or_insert(0) += 1;
    }

    /// Ingests one price event for a specific symbol, evaluating exits, pending entries,
    /// and routing the event to the symbol's strategy instance.
    pub fn process_event<S: Strategy>(
        &mut self,
        symbol: &str,
        event: impl Into<PriceEvent>,
        strategy: &mut S,
    ) {
        let bar = event.into();
        self.last_prices.insert(symbol.to_string(), bar.close);

        let per_symbol_cfg = self.config.per_symbol_config();

        // 1. Evaluate pending entry (Market or Limit with TIF timeout)
        let expired = {
            let core = self.cores.entry(symbol.to_string()).or_default();
            matches!(
                core.step_pending_entry(bar, &per_symbol_cfg, &mut self.equity),
                PendingEntryOutcome::ExpiredLimit
            )
        };
        if expired {
            self.count_rejection("LimitTimeoutExpired");
        }

        // 2. Evaluate protective exits for active position
        let newly_closed: Vec<ClosedTrade> = {
            let core = self.cores.entry(symbol.to_string()).or_default();
            let excursion = self.open_excursions.entry(symbol.to_string()).or_default();
            let trades_before = core.ledger().trades().len();
            if core.position().is_some() {
                core.evaluate_exits(bar, &per_symbol_cfg, &mut self.equity, excursion);
            }
            let trades_after = core.ledger().trades().len();
            if trades_after > trades_before {
                core.ledger().trades()[trades_before..trades_after].to_vec()
            } else {
                Vec::new()
            }
        };
        self.closed_trades.extend(newly_closed);

        // 3. Strategy decision
        let dollar_bar = domain::DollarBar {
            start_time: bar.start_ms,
            end_time: bar.end_ms,
            open: bar.open,
            high: bar.high,
            low: bar.low,
            close: bar.close,
            volume: Decimal::ZERO,
            dollar_volume: Decimal::ZERO,
            trade_count: 1,
            buy_volume: Decimal::ZERO,
            sell_volume: Decimal::ZERO,
            buy_dollar_volume: Decimal::ZERO,
            sell_dollar_volume: Decimal::ZERO,
        };

        if let Some(intent) = strategy.on_bar(&dollar_bar) {
            let is_flat = {
                let core = self.cores.entry(symbol.to_string()).or_default();
                core.position().is_none() && core.pending().is_none()
            };
            if is_flat {
                self.try_submit_entry(symbol, intent, bar.end_ms);
            }
        }

        // 4. Update portfolio peak equity and drawdown metrics
        let current_equity = self.total_marked_equity();
        if current_equity > self.peak_equity {
            self.peak_equity = current_equity;
        }

        let dd_amount = self.peak_equity - current_equity;
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
    }

    /// Evaluates pre-trade risk policy and queues an entry if authorized.
    fn try_submit_entry(&mut self, symbol: &str, intent: OrderIntent, event_end_ms: i64) {
        let target_side = match intent.side {
            Side::Buy => PositionSide::Long,
            Side::Sell => PositionSide::Short,
        };

        let current_open = self.open_positions_count();
        let current_directional = self.directional_positions_count(target_side);
        let total_equity = self.total_marked_equity();

        match self.config.risk_policy.size_order(
            &intent,
            current_open,
            current_directional,
            total_equity,
        ) {
            Ok(quantity) => {
                let due_ms = event_end_ms.saturating_add(self.config.latency_ms);
                let core = self.cores.entry(symbol.to_string()).or_default();
                core.submit_due(PendingEntry::new(intent, quantity), due_ms);
            }
            Err(risk_err) => {
                self.count_rejection(risk_err.kind());
            }
        }
    }

    /// Returns all closed trades across all portfolio assets.
    pub fn closed_trades(&self) -> &[ClosedTrade] {
        &self.closed_trades
    }

    /// Returns closed trades partitioned strictly by trading symbol.
    pub fn closed_trades_by_symbol(&self) -> HashMap<String, Vec<ClosedTrade>> {
        self.cores
            .iter()
            .map(|(sym, core)| (sym.clone(), core.ledger().trades().to_vec()))
            .collect()
    }

    /// Returns recorded order rejections.
    pub fn rejected_orders(&self) -> &BTreeMap<String, usize> {
        &self.rejected_orders
    }

    /// Returns maximum portfolio drawdown in percentage.
    pub fn max_drawdown_pct(&self) -> Decimal {
        self.max_drawdown_pct
    }

    /// Returns total closed realized profit and loss across all assets.
    pub fn total_realized_pnl(&self) -> Decimal {
        self.closed_trades.iter().map(|t| t.pnl_net).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{DollarBar, OrderIntent, Side};
    use rust_decimal_macros::dec;
    use strategies::MarketType;

    struct ScriptedSignalStrategy {
        signal_on_end_ms: i64,
        side: Side,
        price: Decimal,
    }

    impl Strategy for ScriptedSignalStrategy {
        fn name(&self) -> &'static str {
            "ScriptedSignal"
        }

        fn compatible_market_types(&self) -> &'static [MarketType] {
            &[MarketType::UsdMPerpetual]
        }

        fn on_bar(&mut self, bar: &DollarBar) -> Option<OrderIntent> {
            if bar.end_time == self.signal_on_end_ms {
                let (stop, take) = match self.side {
                    Side::Buy => (self.price - dec!(2), self.price + dec!(4)),
                    Side::Sell => (self.price + dec!(2), self.price - dec!(4)),
                };
                let intent = OrderIntent::new(bar.end_time, self.side, self.price, stop, take, 5)
                    .expect("valid intent");
                Some(intent)
            } else {
                None
            }
        }

        fn reset(&mut self) {}
    }

    fn test_event(start_ms: i64, end_ms: i64, price: Decimal) -> PriceEvent {
        PriceEvent {
            start_ms,
            end_ms,
            open: price,
            high: price + dec!(1),
            low: price - dec!(1),
            close: price,
        }
    }

    #[test]
    fn event_merger_orders_events_chronologically() {
        let mut merger = EventMerger::new();

        // Push events out of order across two symbols
        merger.push("ETH", test_event(200, 300, dec!(3000)));
        merger.push("BTC", test_event(100, 200, dec!(50000)));
        merger.push("ETH", test_event(100, 150, dec!(2900)));
        merger.push("BTC", test_event(300, 400, dec!(51000)));

        assert_eq!(merger.len(), 4);

        let e1 = merger.pop().unwrap();
        assert_eq!(e1.symbol, "ETH");
        assert_eq!(e1.event.end_ms, 150);

        let e2 = merger.pop().unwrap();
        assert_eq!(e2.symbol, "BTC");
        assert_eq!(e2.event.end_ms, 200);

        let e3 = merger.pop().unwrap();
        assert_eq!(e3.symbol, "ETH");
        assert_eq!(e3.event.end_ms, 300);

        let e4 = merger.pop().unwrap();
        assert_eq!(e4.symbol, "BTC");
        assert_eq!(e4.event.end_ms, 400);

        assert!(merger.is_empty());
    }

    #[test]
    fn portfolio_engine_manages_concurrent_multi_asset_positions() {
        let mut engine = PortfolioEngine::new(PortfolioEngineConfig::default());
        let mut btc_strat = ScriptedSignalStrategy {
            signal_on_end_ms: 100,
            side: Side::Buy,
            price: dec!(50000),
        };
        let mut eth_strat = ScriptedSignalStrategy {
            signal_on_end_ms: 150,
            side: Side::Buy,
            price: dec!(3000),
        };

        // Event 1: BTC bar ending at 100 emits buy intent
        let btc_bar_1 = test_event(0, 100, dec!(50000));
        engine.process_event("BTCUSDT", btc_bar_1, &mut btc_strat);
        assert_eq!(engine.open_positions_count(), 0); // Queued for next bar

        // Event 2: ETH bar ending at 150 emits buy intent
        let eth_bar_1 = test_event(50, 150, dec!(3000));
        engine.process_event("ETHUSDT", eth_bar_1, &mut eth_strat);

        // Event 3: BTC bar ending at 200 fills BTC entry
        let btc_bar_2 = test_event(100, 200, dec!(50000));
        engine.process_event("BTCUSDT", btc_bar_2, &mut btc_strat);
        assert_eq!(engine.open_positions_count(), 1);

        // Event 4: ETH bar ending at 250 fills ETH entry
        let eth_bar_2 = test_event(150, 250, dec!(3000));
        engine.process_event("ETHUSDT", eth_bar_2, &mut eth_strat);
        assert_eq!(engine.open_positions_count(), 2);
        assert_eq!(engine.directional_positions_count(PositionSide::Long), 2);

        // Event 5: BTC bar touches Take Profit (50000 + 4 = 50004) without touching stop (49998)
        let btc_bar_3 = PriceEvent {
            start_ms: 200,
            end_ms: 300,
            open: dec!(50000),
            high: dec!(50010),
            low: dec!(49999),
            close: dec!(50005),
        };
        engine.process_event("BTCUSDT", btc_bar_3, &mut btc_strat);

        // BTC closed, ETH remains open
        assert_eq!(engine.open_positions_count(), 1);
        assert_eq!(engine.closed_trades().len(), 1);
        assert_eq!(engine.closed_trades()[0].exit_reason, "TakeProfit");

        let by_symbol = engine.closed_trades_by_symbol();
        assert_eq!(by_symbol.get("BTCUSDT").map(|t| t.len()), Some(1));
        assert_eq!(by_symbol.get("ETHUSDT").map(|t| t.len()), Some(0));
    }
}
