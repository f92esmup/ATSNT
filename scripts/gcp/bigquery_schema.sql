-- =============================================================================
-- ATSNT Quantitative Trading Engine - BigQuery DDL Schema (`atsnt_bi`)
--
-- Run-centric analytics model. Every execution mode (backtest, hpo, hpo_trial,
-- monte_carlo, paper, live) writes the same four append-only tables keyed by
-- `run_id`; Looker Studio reads only the `v_*` views.
--
-- Run with `bq query --use_legacy_sql=false < bigquery_schema.sql`
-- (scripts/gcp/setup_gcp_resources.sh does this). Legacy objects from the
-- previous model are removed by `bigquery_drop_legacy.sql`, which must run
-- first on datasets created before the run model.
-- =============================================================================

CREATE SCHEMA IF NOT EXISTS `atsnt_bi`
OPTIONS (
    description = "ATSNT Quantitative Algorithmic Trading Analytics & BI Warehouse",
    location = "europe-southwest1"
);

-- -----------------------------------------------------------------------------
-- 1. Table: `runs` (what ran: spec, lineage, lifecycle)
--    Append-only: a run appends one row per status change (running ->
--    completed | failed | stopped). `v_runs` keeps the latest row per run.
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS `atsnt_bi.runs` (
    run_id STRING NOT NULL,             -- UUID v7 (time ordered)
    parent_run_id STRING,               -- hpo -> hpo_trial, backtest -> monte_carlo
    run_type STRING NOT NULL,           -- backtest | hpo | hpo_trial | monte_carlo | paper | live
    status STRING NOT NULL,             -- running | completed | failed | stopped
    recorded_at TIMESTAMP NOT NULL,
    started_at TIMESTAMP NOT NULL,
    finished_at TIMESTAMP,
    strategy_id STRING NOT NULL,
    symbol STRING NOT NULL,
    market_type STRING NOT NULL,
    sampling JSON NOT NULL,             -- e.g. {"type":"dollar_bar","threshold":"1000000"}
    params JSON NOT NULL,               -- strategy hyperparameters (schema-free per strategy)
    execution_context JSON NOT NULL,    -- capital, maker/taker fees, slippage, risk
    data_source STRING,
    data_start TIMESTAMP,
    data_end TIMESTAMP,
    git_sha STRING
)
PARTITION BY DATE(started_at)
CLUSTER BY run_type, strategy_id, run_id
OPTIONS (description = "Run lifecycle and full execution spec; append-only, latest row per run_id wins");

-- -----------------------------------------------------------------------------
-- 2. Table: `run_metrics` (strategy x-ray per run and scope)
--    Trade metrics are NULL for run types without trades (monte_carlo, hpo).
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS `atsnt_bi.run_metrics` (
    run_id STRING NOT NULL,
    run_type STRING NOT NULL,
    scope STRING NOT NULL,              -- full | is | oos | fold_<n>
    recorded_at TIMESTAMP NOT NULL,
    total_trades INT64,
    winning_trades INT64,
    losing_trades INT64,
    win_rate NUMERIC,
    gross_profit NUMERIC,
    gross_loss NUMERIC,
    net_profit NUMERIC,
    total_fees NUMERIC,
    total_slippage NUMERIC,
    profit_factor NUMERIC,
    payoff_ratio NUMERIC,
    mathematical_expectancy NUMERIC,
    max_drawdown_amount NUMERIC,
    max_drawdown_pct NUMERIC,
    sortino_ratio NUMERIC,
    fee_drag_pct NUMERIC,
    avg_holding_seconds NUMERIC,
    avg_mae_pct NUMERIC,
    avg_mfe_pct NUMERIC,
    exit_reason_counts JSON,            -- {"TakeProfit": 12, "StopLoss": 5, ...}
    extras JSON                         -- dsr_p_value, stability_score, probability_of_ruin_pct, ...
)
CLUSTER BY run_type, run_id
OPTIONS (description = "Performance metrics per run and evaluation scope; run-type specifics in extras");

-- -----------------------------------------------------------------------------
-- 3. Table: `trades` (closed trades with equity excursion)
--    Written for backtest, paper and live runs only.
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS `atsnt_bi.trades` (
    trade_id STRING NOT NULL,           -- <run_id>_t<00001>
    run_id STRING NOT NULL,
    run_type STRING NOT NULL,
    strategy_id STRING NOT NULL,
    symbol STRING NOT NULL,
    side STRING NOT NULL,               -- Long | Short
    entry_timestamp TIMESTAMP NOT NULL,
    exit_timestamp TIMESTAMP NOT NULL,
    entry_price NUMERIC NOT NULL,
    exit_price NUMERIC NOT NULL,
    quantity NUMERIC NOT NULL,
    gross_pnl NUMERIC NOT NULL,
    fees_paid NUMERIC NOT NULL,
    slippage_paid NUMERIC NOT NULL,
    net_pnl NUMERIC NOT NULL,
    return_pct NUMERIC NOT NULL,
    exit_reason STRING NOT NULL,        -- TakeProfit | StopLoss | TimeBarrier | EndOfSimulation | ...
    holding_duration_seconds INT64 NOT NULL,
    entry_equity NUMERIC NOT NULL,
    trough_equity NUMERIC NOT NULL,     -- maximum adverse excursion (MAE)
    trough_timestamp TIMESTAMP NOT NULL,
    peak_equity NUMERIC NOT NULL,       -- maximum favorable excursion (MFE)
    peak_timestamp TIMESTAMP NOT NULL,
    exit_equity NUMERIC NOT NULL,
    max_drawdown_amount NUMERIC NOT NULL, -- deepest session drawdown while open
    max_drawdown_pct NUMERIC NOT NULL
)
PARTITION BY DATE(exit_timestamp)
CLUSTER BY run_type, run_id
OPTIONS (description = "Closed trades with MAE/MFE; max of max_drawdown_* per run equals the engine max drawdown");

-- -----------------------------------------------------------------------------
-- 4. Table: `run_series` (event-based series, never time-sampled)
--    equity: entry/trough/peak/exit per trade. mc_max_drawdown: fan chart.
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS `atsnt_bi.run_series` (
    run_id STRING NOT NULL,
    run_type STRING NOT NULL,
    series STRING NOT NULL,             -- equity | mc_max_drawdown | ...
    event STRING NOT NULL,              -- entry | trough | peak | exit | p05 | p50 | p95 | ...
    ts TIMESTAMP,                       -- time-indexed series
    x INT64,                            -- ordinal series (trade number in a fan chart)
    value NUMERIC NOT NULL,
    trade_id STRING
)
CLUSTER BY run_id, series
OPTIONS (description = "Event-based series points keyed by run_id");

-- =============================================================================
-- ANALYTICAL VIEWS FOR LOOKER STUDIO (deduplicated, latest-row semantics)
-- =============================================================================

-- Latest metrics row per (run_id, scope).
CREATE OR REPLACE VIEW `atsnt_bi.v_run_metrics` AS
SELECT * EXCEPT(row_num)
FROM (
    SELECT
        *,
        ROW_NUMBER() OVER (PARTITION BY run_id, scope ORDER BY recorded_at DESC) AS row_num
    FROM `atsnt_bi.run_metrics`
)
WHERE row_num = 1;

-- One row per run: latest lifecycle row joined with its `full` metrics and
-- the most used spec fields flattened for filtering.
CREATE OR REPLACE VIEW `atsnt_bi.v_runs` AS
WITH latest_runs AS (
    SELECT * EXCEPT(row_num)
    FROM (
        SELECT
            *,
            ROW_NUMBER() OVER (PARTITION BY run_id ORDER BY recorded_at DESC) AS row_num
        FROM `atsnt_bi.runs`
    )
    WHERE row_num = 1
)
SELECT
    r.*,
    TIMESTAMP_DIFF(COALESCE(r.finished_at, CURRENT_TIMESTAMP()), r.started_at, SECOND)
        AS duration_seconds,
    SAFE_CAST(JSON_VALUE(r.sampling, '$.threshold') AS NUMERIC) AS dollar_bar_threshold,
    SAFE_CAST(JSON_VALUE(r.execution_context, '$.initial_capital') AS NUMERIC) AS initial_capital,
    SAFE_CAST(JSON_VALUE(r.execution_context, '$.taker_fee_pct') AS NUMERIC) AS taker_fee_pct,
    SAFE_CAST(JSON_VALUE(r.execution_context, '$.maker_fee_pct') AS NUMERIC) AS maker_fee_pct,
    SAFE_CAST(JSON_VALUE(r.execution_context, '$.slippage_pct') AS NUMERIC) AS slippage_pct,
    m.* EXCEPT(run_id, run_type, scope, recorded_at)
FROM latest_runs AS r
LEFT JOIN `atsnt_bi.v_run_metrics` AS m
    ON m.run_id = r.run_id AND m.scope = 'full';

-- Deduplicated trades (insertAll may deliver retries twice).
CREATE OR REPLACE VIEW `atsnt_bi.v_trades` AS
SELECT * EXCEPT(row_num)
FROM (
    SELECT
        *,
        ROW_NUMBER() OVER (PARTITION BY trade_id ORDER BY exit_timestamp DESC) AS row_num
    FROM `atsnt_bi.trades`
)
WHERE row_num = 1;

-- Deduplicated series points.
CREATE OR REPLACE VIEW `atsnt_bi.v_series` AS
SELECT DISTINCT run_id, run_type, series, event, ts, x, value, trade_id
FROM `atsnt_bi.run_series`;

-- Event-based equity curve with drawdown derived from the running peak
-- (starting at initial capital). The exact max drawdown of a run is
-- `v_runs.max_drawdown_pct`; this view is for plotting the curve.
CREATE OR REPLACE VIEW `atsnt_bi.v_equity` AS
WITH points AS (
    SELECT
        s.run_id,
        s.run_type,
        s.ts,
        s.event,
        s.trade_id,
        s.value AS equity,
        CASE s.event WHEN 'entry' THEN 0 WHEN 'exit' THEN 2 ELSE 1 END AS event_rank,
        SAFE_CAST(JSON_VALUE(r.execution_context, '$.initial_capital') AS NUMERIC)
            AS initial_capital
    FROM `atsnt_bi.v_series` AS s
    LEFT JOIN `atsnt_bi.v_runs` AS r USING (run_id)
    WHERE s.series = 'equity'
),
peaks AS (
    SELECT
        *,
        GREATEST(
            COALESCE(initial_capital, equity),
            MAX(equity) OVER (
                PARTITION BY run_id
                ORDER BY ts, trade_id, event_rank
                ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
            )
        ) AS running_peak
    FROM points
)
SELECT
    run_id,
    run_type,
    ts,
    event,
    trade_id,
    equity,
    running_peak,
    running_peak - equity AS drawdown_amount,
    SAFE_DIVIDE(running_peak - equity, running_peak) AS drawdown_pct
FROM peaks;

-- Latest equity point per run (paper/live monitor) with run status.
CREATE OR REPLACE VIEW `atsnt_bi.v_latest_equity` AS
SELECT
    e.* EXCEPT(row_num),
    r.status,
    r.strategy_id,
    r.symbol
FROM (
    SELECT
        *,
        ROW_NUMBER() OVER (PARTITION BY run_id ORDER BY ts DESC, event DESC) AS row_num
    FROM `atsnt_bi.v_equity`
) AS e
JOIN `atsnt_bi.v_runs` AS r USING (run_id)
WHERE e.row_num = 1;

-- One row per HPO trial: its parameters (JSON string, strategy agnostic) and
-- the ranking scores stored in its `full` extras row.
CREATE OR REPLACE VIEW `atsnt_bi.v_hpo_trials` AS
SELECT
    r.run_id,
    r.parent_run_id AS hpo_run_id,
    r.strategy_id,
    r.symbol,
    r.started_at,
    TO_JSON_STRING(r.params) AS params,
    SAFE_CAST(JSON_VALUE(m.extras, '$.rank') AS INT64) AS rank,
    SAFE_CAST(JSON_VALUE(m.extras, '$.mean_is_sortino') AS NUMERIC) AS mean_is_sortino,
    SAFE_CAST(JSON_VALUE(m.extras, '$.mean_oos_sortino') AS NUMERIC) AS mean_oos_sortino,
    SAFE_CAST(JSON_VALUE(m.extras, '$.mean_oos_profit_factor') AS NUMERIC) AS mean_oos_profit_factor,
    SAFE_CAST(JSON_VALUE(m.extras, '$.total_oos_trades') AS INT64) AS total_oos_trades,
    SAFE_CAST(JSON_VALUE(m.extras, '$.stability_score') AS NUMERIC) AS stability_score,
    SAFE_CAST(JSON_VALUE(m.extras, '$.fitness') AS NUMERIC) AS fitness,
    SAFE_CAST(JSON_VALUE(m.extras, '$.dsr_p_value') AS NUMERIC) AS dsr_p_value,
    SAFE_CAST(JSON_VALUE(m.extras, '$.is_statistically_significant') AS BOOL)
        AS is_statistically_significant
FROM `atsnt_bi.v_runs` AS r
JOIN `atsnt_bi.v_run_metrics` AS m
    ON m.run_id = r.run_id AND m.scope = 'full'
WHERE r.run_type = 'hpo_trial';

-- Per-fold in-sample / out-of-sample metrics of every HPO trial.
CREATE OR REPLACE VIEW `atsnt_bi.v_hpo_folds` AS
SELECT
    m.run_id,
    r.parent_run_id AS hpo_run_id,
    SPLIT(m.scope, '_fold_')[SAFE_OFFSET(0)] AS sample,      -- is | oos
    SAFE_CAST(SPLIT(m.scope, '_fold_')[SAFE_OFFSET(1)] AS INT64) AS fold_idx,
    m.* EXCEPT(run_id, run_type, scope, recorded_at, extras)
FROM `atsnt_bi.v_run_metrics` AS m
JOIN `atsnt_bi.v_runs` AS r USING (run_id)
WHERE m.scope LIKE '%\\_fold\\_%';

-- One row per Monte Carlo run with its stress percentiles flattened.
CREATE OR REPLACE VIEW `atsnt_bi.v_monte_carlo` AS
SELECT
    r.run_id,
    r.parent_run_id AS backtest_run_id,
    r.strategy_id,
    r.symbol,
    r.started_at,
    SAFE_CAST(JSON_VALUE(m.extras, '$.iterations') AS INT64) AS iterations,
    JSON_VALUE(m.extras, '$.resample_method') AS resample_method,
    SAFE_CAST(JSON_VALUE(m.extras, '$.block_size') AS INT64) AS block_size,
    SAFE_CAST(JSON_VALUE(m.extras, '$.ruin_threshold_pct') AS NUMERIC) AS ruin_threshold_pct,
    SAFE_CAST(JSON_VALUE(m.extras, '$.historical_max_drawdown_pct') AS NUMERIC)
        AS historical_max_drawdown_pct,
    SAFE_CAST(JSON_VALUE(m.extras, '$.p05_max_drawdown_pct') AS NUMERIC) AS p05_max_drawdown_pct,
    SAFE_CAST(JSON_VALUE(m.extras, '$.p50_max_drawdown_pct') AS NUMERIC) AS p50_max_drawdown_pct,
    SAFE_CAST(JSON_VALUE(m.extras, '$.p95_max_drawdown_pct') AS NUMERIC) AS p95_max_drawdown_pct,
    SAFE_CAST(JSON_VALUE(m.extras, '$.p99_max_drawdown_pct') AS NUMERIC) AS p99_max_drawdown_pct,
    SAFE_CAST(JSON_VALUE(m.extras, '$.worst_max_drawdown_pct') AS NUMERIC)
        AS worst_max_drawdown_pct,
    SAFE_CAST(JSON_VALUE(m.extras, '$.probability_of_ruin_pct') AS NUMERIC)
        AS probability_of_ruin_pct,
    SAFE_CAST(JSON_VALUE(m.extras, '$.median_underwater_trades') AS INT64)
        AS median_underwater_trades,
    SAFE_CAST(JSON_VALUE(m.extras, '$.p95_underwater_trades') AS INT64) AS p95_underwater_trades
FROM `atsnt_bi.v_runs` AS r
JOIN `atsnt_bi.v_run_metrics` AS m
    ON m.run_id = r.run_id AND m.scope = 'full'
WHERE r.run_type = 'monte_carlo';
