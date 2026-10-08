//! Account context metadata handlers for Spot and USD-M Futures.

use axum::Json;
use serde::{Deserialize, Serialize};

/// Account and wallet context configuration metadata returned to the dashboard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountContextMetadata {
    /// Context unique identifier: "spot" | "usdm_futures".
    pub id: &'static str,
    /// Human-readable context display name.
    pub name: &'static str,
    /// Base reference currency for valuation and risk limits.
    pub base_currency: &'static str,
    /// Margin calculation mode: "cash" | "isolated".
    pub margin_mode: &'static str,
    /// Position directionality mode: "cash" | "one_way".
    pub position_mode: &'static str,
    /// Whether ATSNT actively supports trading/observation in this context.
    pub is_supported: bool,
    /// Whether ATSNT is dedicated as the sole order writer for this context.
    pub sole_order_writer: bool,
    /// Explanatory description of the accounting profile and risk perimeter.
    pub description: &'static str,
}

/// GET /api/contexts - Returns supported account and wallet contexts.
pub async fn get_contexts_handler() -> Json<Vec<AccountContextMetadata>> {
    Json(vec![
        AccountContextMetadata {
            id: "spot",
            name: "Binance Spot Standard",
            base_currency: "USDT",
            margin_mode: "cash",
            position_mode: "cash",
            is_supported: true,
            sole_order_writer: false,
            description: "Standard Spot wallet with cash settlement, base/quote balance separation, and pre-dispatch funds checks.",
        },
        AccountContextMetadata {
            id: "usdm_futures",
            name: "Binance USDⓈ-M Futures",
            base_currency: "USDT",
            margin_mode: "isolated",
            position_mode: "one_way",
            is_supported: true,
            sole_order_writer: true,
            description: "USD-M Futures profile: USDT single-asset, isolated margin, one-way position mode, dedicated to ATSNT as sole order writer.",
        },
    ])
}
