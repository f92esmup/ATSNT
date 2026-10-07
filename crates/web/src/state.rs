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
}

impl AppState {
    /// Creates state without assuming a producer identity. Identity strings remain
    /// empty until an envelope arrives; use [`Self::with_telemetry`] to configure
    /// identity before the first event.
    pub fn new(
        event_sender: broadcast::Sender<TelemetryEnvelope<PaperTradingEvent>>,
        reports_dir: PathBuf,
    ) -> Self {
        Self::with_telemetry(
            event_sender,
            reports_dir,
            TelemetryConfig {
                symbol: String::new(),
                strategy_id: String::new(),
            },
        )
    }

    /// Creates dashboard state for an explicitly configured telemetry producer.
    /// No timestamp is assigned until an event from that producer is observed.
    pub fn with_telemetry(
        event_sender: broadcast::Sender<TelemetryEnvelope<PaperTradingEvent>>,
        reports_dir: PathBuf,
        telemetry: TelemetryConfig,
    ) -> Self {
        let state = Arc::new(RwLock::new(TelemetryState {
            active_symbol: telemetry.symbol,
            active_strategy: telemetry.strategy_id,
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
                st.active_symbol = event.symbol;
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
            uncertainty,
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
