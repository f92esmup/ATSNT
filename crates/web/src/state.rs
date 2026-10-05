//! Shared application state and telemetry aggregator for the web server.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use backtest::PaperTradingEvent;
use domain::PositionSide;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};

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

/// Instantaneous telemetry state of the trading portfolio and engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryState {
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
}

impl Default for TelemetryState {
    fn default() -> Self {
        Self {
            portfolio_value: dec!(10_000.00),
            cash_balance: dec!(10_000.00),
            unrealized_pnl: dec!(0.00),
            active_position: None,
            active_symbol: "BTCUSDT".to_string(),
            active_strategy: "DollarBarsCusumStrategy".to_string(),
            last_price: None,
            drawdown_pct: dec!(0.00),
        }
    }
}

/// Shared thread-safe application state passed to Axum route handlers.
#[derive(Clone)]
pub struct AppState {
    /// Broadcast sender channel for paper trading telemetry events.
    pub event_sender: broadcast::Sender<PaperTradingEvent>,
    /// Active telemetry snapshot.
    pub state: Arc<RwLock<TelemetryState>>,
    /// Directory containing historical JSON reports.
    pub reports_dir: PathBuf,
    /// Number of active WebSocket client connections.
    pub connected_clients: Arc<AtomicUsize>,
    /// Process start time for uptime tracking.
    pub start_time: Instant,
}

impl AppState {
    /// Creates a new [`AppState`] with the provided broadcast channel and reports directory.
    pub fn new(event_sender: broadcast::Sender<PaperTradingEvent>, reports_dir: PathBuf) -> Self {
        let state = Arc::new(RwLock::new(TelemetryState::default()));
        let connected_clients = Arc::new(AtomicUsize::new(0));
        let start_time = Instant::now();

        // Spawn a background task to keep TelemetryState continuously up-to-date
        // by listening to the broadcast channel.
        let state_updater = Arc::clone(&state);
        let mut rx = event_sender.subscribe();

        tokio::spawn(async move {
            while let Ok(event) = rx.recv().await {
                let mut st = state_updater.write().await;
                match event {
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
                        st.active_position = Some(PositionInfo {
                            side,
                            entry_price,
                            quantity,
                            stop_loss,
                            take_profit,
                        });
                    }
                    PaperTradingEvent::PositionClosed { total_equity, .. } => {
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
            reports_dir,
            connected_clients,
            start_time,
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
}
