#!/usr/bin/env bash
# =============================================================================
# ATSNT - Automated End-to-End Production Staging Pipeline
#
# Chains together:
#   1. Binance Historical ETL (fetch_data) -> Compressed Apache Parquet
#   2. Parallel Walk-Forward HPO (run_hpo) -> configs/hpo_<symbol>_<year>.json
#   3. Deterministic Backtest + Monte Carlo (backtest) -> storage/reports/
#
# Automatically inhibits OS sleep, idle, and laptop lid-switch while running.
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${PROJECT_ROOT}"

# Default parameters
SYMBOL="BTCUSDT"
YEAR="$(date +%Y)"
MONTH=""
DOLLAR_BAR="1000000"
FOLDS="5"
TRAIN_RATIO="0.70"
EMBARGO_BARS="30"
CANDIDATES="60"
MC_ITERATIONS="10000"
OUTPUT_DIR=""
CONFIG_PATH=""
REPORT_PATH=""
LOG_FILE=""
GCP_BIGQUERY=false
SKIP_FETCH=false
SKIP_HPO=false
NO_INHIBIT=false
DETACH=false

show_help() {
    cat << EOF
ATSNT - Automated Production Staging Pipeline

Usage:
  $(basename "$0") [OPTIONS]

Options:
  -s, --symbol <SYM>         Trading pair symbol (default: ${SYMBOL})
  -y, --year <YYYY>          Year to process (default: current year ${YEAR})
  -m, --month <MM>           Optional single month to fetch (1-12)
  -b, --dollar-bar <AMOUNT>  Dollar bar threshold in USD (default: ${DOLLAR_BAR})
  -f, --folds <N>            Walk-Forward validation folds (default: ${FOLDS})
  -r, --train-ratio <FLOAT>  In-Sample train ratio (default: ${TRAIN_RATIO})
  -e, --embargo-bars <N>     Quarantined embargo bars between train/test (default: ${EMBARGO_BARS})
  -c, --candidates <N>       Parameter space candidates to evaluate in HPO (default: ${CANDIDATES})
  -i, --mc-iterations <N>    Monte Carlo bootstrap paths (default: ${MC_ITERATIONS})
  -o, --output-dir <DIR>     Root destination for Parquet dataset (default: data/historical_<YEAR>)
      --config <PATH>        Output/input path for winning HPO configuration JSON
      --report <PATH>        Output path for Monte Carlo telemetry report JSON
      --gcp-bigquery         Stream telemetry and closed trades to BigQuery
      --skip-fetch           Skip Phase 1 (use already downloaded data in output-dir)
      --skip-hpo             Skip Phase 2 (use existing --config JSON directly in Backtest)
      --no-inhibit           Do not invoke systemd-inhibit (allow OS to sleep)
  -d, --detach               Run entire pipeline in background (nohup)
  -l, --log <PATH>           Log file path when detached (default: pipeline_<symbol>_<year>.log)
  -h, --help                 Show this help message and exit

Examples:
  # 1. Standard full-year execution (auto sleep-inhibited):
  ./scripts/run_production_pipeline.sh --symbol BTCUSDT --year 2026

  # 2. Run in background (unattended with log tracking):
  ./scripts/run_production_pipeline.sh --symbol BTCUSDT --year 2026 --detach

  # 3. High-resolution stress test on existing data:
  ./scripts/run_production_pipeline.sh --skip-fetch --candidates 100 --mc-iterations 25000
EOF
}

# Parse CLI options
while [[ $# -gt 0 ]]; do
    case "$1" in
        -s|--symbol)
            SYMBOL="${2^^}"
            shift 2
            ;;
        -y|--year)
            YEAR="$2"
            shift 2
            ;;
        -m|--month)
            MONTH="$2"
            shift 2
            ;;
        -b|--dollar-bar)
            DOLLAR_BAR="$2"
            shift 2
            ;;
        -f|--folds)
            FOLDS="$2"
            shift 2
            ;;
        -r|--train-ratio)
            TRAIN_RATIO="$2"
            shift 2
            ;;
        -e|--embargo-bars)
            EMBARGO_BARS="$2"
            shift 2
            ;;
        -c|--candidates)
            CANDIDATES="$2"
            shift 2
            ;;
        -i|--mc-iterations)
            MC_ITERATIONS="$2"
            shift 2
            ;;
        -o|--output-dir)
            OUTPUT_DIR="$2"
            shift 2
            ;;
        --config)
            CONFIG_PATH="$2"
            shift 2
            ;;
        --report)
            REPORT_PATH="$2"
            shift 2
            ;;
        --gcp-bigquery)
            GCP_BIGQUERY=true
            shift
            ;;
        --skip-fetch)
            SKIP_FETCH=true
            shift
            ;;
        --skip-hpo)
            SKIP_HPO=true
            shift
            ;;
        --no-inhibit)
            NO_INHIBIT=true
            shift
            ;;
        -d|--detach)
            DETACH=true
            shift
            ;;
        -l|--log)
            LOG_FILE="$2"
            shift 2
            ;;
        -h|--help)
            show_help
            exit 0
            ;;
        *)
            echo "[ERROR] Unknown option: $1"
            echo "Run '$(basename "$0") --help' for usage."
            exit 1
            ;;
    esac
done

# Set dynamic default paths
if [ -z "${OUTPUT_DIR}" ]; then
    OUTPUT_DIR="data/historical_${YEAR}"
fi

if [ -z "${CONFIG_PATH}" ]; then
    CONFIG_PATH="configs/hpo_${SYMBOL,,}_${YEAR}.json"
fi

if [ -z "${REPORT_PATH}" ]; then
    REPORT_PATH="storage/reports/monte_carlo_${SYMBOL,,}_${YEAR}.json"
fi

if [ -z "${LOG_FILE}" ]; then
    LOG_FILE="pipeline_${SYMBOL,,}_${YEAR}.log"
fi

# Self-wrap with systemd-inhibit if not detached and inhibitor is available
if [ "${NO_INHIBIT}" = "false" ] && [ "${DETACH}" = "false" ] && [ -z "${SYSTEMD_INHIBITED:-}" ] && command -v systemd-inhibit >/dev/null 2>&1; then
    export SYSTEMD_INHIBITED=1
    echo "[*] Activating systemd-inhibit (sleep/idle/lid-switch locked for duration of pipeline)..."
    exec systemd-inhibit \
        --what=idle:sleep:handle-lid-switch \
        --who="ATSNT Production Pipeline" \
        --why="ETL, HPO Walk-Forward, and Monte Carlo" \
        "$0" \
        --symbol "${SYMBOL}" \
        --year "${YEAR}" \
        ${MONTH:+--month "${MONTH}"} \
        --dollar-bar "${DOLLAR_BAR}" \
        --folds "${FOLDS}" \
        --train-ratio "${TRAIN_RATIO}" \
        --embargo-bars "${EMBARGO_BARS}" \
        --candidates "${CANDIDATES}" \
        --mc-iterations "${MC_ITERATIONS}" \
        --output-dir "${OUTPUT_DIR}" \
        --config "${CONFIG_PATH}" \
        --report "${REPORT_PATH}" \
        $( [ "${GCP_BIGQUERY}" = "true" ] && echo "--gcp-bigquery" ) \
        $( [ "${SKIP_FETCH}" = "true" ] && echo "--skip-fetch" ) \
        $( [ "${SKIP_HPO}" = "true" ] && echo "--skip-hpo" ) \
        --no-inhibit
fi

# Handle background detach
if [ "${DETACH}" = "true" ]; then
    echo "============================================================"
    echo " ATSNT - Launching Pipeline in Background"
    echo "============================================================"
    echo "Symbol:       ${SYMBOL}"
    echo "Year:         ${YEAR}"
    echo "Log Output:   ${LOG_FILE}"
    echo "------------------------------------------------------------"

    INHIBIT_CMD=""
    if [ "${NO_INHIBIT}" = "false" ] && command -v systemd-inhibit >/dev/null 2>&1; then
        INHIBIT_CMD="systemd-inhibit --what=idle:sleep:handle-lid-switch --who=ATSNT --why=Pipeline"
    fi

    # Forward arguments excluding --detach
    nohup ${INHIBIT_CMD} "$0" \
        --symbol "${SYMBOL}" \
        --year "${YEAR}" \
        ${MONTH:+--month "${MONTH}"} \
        --dollar-bar "${DOLLAR_BAR}" \
        --folds "${FOLDS}" \
        --train-ratio "${TRAIN_RATIO}" \
        --embargo-bars "${EMBARGO_BARS}" \
        --candidates "${CANDIDATES}" \
        --mc-iterations "${MC_ITERATIONS}" \
        --output-dir "${OUTPUT_DIR}" \
        --config "${CONFIG_PATH}" \
        --report "${REPORT_PATH}" \
        $( [ "${GCP_BIGQUERY}" = "true" ] && echo "--gcp-bigquery" ) \
        $( [ "${SKIP_FETCH}" = "true" ] && echo "--skip-fetch" ) \
        $( [ "${SKIP_HPO}" = "true" ] && echo "--skip-hpo" ) \
        --no-inhibit > "${LOG_FILE}" 2>&1 &

    BG_PID=$!
    echo "[OK] Process dispatched with PID: ${BG_PID}"
    echo ""
    echo "To monitor progress in real-time, run:"
    echo "  tail -f ${LOG_FILE}"
    echo "============================================================"
    exit 0
fi

START_TOTAL=$(date +%s)

echo "============================================================"
echo " ATSNT - Automated End-to-End Production Staging Pipeline"
echo "============================================================"
echo "Symbol:          ${SYMBOL}"
echo "Year:            ${YEAR}"
[ -n "${MONTH}" ] && echo "Month:           ${MONTH}"
echo "Dollar Bar:      \$${DOLLAR_BAR}"
echo "Data Directory:  ${OUTPUT_DIR}/${SYMBOL}"
echo "HPO Config:      ${CONFIG_PATH}"
echo "Monte Carlo:     ${MC_ITERATIONS} iterations -> ${REPORT_PATH}"
echo "BigQuery Export: ${GCP_BIGQUERY}"
echo "------------------------------------------------------------"

# Step 0: Ensure binaries are compiled in release mode
echo "[0/3] Compiling optimized release binaries..."
cargo build --release \
    -p adapters --bin fetch_data \
    -p backtest --bin run_hpo \
    -p backtest --bin backtest
echo "      [OK] Binaries built successfully."

# Step 1: Binance ETL
DATA_PATH="${OUTPUT_DIR}/${SYMBOL}"
if [ "${SKIP_FETCH}" = "true" ]; then
    echo -e "\n[1/3] [SKIPPED] Historical ETL (Using existing data in ${DATA_PATH})"
else
    echo -e "\n[1/3] Running Historical ETL (Binance aggTrades -> Parquet)..."
    FETCH_ARGS=(
        "--symbol" "${SYMBOL}"
        "--year" "${YEAR}"
        "--output-dir" "${OUTPUT_DIR}"
    )
    if [ -n "${MONTH}" ]; then
        FETCH_ARGS+=("--month" "${MONTH}")
    fi
    ./target/release/fetch_data "${FETCH_ARGS[@]}"
fi

# Step 2: Parallel Walk-Forward HPO
if [ "${SKIP_HPO}" = "true" ]; then
    echo -e "\n[2/3] [SKIPPED] Walk-Forward HPO (Using existing config: ${CONFIG_PATH})"
    if [ ! -f "${CONFIG_PATH}" ]; then
        echo "[ERROR] Configuration file ${CONFIG_PATH} not found!"
        exit 1
    fi
else
    echo -e "\n[2/3] Running Parallel Walk-Forward HPO (Rayon multi-core)..."
    HPO_ARGS=(
        "--data" "${DATA_PATH}"
        "--dollar-bar" "${DOLLAR_BAR}"
        "--folds" "${FOLDS}"
        "--train-ratio" "${TRAIN_RATIO}"
        "--embargo-bars" "${EMBARGO_BARS}"
        "--candidates" "${CANDIDATES}"
        "--output-config" "${CONFIG_PATH}"
    )
    if [ "${GCP_BIGQUERY}" = "true" ]; then
        HPO_ARGS+=("--gcp-bigquery")
    fi
    ./target/release/run_hpo "${HPO_ARGS[@]}"
fi

# Step 3: Backtest + Discrete Event Monte Carlo Stress-Test
echo -e "\n[3/3] Running 1:1 Backtest & Monte Carlo Stress-Testing..."
BACKTEST_ARGS=(
    "--data" "${DATA_PATH}"
    "--dollar-bar" "${DOLLAR_BAR}"
    "--config" "${CONFIG_PATH}"
    "--monte-carlo"
    "--mc-iterations" "${MC_ITERATIONS}"
    "--report" "${REPORT_PATH}"
)
if [ "${GCP_BIGQUERY}" = "true" ]; then
    BACKTEST_ARGS+=("--gcp-bigquery")
fi
./target/release/backtest "${BACKTEST_ARGS[@]}"

END_TOTAL=$(date +%s)
TOTAL_DURATION=$((END_TOTAL - START_TOTAL))

echo ""
echo "============================================================"
echo " [SUCCESS] Entire Production Pipeline Completed!"
echo " Total Elapsed Time:   ${TOTAL_DURATION}s"
echo " Winning Parameters:   ${CONFIG_PATH}"
echo " Monte Carlo Audit:    ${REPORT_PATH}"
echo " Next Production Step: Run real-time paper trading:"
echo "   cargo run --release -p backtest --bin paper_trading -- \\"
echo "     --symbol ${SYMBOL,,} --config ${CONFIG_PATH} --dollar-bar ${DOLLAR_BAR}"
echo "============================================================"
