#!/usr/bin/env bash
# =============================================================================
# Argument-resolution tests for scripts/run_production_pipeline.sh.
#
# Runs the pipeline with --dry-run (prints resolved commands, executes nothing)
# against throwaway data directories. No network, no cargo build, no binaries.
#
# Usage: bash scripts/tests/pipeline_args_test.sh
# =============================================================================

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PIPELINE="${SCRIPT_DIR}/../run_production_pipeline.sh"

WORK_DIR="$(mktemp -d "${TMPDIR:-${HOME}/.cache}/atsnt-pipeline-test.XXXXXX")"
trap 'rm -rf "${WORK_DIR}"' EXIT

DATA_ROOT="${WORK_DIR}/data"
mkdir -p "${DATA_ROOT}/BTCUSDT"
touch "${DATA_ROOT}/BTCUSDT/BTCUSDT-aggTrades-2026-01.parquet"
touch "${DATA_ROOT}/BTCUSDT/BTCUSDT-aggTrades-2026-02.parquet"

PASSED=0
FAILED=0
OUT=""
STATUS=0

run() {
    OUT="$(bash "${PIPELINE}" --no-inhibit --dry-run --year 2026 --output-dir "${DATA_ROOT}" "$@" 2>&1)"
    STATUS=$?
}

check() {
    local name="$1" condition="$2"
    if eval "${condition}"; then
        PASSED=$((PASSED + 1))
        echo "ok   - ${name}"
    else
        FAILED=$((FAILED + 1))
        echo "FAIL - ${name}"
        echo "       condition: ${condition}"
        echo "${OUT}" | sed 's/^/       | /'
    fi
}

has() { grep -qF -- "$1" <<< "${OUT}"; }
line_with() { grep -F -- "$1" <<< "${OUT}" | head -1; }

MONTH_FILE="${DATA_ROOT}/BTCUSDT/BTCUSDT-aggTrades-2026-01.parquet"

# --month resolves to the exact monthly Parquet file for HPO and backtest.
run --month 1 --skip-fetch
check "month: exit 0" '[ "${STATUS}" -eq 0 ]'
check "month: run_hpo reads the monthly file" '[[ "$(line_with "/run_hpo ")" == *"--data ${MONTH_FILE} "* ]]'
check "month: backtest reads the monthly file" '[[ "$(line_with "backtest --data")" == *"--data ${MONTH_FILE} "* ]]'
check "month: default config name includes the month" 'has "--output-config configs/hpo_btcusdt_2026_01.json"'
check "month: default log name includes the month" 'has "Log File:        pipeline_btcusdt_2026_01.log"'
check "month: nothing is executed" 'has "[DRY-RUN] No commands were executed."'

# Zero-padded and unpadded months resolve to the same file.
run --month 01 --skip-fetch
check "month 01: same monthly file" '[[ "$(line_with "/run_hpo ")" == *"--data ${MONTH_FILE} "* ]]'

# Without --month the whole symbol directory is used, as before.
run --skip-fetch
check "no month: exit 0" '[ "${STATUS}" -eq 0 ]'
check "no month: run_hpo reads the directory" '[[ "$(line_with "/run_hpo ")" == *"--data ${DATA_ROOT}/BTCUSDT "* ]]'
check "no month: year-only config name unchanged" 'has "--output-config configs/hpo_btcusdt_2026.json"'
check "no month: year-only log name unchanged" 'has "Log File:        pipeline_btcusdt_2026.log"'

# A missing monthly file fails fast instead of silently reading other months.
run --month 3 --skip-fetch
check "missing month: non-zero exit" '[ "${STATUS}" -ne 0 ]'
check "missing month: explains the missing file" 'has "BTCUSDT-aggTrades-2026-03.parquet not found"'
check "missing month: no HPO command printed" '! has "/run_hpo "'

# Invalid months are rejected.
run --month 13 --skip-fetch
check "invalid month: non-zero exit" '[ "${STATUS}" -ne 0 ]'
check "invalid month: error message" 'has "--month must be between 1 and 12"'

# The fetch stage passes --month to fetch_data and defers the file check to after fetching.
run --month 3
check "fetch: exit 0 in dry-run (file is produced by fetch)" '[ "${STATUS}" -eq 0 ]'
check "fetch: fetch_data receives --month" '[[ "$(line_with "/fetch_data ")" == *"--month 3"* ]]'

# Default build step versus prebuilt binaries.
run --skip-fetch
check "build: cargo build is planned by default" 'has "[DRY-RUN] cargo build --release"'
check "build: default binaries from target/release" 'has "target/release/run_hpo "'
run --skip-fetch --bin-dir /opt/atsnt/bin
check "bin-dir: cargo build skipped" '! has "cargo build"'
check "bin-dir: run_hpo from bin dir" 'has "/opt/atsnt/bin/run_hpo "'
check "bin-dir: backtest from bin dir" 'has "/opt/atsnt/bin/backtest "'

# New passthroughs.
run --skip-fetch --gcp-bigquery --strict-sinks --upload-winner --artifact-suffix hpo-42
check "strict: run_hpo gets --strict-sinks" '[[ "$(line_with "/run_hpo ")" == *"--strict-sinks"* ]]'
check "strict: backtest gets --strict-sinks" '[[ "$(line_with "backtest --data")" == *"--strict-sinks"* ]]'
check "upload: run_hpo gets --gcs-upload" '[[ "$(line_with "/run_hpo ")" == *"--gcs-upload"* ]]'
check "upload: run_hpo gets --artifact-suffix" '[[ "$(line_with "/run_hpo ")" == *"--artifact-suffix hpo-42"* ]]'
check "bigquery: both stages get --gcp-bigquery" '[[ "$(line_with "/run_hpo ")" == *"--gcp-bigquery"* && "$(line_with "backtest --data")" == *"--gcp-bigquery"* ]]'

# Existing flags keep their behavior.
run --skip-fetch
check "defaults: no strict/upload flags" '! has "--strict-sinks" && ! has "--gcs-upload"'
run --skip-fetch --capital 200 --candidates 7 --strategy order_flow_imbalance
check "capital: passed to run_hpo" '[[ "$(line_with "/run_hpo ")" == *"--capital 200"* ]]'
check "capital: passed to backtest" '[[ "$(line_with "backtest --data")" == *"--capital 200"* ]]'
check "candidates: passed to run_hpo" '[[ "$(line_with "/run_hpo ")" == *"--candidates 7"* ]]'
check "strategy: passed to both stages" '[[ "$(line_with "/run_hpo ")" == *"--strategy order_flow_imbalance"* && "$(line_with "backtest --data")" == *"--strategy order_flow_imbalance"* ]]'
check "skip-fetch: ETL reported as skipped" 'has "[1/3] [SKIPPED] Historical ETL"'

run --skip-fetch --execution-context configs/execution_real_500eur.json --candidates 7
check "execution-context: passed to run_hpo" '[[ "$(line_with "/run_hpo ")" == *"--execution-context configs/execution_real_500eur.json"* ]]'
check "execution-context: the backtest adopts it from the artifact" '[[ "$(line_with "backtest --data")" != *"--execution-context"* ]]'

run --skip-fetch --skip-hpo --config "${WORK_DIR}/missing.json"
check "skip-hpo: missing config still fails" '[ "${STATUS}" -ne 0 ] && has "not found"'
touch "${WORK_DIR}/winner.json"
run --skip-fetch --skip-hpo --config "${WORK_DIR}/winner.json"
check "skip-hpo: existing config used by backtest" '! has "/run_hpo " && [[ "$(line_with "backtest --data")" == *"--config ${WORK_DIR}/winner.json"* ]]'

run --skip-fetch --detach
check "detach: dry-run takes precedence (nothing dispatched)" '[ "${STATUS}" -eq 0 ] && ! has "Process dispatched"'

run --bogus-flag
check "unknown option: still rejected" '[ "${STATUS}" -ne 0 ] && has "Unknown option: --bogus-flag"'

echo ""
echo "${PASSED} passed, ${FAILED} failed"
[ "${FAILED}" -eq 0 ]
