//! paper_trading and live_trading refuse an artifact whose HPO scorecard shows no edge.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn losing_artifact(dir: &Path) -> PathBuf {
    let path = dir.join("losing.json");
    let artifact = serde_json::json!({
        "schema_version": "1.0",
        "strategy_id": "dollar_bars_cusum",
        "symbol": "BTCUSDT",
        "market_type": "usd_m_perpetual",
        "sampling": {"type": "dollar_bar", "threshold": "1000000"},
        "strategy_config": {
            "rolling_window_len": 1600, "cusum_vol_multiplier": "0.8", "z_entry_threshold": "1.7",
            "z_stop_threshold": "5.0", "time_barrier_bars": 2000, "min_take_profit_pct": "0.005"
        },
        "execution_context": {
            "initial_capital": "562", "maker_fee_pct": "0.0002", "taker_fee_pct": "0.0005",
            "slippage_pct": "0.0005", "risk_per_trade_pct": "0.005", "max_daily_drawdown_pct": "0.03",
            "max_order_notional": "1700", "max_position_notional": "1700"
        },
        "hpo_scorecard": {
            "mean_is_sortino": "-0.3092", "mean_oos_sortino": "-0.3226",
            "mean_oos_profit_factor": "0.56", "oos_sharpe": "-0.29", "total_oos_trades": 778,
            "min_fold_trades": 100, "stability_score": "0", "fitness": "0",
            "dsr_p_value": "1.0000", "is_statistically_significant": false, "trials_evaluated": 120
        }
    });
    std::fs::write(&path, serde_json::to_string_pretty(&artifact).unwrap()).unwrap();
    path
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn run(binary: &str, config: &Path, extra: &[&str]) -> Output {
    Command::new(binary)
        .arg("--config")
        .arg(config)
        .args(extra)
        // No network in tests: the gate must fire before any connection is opened.
        .env("BINANCE_API_KEY", "test-key")
        .env("BINANCE_SECRET_KEY", "test-secret")
        .env_remove("GCP_PROJECT_ID")
        .output()
        .expect("run binary")
}

fn assert_refused(out: &Output) {
    let stderr = text(&out.stderr);
    assert!(
        !out.status.success(),
        "must refuse; stdout: {}",
        text(&out.stdout)
    );
    assert!(stderr.contains("not deployable"), "stderr: {stderr}");
    assert!(
        stderr.contains("no positive out-of-sample edge"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("--allow-unproven-strategy"),
        "stderr: {stderr}"
    );
}

#[test]
fn paper_trading_refuses_a_losing_artifact() {
    let dir = tempfile::tempdir().unwrap();
    let config = losing_artifact(dir.path());

    assert_refused(&run(env!("CARGO_BIN_EXE_paper_trading"), &config, &[]));
}

#[test]
fn live_trading_refuses_a_losing_artifact() {
    let dir = tempfile::tempdir().unwrap();
    let config = losing_artifact(dir.path());

    assert_refused(&run(
        env!("CARGO_BIN_EXE_live_trading"),
        &config,
        &["--api-key", "test-key", "--secret-key", "test-secret"],
    ));
}
