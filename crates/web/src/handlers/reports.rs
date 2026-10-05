//! Historical report index and inspection handlers.

use std::fs;
use std::time::UNIX_EPOCH;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// High-level summary of an execution or optimization report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportSummary {
    /// Name of the report JSON file on disk.
    pub filename: String,
    /// Inferred category: "paper_trading" | "hpo" | "backtest".
    pub report_type: String,
    /// Size of the JSON file in bytes.
    pub size_bytes: u64,
    /// Last modified Unix timestamp in seconds.
    pub modified_timestamp: u64,
    /// Traded asset symbol, if parsed.
    pub symbol: Option<String>,
    /// Net profit / return, if present.
    pub net_profit: Option<String>,
    /// Win rate percentage, if present.
    pub win_rate: Option<String>,
    /// Sortino ratio, if present.
    pub sortino_ratio: Option<String>,
    /// Total closed trades count.
    pub total_trades: Option<usize>,
}

/// GET /api/reports - Lists all available audit reports in descending chronological order.
pub async fn list_reports_handler(State(app_state): State<AppState>) -> Json<Vec<ReportSummary>> {
    let mut summaries = Vec::new();
    let dir = &app_state.reports_dir;

    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("json") {
                let filename = entry.file_name().to_string_lossy().to_string();
                let metadata = match entry.metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };

                let size_bytes = metadata.len();
                let modified_timestamp = metadata
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);

                let report_type = if filename.starts_with("hpo") {
                    "hpo".to_string()
                } else if filename.starts_with("paper") {
                    "paper_trading".to_string()
                } else {
                    "backtest".to_string()
                };

                // Best-effort extraction of basic metrics
                let mut symbol = None;
                let mut net_profit = None;
                let mut win_rate = None;
                let mut sortino_ratio = None;
                let mut total_trades = None;

                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                        symbol = val.get("symbol").and_then(|v| v.as_str()).map(String::from);
                        if let Some(metrics) = val.get("metrics") {
                            net_profit = metrics
                                .get("net_profit")
                                .and_then(|v| v.as_str())
                                .map(String::from);
                            win_rate = metrics
                                .get("win_rate")
                                .and_then(|v| v.as_str())
                                .map(String::from);
                            sortino_ratio = metrics
                                .get("sortino_ratio")
                                .and_then(|v| v.as_str())
                                .map(String::from);
                            total_trades = metrics
                                .get("total_trades")
                                .and_then(|v| v.as_u64())
                                .map(|n| n as usize);
                        }
                    }
                }

                summaries.push(ReportSummary {
                    filename,
                    report_type,
                    size_bytes,
                    modified_timestamp,
                    symbol,
                    net_profit,
                    win_rate,
                    sortino_ratio,
                    total_trades,
                });
            }
        }
    }

    // Sort newest first
    summaries.sort_by_key(|a| std::cmp::Reverse(a.modified_timestamp));
    Json(summaries)
}

/// GET /api/reports/:id - Retrieves full JSON report with directory traversal protection.
pub async fn get_report_by_id_handler(
    State(app_state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Validate filename to prevent path traversal
    if id.contains('/') || id.contains('\\') || id.contains("..") {
        return Err((
            StatusCode::BAD_REQUEST,
            "Invalid report identifier".to_string(),
        ));
    }

    let filename = if id.ends_with(".json") {
        id
    } else {
        format!("{id}.json")
    };

    let file_path = app_state.reports_dir.join(&filename);
    if !file_path.exists() || !file_path.is_file() {
        return Err((
            StatusCode::NOT_FOUND,
            format!("Report '{filename}' not found"),
        ));
    }

    let content = fs::read_to_string(&file_path).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed reading file: {e}"),
        )
    })?;

    let json_val = serde_json::from_str::<serde_json::Value>(&content).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Invalid JSON structure: {e}"),
        )
    })?;

    Ok(Json(json_val))
}
