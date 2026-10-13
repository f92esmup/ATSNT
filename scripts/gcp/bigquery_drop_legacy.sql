-- =============================================================================
-- ATSNT - One-off removal of the pre-run-model BigQuery objects (`atsnt_bi`).
--
-- DESTRUCTIVE: deletes the legacy tables and their data. Run once, before
-- `bigquery_schema.sql`, on datasets created with the old model:
--   bq query --use_legacy_sql=false < scripts/gcp/bigquery_drop_legacy.sql
--   bq query --use_legacy_sql=false < scripts/gcp/bigquery_schema.sql
-- The legacy `trades` table is dropped because the run model recreates it
-- with a different schema (run_id, run_type, MAE/MFE columns).
-- =============================================================================

DROP VIEW IF EXISTS `atsnt_bi.v_live_paper_monitor`;
DROP VIEW IF EXISTS `atsnt_bi.v_trades`;
DROP TABLE IF EXISTS `atsnt_bi.equity_snapshots`;
DROP TABLE IF EXISTS `atsnt_bi.hpo_evaluations`;
DROP TABLE IF EXISTS `atsnt_bi.monte_carlo_runs`;
DROP TABLE IF EXISTS `atsnt_bi.trades`;
