//! Shared application state and telemetry aggregator for the web server.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use backtest::{PaperTradingEvent, TelemetryConfig, TelemetryEnvelope};
use domain::PositionSide;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, watch, RwLock};

/// Information about an active open position in the paper trading engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PositionInfo {
    /// Direction: Long or Short.
    pub side: PositionSide,
    /// Executed entry price.
    pub entry_price: Decimal,
    /// Position size in base asset units.
    pub quantity: Decimal,
    /// Stop loss exit price.
    pub stop_loss: Decimal,
    /// Take profit exit price.
    pub take_profit: Decimal,
}

/// Summary of a closed trade for session history in telemetry snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClosedTradeSummary {
    /// Unique trade identifier.
    pub id: String,
    /// Traded asset symbol.
    pub symbol: String,
    /// Direction: Long or Short.
    pub side: String,
    /// Execution entry price.
    pub entry_price: Decimal,
    /// Execution exit price.
    pub exit_price: Decimal,
    /// Trade quantity in base units.
    pub quantity: Decimal,
    /// Realized net PnL.
    pub net_pnl: Decimal,
    /// Reason for position exit (e.g. StopLoss, TakeProfit, TimeBarrier).
    pub exit_reason: String,
    /// Exit timestamp in Unix milliseconds.
    pub timestamp: i64,
}

/// Summary of an executed order fill for session telemetry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FillSummary {
    /// Unique fill identifier.
    pub id: String,
    /// Traded asset symbol.
    pub symbol: String,
    /// Order side: Buy or Sell.
    pub side: String,
    /// Execution fill price.
    pub price: Decimal,
    /// Filled quantity.
    pub quantity: Decimal,
    /// Execution fee (USDT).
    pub fee: Decimal,
    /// Execution type: "simulated" or "actual".
    pub execution_type: String,
    /// Fill timestamp in Unix milliseconds.
    pub timestamp: i64,
}

fn default_execution_mode() -> String {
    "idle".to_string()
}

/// Instantaneous telemetry state of the trading portfolio and engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryState {
    /// Latest originating market event time in Unix milliseconds; absent before observation.
    pub timestamp: Option<i64>,
    /// An event gap makes this last-known projection uncertain until authoritative resync.
    #[serde(default)]
    pub stale: bool,
    /// Total portfolio equity (cash balance + unrealized PnL).
    pub portfolio_value: Decimal,
    /// Settled cash balance.
    pub cash_balance: Decimal,
    /// Mark-to-market unrealized PnL.
    pub unrealized_pnl: Decimal,
    /// Currently open position, if any.
    pub active_position: Option<PositionInfo>,
    /// Currently traded asset symbol (e.g. BTCUSDT).
    pub active_symbol: String,
    /// Active strategy identifier.
    pub active_strategy: String,
    /// Latest observed trade/bar price.
    pub last_price: Option<Decimal>,
    /// Current peak-to-trough drawdown percentage.
    pub drawdown_pct: Decimal,
    /// Execution mode: "mock", "paper", or "idle".
    #[serde(default = "default_execution_mode")]
    pub execution_mode: String,
    /// Recent closed trades from this session (bounded at 50).
    #[serde(default)]
    pub recent_closed_trades: Vec<ClosedTradeSummary>,
    /// Recent order fills from this session (bounded at 50).
    #[serde(default)]
    pub recent_fills: Vec<FillSummary>,
}

impl Default for TelemetryState {
    fn default() -> Self {
        Self {
            timestamp: None,
            stale: false,
            portfolio_value: dec!(10_000.00),
            cash_balance: dec!(10_000.00),
            unrealized_pnl: dec!(0.00),
            active_position: None,
            active_symbol: String::new(),
            active_strategy: String::new(),
            last_price: None,
            drawdown_pct: dec!(0.00),
            execution_mode: "idle".to_string(),
            recent_closed_trades: Vec::new(),
            recent_fills: Vec::new(),
        }
    }
}

/// Shared thread-safe application state passed to Axum route handlers.
#[derive(Clone)]
pub struct AppState {
    /// Broadcast sender channel for paper trading telemetry events.
    pub event_sender: broadcast::Sender<TelemetryEnvelope<PaperTradingEvent>>,
    /// Active telemetry snapshot.
    pub state: Arc<RwLock<TelemetryState>>,
    /// Sticky shared uncertainty notification, independent of producer events.
    pub uncertainty: watch::Receiver<bool>,
    /// Directory containing historical JSON reports.
    pub reports_dir: PathBuf,
    /// Number of active WebSocket client connections.
    pub connected_clients: Arc<AtomicUsize>,
    /// Process start time for uptime tracking.
    pub start_time: Instant,
    /// Current execution mode ("mock", "paper", or "idle").
    pub execution_mode: &'static str,
}

impl AppState {
    /// Creates state without assuming a producer identity. Identity strings remain
    /// empty until an envelope arrives; use [`Self::with_telemetry`] to configure
    /// identity before the first event.
    pub fn new(
        event_sender: broadcast::Sender<TelemetryEnvelope<PaperTradingEvent>>,
        reports_dir: PathBuf,
    ) -> Self {
        Self::with_telemetry_and_mode(
            event_sender,
            reports_dir,
            TelemetryConfig {
                symbol: String::new(),
                strategy_id: String::new(),
            },
            "idle",
        )
    }

    /// Creates dashboard state for an explicitly configured telemetry producer.
    /// No timestamp is assigned until an event from that producer is observed.
    pub fn with_telemetry(
        event_sender: broadcast::Sender<TelemetryEnvelope<PaperTradingEvent>>,
        reports_dir: PathBuf,
        telemetry: TelemetryConfig,
    ) -> Self {
        Self::with_telemetry_and_mode(event_sender, reports_dir, telemetry, "mock")
    }

    /// Creates dashboard state with explicit telemetry producer configuration and execution mode.
    pub fn with_telemetry_and_mode(
        event_sender: broadcast::Sender<TelemetryEnvelope<PaperTradingEvent>>,
        reports_dir: PathBuf,
        telemetry: TelemetryConfig,
        execution_mode: &'static str,
    ) -> Self {
        let state = Arc::new(RwLock::new(TelemetryState {
            active_symbol: telemetry.symbol,
            active_strategy: telemetry.strategy_id,
            execution_mode: execution_mode.to_string(),
            ..TelemetryState::default()
        }));
        let connected_clients = Arc::new(AtomicUsize::new(0));
        let start_time = Instant::now();

        // Spawn a background task to keep TelemetryState continuously up-to-date
        // by listening to the broadcast channel.
        let (uncertainty_sender, uncertainty) = watch::channel(false);
        let state_updater = Arc::clone(&state);
        let mut rx = event_sender.subscribe();

        tokio::spawn(async move {
            loop {
                let event = match rx.recv().await {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        state_updater.write().await.stale = true;
                        uncertainty_sender.send_replace(true);
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                let mut st = state_updater.write().await;
                st.timestamp = event.timestamp;
                st.active_symbol = event.symbol.clone();
                st.active_strategy = event.strategy_id;
                match event.payload {
                    PaperTradingEvent::BarFormed(bar) => {
                        st.last_price = Some(bar.close);
                    }
                    PaperTradingEvent::MarkToMarket {
                        current_price,
                        unrealized_pnl,
                        total_equity,
                        drawdown_pct,
                    } => {
                        st.last_price = Some(current_price);
                        st.unrealized_pnl = unrealized_pnl;
                        st.portfolio_value = total_equity;
                        st.drawdown_pct = drawdown_pct;
                    }
                    PaperTradingEvent::PositionOpened {
                        side,
                        entry_price,
                        quantity,
                        stop_loss,
                        take_profit,
                    } => {
                        let now = event.timestamp.unwrap_or(0);
                        let fill_id = format!("fill-{}", now);
                        let side_str = match side {
                            PositionSide::Long => "BUY",
                            PositionSide::Short => "SELL",
                        };
                        st.recent_fills.push(FillSummary {
                            id: fill_id,
                            symbol: event.symbol.clone(),
                            side: side_str.to_string(),
                            price: entry_price,
                            quantity,
                            fee: Decimal::ZERO,
                            execution_type: "simulated".to_string(),
                            timestamp: now,
                        });
                        if st.recent_fills.len() > 50 {
                            st.recent_fills.remove(0);
                        }
                        st.active_position = Some(PositionInfo {
                            side,
                            entry_price,
                            quantity,
                            stop_loss,
                            take_profit,
                        });
                    }
                    PaperTradingEvent::PositionClosed {
                        exit_reason,
                        exit_price,
                        net_pnl,
                        total_equity,
                    } => {
                        let now = event.timestamp.unwrap_or(0);
                        let (entry_px, qty, pos_side) = if let Some(ref pos) = st.active_position {
                            (pos.entry_price, pos.quantity, pos.side)
                        } else {
                            (exit_price, Decimal::ZERO, PositionSide::Long)
                        };
                        let exit_side_str = match pos_side {
                            PositionSide::Long => "SELL",
                            PositionSide::Short => "BUY",
                        };
                        let fill_id = format!("fill-{}", now);
                        st.recent_fills.push(FillSummary {
                            id: fill_id,
                            symbol: event.symbol.clone(),
                            side: exit_side_str.to_string(),
                            price: exit_price,
                            quantity: qty,
                            fee: Decimal::ZERO,
                            execution_type: "simulated".to_string(),
                            timestamp: now,
                        });
                        if st.recent_fills.len() > 50 {
                            st.recent_fills.remove(0);
                        }
                        let trade_id = format!("trade-{}", now);
                        st.recent_closed_trades.push(ClosedTradeSummary {
                            id: trade_id,
                            symbol: event.symbol.clone(),
                            side: format!("{:?}", pos_side),
                            entry_price: entry_px,
                            exit_price,
                            quantity: qty,
                            net_pnl,
                            exit_reason,
                            timestamp: now,
                        });
                        if st.recent_closed_trades.len() > 50 {
                            st.recent_closed_trades.remove(0);
                        }
                        st.active_position = None;
                        st.cash_balance = total_equity;
                        st.portfolio_value = total_equity;
                        st.unrealized_pnl = Decimal::ZERO;
                    }
                    _ => {}
                }
            }
        });

        Self {
            event_sender,
            state,
            uncertainty,
            reports_dir,
            connected_clients,
            start_time,
            execution_mode,
        }
    }

    /// Increments the connected client count.
    #[inline]
    pub fn client_connected(&self) {
        self.connected_clients.fetch_add(1, Ordering::Relaxed);
    }

    /// Decrements the connected client count.
    #[inline]
    pub fn client_disconnected(&self) {
        self.connected_clients.fetch_sub(1, Ordering::Relaxed);
    }

    /// Number of currently connected WebSocket clients.
    #[inline]
    pub fn connected_count(&self) -> usize {
        self.connected_clients.load(Ordering::Relaxed)
    }

    /// Server uptime in seconds.
    #[inline]
    pub fn uptime_secs(&self) -> u64 {
        self.start_time.elapsed().as_secs()
    }

    /// Sets or overrides the current execution mode.
    pub fn with_execution_mode(mut self, mode: &'static str) -> Self {
        self.execution_mode = mode;
        if let Ok(mut st) = self.state.try_write() {
            st.execution_mode = mode.to_string();
        }
        self
    }
}
