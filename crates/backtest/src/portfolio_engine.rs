//! Multi-asset portfolio execution and backtesting engine.
//!
//! Orchestrates multiple isolated execution cores across distinct market streams
//! (e.g. BTCUSDT, ETHUSDT, SOLUSDT) with a unified shared cash ledger and strict
//! pre-trade portfolio risk limits (max concurrent positions, max directional exposure,
//! risk-budget sizing, and isolated margin caps).

use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap, HashMap, VecDeque};

use domain::{OrderIntent, PortfolioRiskPolicy, PositionSide, Side, UniverseSelectionPolicy};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use strategies::{MarketType, Strategy};

use crate::config::BacktestConfig;
use crate::execution_core::{ExecutionCore, PendingEntry, PendingEntryOutcome};
use crate::metrics::{BacktestMetrics, ClosedTrade, TradeExcursion, SPOT_SHORTING_DISALLOWED};
use crate::price_event::PriceEvent;
use crate::universe_selector::CrossSectionalUniverseSelector;

/// Online rolling Pearson correlation estimator over time-synchronized horizon return pairs.
#[derive(Debug, Clone)]
pub struct RollingCorrelationTracker {
    horizon_ms: i64,
    window_capacity: usize,
    min_observations: usize,
    price_history: VecDeque<(i64, f64, f64)>,
    returns_x: VecDeque<f64>,
    returns_y: VecDeque<f64>,
}

impl RollingCorrelationTracker {
    /// Creates a new rolling correlation tracker with default 36-hour swing horizon.
    pub fn new(window_capacity: usize, min_observations: usize) -> Self {
        Self::with_horizon(36, window_capacity, min_observations)
    }

    /// Creates a new rolling correlation tracker with a custom horizon in hours.
    pub fn with_horizon(
        horizon_hours: i64,
        window_capacity: usize,
        min_observations: usize,
    ) -> Self {
        Self {
            horizon_ms: horizon_hours.max(1) * 3_600_000,
            window_capacity: window_capacity.max(8),
            min_observations: min_observations.max(4),
            price_history: VecDeque::new(),
            returns_x: VecDeque::with_capacity(window_capacity + 1),
            returns_y: VecDeque::with_capacity(window_capacity + 1),
        }
    }

    /// Pushes a new price observation pair $(P_x, P_y)$ with exact timestamp.
    /// Calculates log-returns $r_x = \ln(P_x(t) / P_x(t - \Delta t))$ and $r_y = \ln(P_y(t) / P_y(t - \Delta t))$.
    pub fn push_observation(&mut self, timestamp_ms: i64, price_x: f64, price_y: f64) {
        if price_x <= 0.0 || price_y <= 0.0 {
            return;
        }

        self.price_history
            .push_back((timestamp_ms, price_x, price_y));

        let cutoff = timestamp_ms.saturating_sub(self.horizon_ms * 3);
        while let Some(&(t, _, _)) = self.price_history.front() {
            if t < cutoff {
                self.price_history.pop_front();
            } else {
                break;
            }
        }

        let target_t = timestamp_ms - self.horizon_ms;
        let mut past_sample = None;
        for &(t, px, py) in self.price_history.iter().rev() {
            if t <= target_t {
                past_sample = Some((px, py));
                break;
            }
        }

        if let Some((past_x, past_y)) = past_sample {
            if past_x > 0.0 && past_y > 0.0 {
                let ret_x = (price_x / past_x).ln();
                let ret_y = (price_y / past_y).ln();

                if self.returns_x.len() >= self.window_capacity {
                    self.returns_x.pop_front();
                    self.returns_y.pop_front();
                }
                self.returns_x.push_back(ret_x);
                self.returns_y.push_back(ret_y);
            }
        }
    }

    /// Pushes a price observation using incremental horizon steps for synthetic tests.
    pub fn push_prices(&mut self, price_x: f64, price_y: f64) {
        let last_t = self.price_history.back().map_or(0, |&(t, _, _)| t);
        let next_t = last_t + self.horizon_ms;
        self.push_observation(next_t, price_x, price_y);
    }

    /// Computes the sample Pearson correlation coefficient $\rho_{x, y} \in [-1.0, 1.0]$.
    /// Returns `None` if the number of observations is below `min_observations` or if variance is degenerate.
    pub fn correlation(&self) -> Option<f64> {
        let n = self.returns_x.len();
        if n < self.min_observations {
            return None;
        }

        let mut sum_x = 0.0;
        let mut sum_y = 0.0;
        for i in 0..n {
            sum_x += self.returns_x[i];
            sum_y += self.returns_y[i];
        }
        let mean_x = sum_x / n as f64;
        let mean_y = sum_y / n as f64;

        let mut var_x = 0.0;
        let mut var_y = 0.0;
        let mut cov_xy = 0.0;

        for i in 0..n {
            let dx = self.returns_x[i] - mean_x;
            let dy = self.returns_y[i] - mean_y;
            cov_xy += dx * dy;
            var_x += dx * dx;
            var_y += dy * dy;
        }

        let denom = (var_x * var_y).sqrt();
        if denom <= 1e-12 {
            return None; // Degenerate variance (constant returns)
        }

        let rho = (cov_xy / denom).clamp(-1.0, 1.0);
        Some(rho)
    }

    /// Number of accumulated return observations.
    pub fn len(&self) -> usize {
        self.returns_x.len()
    }

    /// Whether any return observations have been accumulated.
    pub fn is_empty(&self) -> bool {
        self.returns_x.is_empty()
    }

    /// Whether minimum observations have been satisfied.
    pub fn is_warm(&self) -> bool {
        self.returns_x.len() >= self.min_observations
    }
}

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
    /// Market type governing exchange rules (Spot vs USD-M Perpetual).
    pub market_type: MarketType,
    /// Whether to enforce BTC market breadth filter (blocks Altcoin Longs when BTC is macro bearish).
    pub btc_breadth_filter: bool,
    /// Minimum Pearson correlation with Bitcoin to trigger macro bearish blocking (default 0.65).
    pub btc_correlation_threshold: Decimal,
    /// Rolling window capacity in bars for correlation estimation (default 35).
    pub btc_correlation_window: usize,
    /// Dynamic universe selection policy for multi-asset ranking and Top-K filtering.
    pub universe_policy: Option<UniverseSelectionPolicy>,
    /// Benchmark symbol representing the market lead (default "BTCUSDT", or "BTCEUR" in EUR universe).
    pub benchmark_symbol: String,
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
            market_type: MarketType::UsdMPerpetual,
            btc_breadth_filter: false,
            btc_correlation_threshold: dec!(0.65),
            btc_correlation_window: 35,
            universe_policy: None,
            benchmark_symbol: "BTCUSDT".to_string(),
        }
    }
}

impl PortfolioEngineConfig {
    /// Constructs a Spot portfolio configuration with standard Binance Spot fees (0.10% maker/taker),
    /// strict 1x leverage, and zero funding.
    pub fn spot() -> Self {
        Self {
            initial_capital: Decimal::new(10_000, 0),
            maker_fee_pct: Decimal::new(10, 4), // 0.0010 = 0.10%
            taker_fee_pct: Decimal::new(10, 4), // 0.0010 = 0.10%
            slippage_pct: Decimal::new(5, 4),   // 0.0005 = 0.05%
            risk_policy: PortfolioRiskPolicy {
                max_leverage: Decimal::ONE,
                ..PortfolioRiskPolicy::default()
            },
            latency_ms: 0,
            market_type: MarketType::Spot,
            btc_breadth_filter: false,
            btc_correlation_threshold: dec!(0.65),
            btc_correlation_window: 35,
            universe_policy: None,
            benchmark_symbol: "BTCUSDT".to_string(),
        }
    }

    /// Constructs a Spot portfolio configuration with Binance BNB 25% fee discount (0.075% maker/taker).
    pub fn spot_with_bnb_discount() -> Self {
        let mut cfg = Self::spot();
        cfg.maker_fee_pct = Decimal::new(75, 5); // 0.00075 = 0.075%
        cfg.taker_fee_pct = Decimal::new(75, 5);
        cfg
    }

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
            market_type: self.market_type,
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

/// Detailed outcome of stepping one price event through the portfolio engine.
#[derive(Debug, Clone, Default)]
pub struct PortfolioStepReport {
    /// Newly opened position on this event (if any), with its protective barriers.
    pub opened_position: Option<(domain::Position, Option<domain::ProtectiveExits>)>,
    /// Newly closed trades on this event (if any).
    pub closed_trades: Vec<ClosedTrade>,
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
    btc_macro_bullish: bool,
    correlation_trackers: BTreeMap<String, RollingCorrelationTracker>,
    universe_selector: Option<CrossSectionalUniverseSelector>,
}

impl PortfolioEngine {
    /// Creates a new portfolio engine initialized with the given configuration.
    pub fn new(config: PortfolioEngineConfig) -> Self {
        let initial_capital = config.initial_capital;
        let benchmark = config.benchmark_symbol.clone();
        let universe_selector = config
            .universe_policy
            .as_ref()
            .map(|p| CrossSectionalUniverseSelector::new(p.clone(), &benchmark));
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
            btc_macro_bullish: true,
            correlation_trackers: BTreeMap::new(),
            universe_selector,
        }
    }

    /// Sets or replaces the cross-sectional universe selector.
    pub fn with_universe_selector(mut self, selector: CrossSectionalUniverseSelector) -> Self {
        self.universe_selector = Some(selector);
        self
    }

    /// Returns a reference to the active universe selector, if present.
    pub fn universe_selector(&self) -> Option<&CrossSectionalUniverseSelector> {
        self.universe_selector.as_ref()
    }

    /// Returns a mutable reference to the active universe selector, if present.
    pub fn universe_selector_mut(&mut self) -> Option<&mut CrossSectionalUniverseSelector> {
        self.universe_selector.as_mut()
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
    pub fn process_event<S: Strategy + ?Sized>(
        &mut self,
        symbol: &str,
        event: impl Into<PriceEvent>,
        strategy: &mut S,
    ) -> PortfolioStepReport {
        let bar = event.into();
        self.last_prices.insert(symbol.to_string(), bar.close);

        // Update rolling cross-asset correlation tracker against Bitcoin / benchmark lead
        if symbol != self.config.benchmark_symbol.as_str() {
            if let Some(btc_price) = self.last_prices.get(&self.config.benchmark_symbol).copied() {
                let sym_f64 = bar.close.to_f64().unwrap_or(0.0);
                let btc_f64 = btc_price.to_f64().unwrap_or(0.0);
                let window = self.config.btc_correlation_window;
                let min_obs = (window / 2).clamp(4, 15);
                let tracker = self
                    .correlation_trackers
                    .entry(symbol.to_string())
                    .or_insert_with(|| RollingCorrelationTracker::new(window, min_obs));
                tracker.push_observation(bar.end_ms, sym_f64, btc_f64);
            }
        }

        let per_symbol_cfg = self.config.per_symbol_config();

        // 1. Evaluate pending entry (Market or Limit with TIF timeout)
        let (expired, newly_opened) = {
            let core = self.cores.entry(symbol.to_string()).or_default();
            let pos_before = core.position().is_some();
            let outcome = core.step_pending_entry(bar, &per_symbol_cfg, &mut self.equity);
            let expired = matches!(outcome, PendingEntryOutcome::ExpiredLimit);
            let opened = if !pos_before && core.position().is_some() {
                core.position()
                    .cloned()
                    .map(|pos| (pos, core.exits().copied()))
            } else {
                None
            };
            (expired, opened)
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
        self.closed_trades.extend(newly_closed.clone());

        // 3. Strategy decision
        let dollar_bar = domain::DollarBar {
            start_time: bar.start_ms,
            end_time: bar.end_ms,
            open: bar.open,
            high: bar.high,
            low: bar.low,
            close: bar.close,
            volume: bar.volume,
            dollar_volume: bar.dollar_volume,
            trade_count: 1,
            buy_volume: Decimal::ZERO,
            sell_volume: Decimal::ZERO,
            buy_dollar_volume: bar.buy_dollar_volume,
            sell_dollar_volume: bar.sell_dollar_volume,
        };

        let strategy_intent = strategy.on_bar(&dollar_bar);

        // Feed dynamic universe selector with bar and quantum metrics if active
        if let Some(ref mut selector) = self.universe_selector {
            let quantum_metric = strategy.audit().and_then(|a| {
                a.get("curvature_k")
                    .or_else(|| a.get("f_total"))
                    .and_then(|v| v.as_f64())
                    .and_then(Decimal::from_f64_retain)
            });
            selector.process_bar(symbol, &dollar_bar, quantum_metric);
        }

        if let Some(intent) = strategy_intent {
            let is_flat = {
                let core = self.cores.entry(symbol.to_string()).or_default();
                core.position().is_none() && core.pending().is_none()
            };
            if is_flat {
                self.try_submit_entry(symbol, intent, bar.end_ms);
            }
        }

        // Track BTC macro trend for market breadth filter
        if symbol == self.config.benchmark_symbol.as_str() {
            if let Some(audit) = strategy.audit() {
                if let Some(macro_ok) = audit.get("macro_ok").and_then(|v| v.as_bool()) {
                    self.btc_macro_bullish = macro_ok;
                }
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

        PortfolioStepReport {
            opened_position: newly_opened,
            closed_trades: newly_closed,
        }
    }

    /// Evaluates pre-trade risk policy and queues an entry if authorized.
    fn try_submit_entry(&mut self, symbol: &str, intent: OrderIntent, event_end_ms: i64) {
        if self.config.market_type == MarketType::Spot && intent.side == Side::Sell {
            self.count_rejection(SPOT_SHORTING_DISALLOWED);
            return;
        }

        // Enforce Universe Selection Policy (if universe_selector is active):
        if let Some(ref selector) = self.universe_selector {
            if !selector.is_symbol_authorized_for_entry(symbol) {
                self.count_rejection("ExcludedByUniverseRanking");
                return;
            }
        }

        // Enforce BTC market breadth lead filter with dynamic rolling Pearson correlation gating:
        // When BTC is macro bearish, block altcoins ONLY if their rolling Pearson correlation
        // with BTC is high (>= btc_correlation_threshold). If decoupled or during warmup, allow.
        if self.config.btc_breadth_filter
            && symbol != self.config.benchmark_symbol.as_str()
            && !self.btc_macro_bullish
        {
            if let Some(tracker) = self.correlation_trackers.get(symbol) {
                if let Some(rho) = tracker.correlation() {
                    let rho_dec = Decimal::from_f64_retain(rho).unwrap_or(Decimal::ZERO);
                    if rho_dec >= self.config.btc_correlation_threshold {
                        self.count_rejection("BtcCorrelatedBearish");
                        return;
                    }
                }
            }
        }

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

    /// Returns the current (free quote, free base) balance for a specific symbol in Spot mode.
    pub fn spot_balances(&self, symbol: &str) -> (Decimal, Decimal) {
        if let Some(core) = self.cores.get(symbol) {
            core.spot_balances(self.equity)
        } else {
            (self.equity, Decimal::ZERO)
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

    /// Returns the cash balance in the shared portfolio wallet.
    pub fn cash_equity(&self) -> Decimal {
        self.equity
    }

    /// Returns the active position for a specific symbol, if one is currently open.
    pub fn position(&self, symbol: &str) -> Option<&domain::Position> {
        self.cores.get(symbol).and_then(|c| c.position())
    }

    /// Returns the active protective exits for a specific symbol, if any.
    pub fn exits(&self, symbol: &str) -> Option<&domain::ProtectiveExits> {
        self.cores.get(symbol).and_then(|c| c.exits())
    }

    /// Returns whether the specified symbol has no open position and no pending entry.
    pub fn is_flat(&self, symbol: &str) -> bool {
        self.cores
            .get(symbol)
            .map_or(true, |c| c.position().is_none() && c.pending().is_none())
    }

    /// Returns the latest marked close price recorded for a symbol.
    pub fn last_price(&self, symbol: &str) -> Option<Decimal> {
        self.last_prices.get(symbol).copied()
    }

    /// Returns the rolling Pearson correlation of an asset with Bitcoin, if warmed up.
    pub fn btc_correlation(&self, symbol: &str) -> Option<f64> {
        self.correlation_trackers
            .get(symbol)
            .and_then(|t| t.correlation())
    }

    /// Current peak-to-trough drawdown percentage.
    pub fn current_drawdown_pct(&self) -> Decimal {
        let current = self.total_marked_equity();
        if self.peak_equity > Decimal::ZERO && current < self.peak_equity {
            (self.peak_equity - current) / self.peak_equity
        } else {
            Decimal::ZERO
        }
    }

    /// Finalizes the portfolio backtest by closing any remaining open positions at
    /// the last recorded price as `EndOfSimulation` and computing comprehensive portfolio metrics.
    pub fn finish(&mut self, last_timestamp: i64) -> (BacktestMetrics, Vec<ClosedTrade>) {
        let per_symbol_cfg = self.config.per_symbol_config();
        for (symbol, core) in &mut self.cores {
            if core.position().is_some() {
                let mark = self
                    .last_prices
                    .get(symbol)
                    .copied()
                    .unwrap_or(Decimal::ZERO);
                if mark > Decimal::ZERO {
                    let mut excursion = self.open_excursions.remove(symbol).flatten();
                    let trades_before = core.ledger().trades().len();
                    core.execute_close(
                        last_timestamp,
                        mark,
                        Decimal::ZERO,
                        false,
                        "EndOfSimulation",
                        &per_symbol_cfg,
                        &mut self.equity,
                        &mut excursion,
                    );
                    let newly_closed = &core.ledger().trades()[trades_before..];
                    self.closed_trades.extend_from_slice(newly_closed);
                }
            }
        }

        let metrics = BacktestMetrics::calculate(
            self.config.initial_capital,
            &self.closed_trades,
            self.max_drawdown_amount,
            self.max_drawdown_pct,
        );

        (metrics, self.closed_trades.clone())
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
            volume: Decimal::ONE,
            dollar_volume: price,
            buy_dollar_volume: price,
            sell_dollar_volume: Decimal::ZERO,
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
            volume: Decimal::ONE,
            dollar_volume: dec!(50000),
            buy_dollar_volume: dec!(50000),
            sell_dollar_volume: Decimal::ZERO,
        };
        engine.process_event("BTCUSDT", btc_bar_3, &mut btc_strat);

        // BTC closed, ETH remains open
        assert_eq!(engine.open_positions_count(), 1);
        assert_eq!(engine.closed_trades().len(), 1);
        assert_eq!(engine.closed_trades()[0].exit_reason, "TakeProfit");

        let by_symbol = engine.closed_trades_by_symbol();
        assert_eq!(by_symbol.get("BTCUSDT").map(|t| t.len()), Some(1));
        assert_eq!(by_symbol.get("ETHUSDT").map(|t| t.len()), Some(0));

        // Finish engine: remaining ETH position is closed as EndOfSimulation
        let (metrics, trades) = engine.finish(300);
        assert_eq!(trades.len(), 2);
        assert_eq!(trades[1].exit_reason, "EndOfSimulation");
        assert_eq!(metrics.total_trades, 2);
    }

    #[test]
    fn test_portfolio_spot_shorting_disallowed() {
        let mut engine = PortfolioEngine::new(PortfolioEngineConfig::spot());

        let mut sell_strat = ScriptedSignalStrategy {
            signal_on_end_ms: 100,
            side: Side::Sell,
            price: dec!(50000),
        };

        let bar = test_event(0, 100, dec!(50000));
        engine.process_event("BTCUSDT", bar, &mut sell_strat);

        assert_eq!(engine.open_positions_count(), 0);
        assert_eq!(
            engine.rejected_orders().get(SPOT_SHORTING_DISALLOWED),
            Some(&1),
            "Spot portfolio engine must reject short entry intents"
        );
    }

    #[test]
    fn test_portfolio_spot_multi_asset_execution() {
        let mut engine = PortfolioEngine::new(PortfolioEngineConfig::spot());

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

        // 1. Submit signals
        engine.process_event("BTCUSDT", test_event(0, 100, dec!(50000)), &mut btc_strat);
        engine.process_event("ETHUSDT", test_event(50, 150, dec!(3000)), &mut eth_strat);

        // 2. Fill entries
        engine.process_event("BTCUSDT", test_event(100, 200, dec!(50000)), &mut btc_strat);
        engine.process_event("ETHUSDT", test_event(150, 250, dec!(3000)), &mut eth_strat);

        assert_eq!(engine.open_positions_count(), 2);
        assert_eq!(engine.directional_positions_count(PositionSide::Long), 2);

        // Check spot dual balances
        let (free_quote_btc, free_base_btc) = engine.spot_balances("BTCUSDT");
        assert!(free_base_btc > Decimal::ZERO);
        assert!(free_quote_btc < dec!(10000));

        // 3. Take profit on BTC
        let btc_tp = PriceEvent {
            start_ms: 200,
            end_ms: 300,
            open: dec!(50000),
            high: dec!(50010),
            low: dec!(49999),
            close: dec!(50005),
            volume: Decimal::ONE,
            dollar_volume: dec!(50000),
            buy_dollar_volume: dec!(50000),
            sell_dollar_volume: Decimal::ZERO,
        };
        engine.process_event("BTCUSDT", btc_tp, &mut btc_strat);

        assert_eq!(engine.open_positions_count(), 1);
        assert_eq!(engine.closed_trades().len(), 1);
        assert_eq!(engine.closed_trades()[0].exit_reason, "TakeProfit");
    }

    #[test]
    fn test_rolling_correlation_tracker_lifecycle() {
        let mut tracker = RollingCorrelationTracker::new(20, 5);
        assert_eq!(tracker.len(), 0);
        assert!(!tracker.is_warm());
        assert_eq!(tracker.correlation(), None);

        // 1. Degenerate / flat returns: constant price series
        for _ in 0..10 {
            tracker.push_prices(100.0, 50.0);
        }
        assert!(tracker.is_warm());
        // All returns are 0.0 -> variance is 0.0 -> returns None safely
        assert_eq!(tracker.correlation(), None);

        // 2. Perfectly correlated series: Px = 2 * Py (with variance)
        let mut corr_tracker = RollingCorrelationTracker::new(20, 5);
        let btc_prices = [100.0, 102.0, 101.0, 105.0, 103.0, 108.0, 112.0, 110.0];
        for &p in &btc_prices {
            corr_tracker.push_prices(p * 2.5, p);
        }
        let rho = corr_tracker.correlation().expect("Must have correlation");
        assert!((rho - 1.0).abs() < 1e-4, "Expected rho ~ 1.0, got {rho}");

        // 3. Inverse series
        let mut inv_tracker = RollingCorrelationTracker::new(20, 5);
        let eth_prices = [200.0, 196.0, 198.0, 190.0, 194.0, 185.0, 178.0, 182.0];
        for i in 0..btc_prices.len() {
            inv_tracker.push_prices(eth_prices[i], btc_prices[i]);
        }
        let inv_rho = inv_tracker.correlation().expect("Must have correlation");
        assert!(
            inv_rho < -0.90,
            "Expected negative rho < -0.90, got {inv_rho}"
        );
    }

    #[test]
    fn test_dynamic_correlation_breadth_filter() {
        let mut cfg = PortfolioEngineConfig::spot();
        cfg.btc_breadth_filter = true;
        cfg.btc_correlation_threshold = dec!(0.65);
        cfg.btc_correlation_window = 10;
        let mut engine = PortfolioEngine::new(cfg);

        // Set BTC macro to bearish via an audit-returning strategy
        struct BearishAuditStrategy;
        impl Strategy for BearishAuditStrategy {
            fn name(&self) -> &'static str {
                "BearishAudit"
            }
            fn compatible_market_types(&self) -> &'static [MarketType] {
                &[MarketType::Spot, MarketType::UsdMPerpetual]
            }
            fn reset(&mut self) {}
            fn on_bar(&mut self, _bar: &domain::DollarBar) -> Option<OrderIntent> {
                None
            }
            fn audit(&self) -> Option<serde_json::Value> {
                Some(serde_json::json!({ "macro_ok": false }))
            }
        }

        let mut btc_strat = BearishAuditStrategy;
        let mut signal_strat = ScriptedSignalStrategy {
            signal_on_end_ms: 1000,
            side: Side::Buy,
            price: dec!(100),
        };

        // Seed BTC prices and set BTC bearish
        let btc_prices = [
            50000.0, 50200.0, 49800.0, 49500.0, 49200.0, 48900.0, 48500.0, 48000.0,
        ];
        // ETH correlated with BTC
        let eth_prices = [
            3000.0, 3012.0, 2988.0, 2970.0, 2952.0, 2934.0, 2910.0, 2880.0,
        ];
        // SOL decoupled / diverging from BTC (moving upwards while BTC drops)
        let sol_prices = [150.0, 153.0, 156.0, 160.0, 165.0, 172.0, 180.0, 190.0];

        let step_ms = 36 * 3_600_000;
        for i in 0..btc_prices.len() {
            let t = (i as i64 + 1) * step_ms;
            engine.process_event(
                "BTCUSDT",
                test_event(t - 50, t, Decimal::from_f64_retain(btc_prices[i]).unwrap()),
                &mut btc_strat,
            );
            engine.process_event(
                "ETHUSDT",
                test_event(t - 50, t, Decimal::from_f64_retain(eth_prices[i]).unwrap()),
                &mut signal_strat,
            );
            engine.process_event(
                "SOLUSDT",
                test_event(t - 50, t, Decimal::from_f64_retain(sol_prices[i]).unwrap()),
                &mut signal_strat,
            );
        }

        // Check computed correlations
        let eth_rho = engine.btc_correlation("ETHUSDT").expect("ETH rho");
        let sol_rho = engine.btc_correlation("SOLUSDT").expect("SOL rho");
        assert!(
            eth_rho >= 0.90,
            "ETH should be strongly correlated with BTC, got {eth_rho}"
        );
        assert!(
            sol_rho < 0.0,
            "SOL should be inversely / decoupled from BTC, got {sol_rho}"
        );

        let t_signal = (btc_prices.len() as i64 + 2) * step_ms;
        // Attempt entry on ETH (should be blocked because BTC is bearish AND rho >= 0.65)
        let mut eth_buy = ScriptedSignalStrategy {
            signal_on_end_ms: t_signal,
            side: Side::Buy,
            price: dec!(2880),
        };
        engine.process_event(
            "ETHUSDT",
            test_event(t_signal - 50, t_signal, dec!(2880)),
            &mut eth_buy,
        );
        assert_eq!(
            engine.rejected_orders().get("BtcCorrelatedBearish"),
            Some(&1),
            "Correlated ETH must be blocked when BTC is bearish"
        );

        // Attempt entry on SOL (should be ALLOWED because rho < 0.65)
        let mut sol_buy = ScriptedSignalStrategy {
            signal_on_end_ms: t_signal,
            side: Side::Buy,
            price: dec!(190),
        };
        engine.process_event(
            "SOLUSDT",
            test_event(t_signal - 50, t_signal, dec!(190)),
            &mut sol_buy,
        );
        assert_eq!(
            engine.rejected_orders().get("BtcCorrelatedBearish"),
            Some(&1),
            "Decoupled SOL must NOT increment BtcCorrelatedBearish"
        );
    }
}
