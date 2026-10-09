-- =============================================================================
-- ATSNT Quantitative Trading Engine - BigQuery DDL Schema (`atsnt_bi`)
--
-- Provisions analytical tables and views for Google Looker Studio BI integration.
-- Run this script using `bq query --use_legacy_sql=false < bigquery_schema.sql`
-- or through the Google Cloud BigQuery Console.
-- =============================================================================

CREATE SCHEMA IF NOT EXISTS `atsnt_bi`
OPTIONS (
    description = "ATSNT Quantitative Algorithmic Trading Analytics & BI Warehouse",
    location = "europe-southwest1"
);

-- -----------------------------------------------------------------------------
-- 1. Table: `trades` (Execution Audit & Trade Journal)
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS `atsnt_bi.trades` (
    trade_id STRING NOT NULL,
    session_id STRING NOT NULL,
    strategy_id STRING NOT NULL,
    symbol STRING NOT NULL,
    side STRING NOT NULL,               -- 'Long' | 'Short' | 'ClosedPosition'
    entry_timestamp TIMESTAMP NOT NULL,
    exit_timestamp TIMESTAMP NOT NULL,
    entry_price NUMERIC NOT NULL,
    exit_price NUMERIC NOT NULL,
    quantity NUMERIC NOT NULL,
    gross_pnl NUMERIC NOT NULL,
    fees_paid NUMERIC NOT NULL,
    net_pnl NUMERIC NOT NULL,
    exit_reason STRING NOT NULL,        -- 'TakeProfit' | 'StopLoss' | 'TimeBarrier'
    holding_duration_seconds INT64
)
PARTITION BY DATE(exit_timestamp)
CLUSTER BY symbol, strategy_id
OPTIONS (
    description = "Individual closed trades with execution prices, fees, slippage, and attribution"
);

-- -----------------------------------------------------------------------------
-- 2. Table: `equity_snapshots` (Real-Time Account Valuation & Drawdown)
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS `atsnt_bi.equity_snapshots` (
    timestamp TIMESTAMP NOT NULL,
    session_id STRING NOT NULL,
    symbol STRING NOT NULL,
    cash_equity NUMERIC NOT NULL,
    unrealized_pnl NUMERIC NOT NULL,
    total_equity NUMERIC NOT NULL,
    drawdown_pct NUMERIC NOT NULL,
    active_position_side STRING,         -- 'Long' | 'Short' | NULL
    active_position_qty NUMERIC
)
PARTITION BY DATE(timestamp)
CLUSTER BY session_id
OPTIONS (
    description = "Periodic mark-to-market valuations and live drawdown curves for paper/live trading"
);

-- -----------------------------------------------------------------------------
-- 3. Table: `hpo_evaluations` (Hyperparameter Optimization Surface)
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS `atsnt_bi.hpo_evaluations` (
    run_id STRING NOT NULL,
    timestamp TIMESTAMP NOT NULL,
    strategy_id STRING NOT NULL,
    parameter_space STRING NOT NULL,     -- JSON configuration
    is_sharpe NUMERIC NOT NULL,
    oos_sharpe NUMERIC NOT NULL,
    deflated_sharpe_ratio NUMERIC NOT NULL,
    parameter_stability_score NUMERIC NOT NULL,
    total_trades INT64 NOT NULL,
    win_rate NUMERIC NOT NULL,
    max_drawdown_pct NUMERIC NOT NULL
)
PARTITION BY DATE(timestamp)
CLUSTER BY strategy_id
OPTIONS (
    description = "Walk-Forward parameter evaluations, Deflated Sharpe Ratio, and stability plateau scores"
);

-- -----------------------------------------------------------------------------
-- 4. Table: `monte_carlo_runs` (Quantitative Stress Testing Summary)
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS `atsnt_bi.monte_carlo_runs` (
    run_id STRING NOT NULL,
    timestamp TIMESTAMP NOT NULL,
    strategy_id STRING NOT NULL,
    iterations INT64 NOT NULL,
    resample_method STRING NOT NULL,     -- 'CircularBlockBootstrap' | 'IID'
    historical_max_drawdown NUMERIC NOT NULL,
    p50_max_drawdown NUMERIC NOT NULL,
    p95_max_drawdown NUMERIC NOT NULL,
    p99_max_drawdown NUMERIC NOT NULL,
    probability_of_ruin_pct NUMERIC NOT NULL
)
PARTITION BY DATE(timestamp)
CLUSTER BY strategy_id
OPTIONS (
    description = "Discrete Event Monte Carlo stress simulation results and ruin probability metrics"
);
-- =============================================================================
-- ANALYTICAL VIEWS FOR GOOGLE LOOKER STUDIO (DEDUPLICATED & PARTITION-AWARE)
-- =============================================================================

-- -----------------------------------------------------------------------------
-- View: `v_trades` (Clean, Idempotent Trade Audit Records)
-- -----------------------------------------------------------------------------
CREATE OR REPLACE VIEW `atsnt_bi.v_trades` AS
SELECT * EXCEPT(row_num)
FROM (
    SELECT
        *,
        ROW_NUMBER() OVER(PARTITION BY trade_id ORDER BY exit_timestamp DESC) AS row_num
    FROM `atsnt_bi.trades`
)
WHERE row_num = 1;

-- -----------------------------------------------------------------------------
-- View: `v_live_paper_monitor` (Real-Time Engine Monitor - Latest Mark-to-Market)
-- -----------------------------------------------------------------------------
CREATE OR REPLACE VIEW `atsnt_bi.v_live_paper_monitor` AS
SELECT * EXCEPT(row_num)
FROM (
    SELECT
        timestamp,
        session_id,
        symbol,
        cash_equity,
        unrealized_pnl,
        total_equity,
        drawdown_pct,
        active_position_side,
        active_position_qty,
        ROW_NUMBER() OVER(PARTITION BY session_id ORDER BY timestamp DESC) AS row_num
    FROM `atsnt_bi.equity_snapshots`
)
WHERE row_num = 1;
