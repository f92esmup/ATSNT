#!/usr/bin/env bash
# =============================================================================
# ATSNT - Automated End-to-End Production Staging Pipeline
#
# Chains together:
#   1. Binance Historical ETL (fetch_data) -> Compressed Apache Parquet
#   2. Parallel Walk-Forward HPO (run_hpo) -> configs/hpo_<symbol>_<year>[_<MM>].json
#   3. Deterministic Backtest + Monte Carlo (backtest) -> BigQuery runs (with --gcp-bigquery)
#
# Automatically inhibits OS sleep, idle, and laptop lid-switch while running.
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${PROJECT_ROOT}"

# Default parameters
SYMBOL="BTCUSDT"
STRATEGY="dollar_bars_cusum"
PARAM_SPACE=""
EXECUTION_CONTEXT=""
MIN_TRADES_PER_FOLD=""
CAPITAL=""
YEAR="$(date +%Y)"
MONTH=""
HOLDOUT_FROM=""
DOLLAR_BAR="1000000"
FOLDS="5"
TRAIN_RATIO="0.70"
EMBARGO_BARS="30"
CANDIDATES="60"
MC_ITERATIONS="10000"
OUTPUT_DIR=""
CONFIG_PATH=""
LOG_FILE=""
GCP_BIGQUERY=false
SKIP_FETCH=false
SKIP_HPO=false
NO_INHIBIT=false
DETACH=false
DRY_RUN=false
BIN_DIR=""
STRICT_SINKS=false
UPLOAD_WINNER=false
ARTIFACT_SUFFIX=""

show_help() {
    cat << EOF
ATSNT - Automated Production Staging Pipeline

Usage:
  $(basename "$0") [OPTIONS]

Options:
  -s, --symbol <SYM>         Trading pair symbol (default: ${SYMBOL})
  -k, --capital <AMOUNT>     Starting capital in quote currency (e.g. 200, 1000)
  -y, --year <YYYY>          Year to process (default: current year ${YEAR})
  -m, --month <MM>           Optional single month (1-12): fetches it and runs HPO/backtest on that
                             month's Parquet file only (the current month is only published as
                             daily archives, so pick a completed month or omit --month)
      --holdout-from-month <MM>
                             Hold out month MM (2-12) and later from the HPO: the search reads only
                             the earlier months of --year, the backtest only the held-out ones
                             (daily archives of the current month included). Not with --month
  -b, --dollar-bar <AMOUNT>  Dollar bar threshold in USD (default: ${DOLLAR_BAR})
  -f, --folds <N>            Walk-Forward validation folds (default: ${FOLDS})
  -r, --train-ratio <FLOAT>  In-Sample train ratio (default: ${TRAIN_RATIO})
  -e, --embargo-bars <N>     Quarantined embargo bars between train/test (default: ${EMBARGO_BARS})
  -c, --candidates <N>       Parameter space candidates to evaluate in HPO (default: ${CANDIDATES})
  -i, --mc-iterations <N>    Monte Carlo bootstrap paths (default: ${MC_ITERATIONS})
  -o, --output-dir <DIR>     Root destination for Parquet dataset (default: data/historical_<YEAR>)
      --config <PATH>        Output/input path for winning HPO configuration JSON
      --strategy <NAME>      Target strategy identifier: 'dollar_bars_cusum' or 'order_flow_imbalance' (default: ${STRATEGY})
      --param-space <PATH>   Optional JSON path overriding default search space
      --execution-context <PATH>
                             Optional JSON execution context for the HPO (capital, fees, slippage,
                             risk, drawdown, leverage, notional caps); the backtest adopts it from
                             the winning artifact. --capital still overrides the capital
      --min-trades-per-fold <N>
                             Closed trades a candidate needs in every HPO fold to be ranked
                             (default: run_hpo's, 30); the HPO fails without an eligible one
      --gcp-bigquery         Record HPO, backtest and Monte Carlo runs to BigQuery
      --strict-sinks         Fail the run if any BigQuery recording or GCS upload fails
      --upload-winner        Upload the winning artifact to GCS (artifacts/<config stem>-<suffix>.json)
      --artifact-suffix <S>  Unique suffix for the uploaded artifact (default: UTC timestamp)
      --bin-dir <DIR>        Use prebuilt fetch_data/run_hpo/backtest from DIR (skips cargo build)
      --dry-run              Print the resolved commands without executing anything
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
        -k|--capital)
            CAPITAL="$2"
            shift 2
            ;;
        --holdout-from-month)
            HOLDOUT_FROM="$2"
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
        --strategy)
            STRATEGY="$2"
            shift 2
            ;;
        --param-space)
            PARAM_SPACE="$2"
            shift 2
            ;;
        --execution-context)
            EXECUTION_CONTEXT="$2"
            shift 2
            ;;
        --min-trades-per-fold)
            MIN_TRADES_PER_FOLD="$2"
            shift 2
            ;;
        --gcp-bigquery)
            GCP_BIGQUERY=true
            shift
            ;;
        --strict-sinks)
            STRICT_SINKS=true
            shift
            ;;
        --upload-winner)
            UPLOAD_WINNER=true
            shift
            ;;
        --artifact-suffix)
            ARTIFACT_SUFFIX="$2"
            shift 2
            ;;
        --bin-dir)
            BIN_DIR="$2"
            shift 2
            ;;
        --dry-run)
            DRY_RUN=true
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

# Validate and normalize --month (accepts 1 or 01)
MONTH_PADDED=""
if [ -n "${MONTH}" ]; then
    if ! [[ "${MONTH}" =~ ^[0-9]{1,2}$ ]] || [ $((10#${MONTH})) -lt 1 ] || [ $((10#${MONTH})) -gt 12 ]; then
        echo "[ERROR] --month must be between 1 and 12 (got '${MONTH}')"
        exit 1
    fi
    MONTH_PADDED="$(printf "%02d" "$((10#${MONTH}))")"
fi

HOLDOUT_PADDED=""
if [ -n "${HOLDOUT_FROM}" ]; then
    if [ -n "${MONTH}" ]; then
        echo "[ERROR] --holdout-from-month cannot be combined with --month (it splits the whole --year)"
        exit 1
    fi
    if ! [[ "${HOLDOUT_FROM}" =~ ^[0-9]{1,2}$ ]] || [ $((10#${HOLDOUT_FROM})) -lt 2 ] || [ $((10#${HOLDOUT_FROM})) -gt 12 ]; then
        echo "[ERROR] --holdout-from-month must be between 2 and 12 (got '${HOLDOUT_FROM}')"
        exit 1
    fi
    HOLDOUT_PADDED="$(printf "%02d" "$((10#${HOLDOUT_FROM}))")"
fi

# The execution context only feeds the HPO stage; with --skip-hpo it would be ignored silently.
if [ "${SKIP_HPO}" = "true" ] && [ -n "${EXECUTION_CONTEXT}" ]; then
    echo "[ERROR] --execution-context has no effect with --skip-hpo (the backtest adopts the context of the existing --config artifact)"
    exit 1
fi

if [ -n "${MIN_TRADES_PER_FOLD}" ]; then
    if ! [[ "${MIN_TRADES_PER_FOLD}" =~ ^[0-9]+$ ]]; then
        echo "[ERROR] --min-trades-per-fold must be a non-negative integer, got '${MIN_TRADES_PER_FOLD}'"
        exit 1
    fi
    if [ "${SKIP_HPO}" = "true" ]; then
        echo "[ERROR] --min-trades-per-fold has no effect with --skip-hpo (it only ranks HPO candidates)"
        exit 1
    fi
fi

# Set dynamic default paths (month-scoped when --month is given so runs never overwrite each other)
PERIOD="${YEAR}${MONTH_PADDED:+_${MONTH_PADDED}}"

if [ -z "${OUTPUT_DIR}" ]; then
    OUTPUT_DIR="data/historical_${YEAR}"
fi

if [ -z "${CONFIG_PATH}" ]; then
    CONFIG_PATH="configs/hpo_${SYMBOL,,}_${PERIOD}.json"
fi

if [ -z "${LOG_FILE}" ]; then
    LOG_FILE="pipeline_${SYMBOL,,}_${PERIOD}.log"
fi

# Data actually read by HPO and backtest: the exact monthly file with --month,
# otherwise every Parquet file of the symbol directory.
DATA_PATH="${OUTPUT_DIR}/${SYMBOL}"
TARGET_DATA="${DATA_PATH}"
if [ -n "${MONTH_PADDED}" ]; then
    TARGET_DATA="${DATA_PATH}/${SYMBOL}-aggTrades-${YEAR}-${MONTH_PADDED}.parquet"
fi
# With --holdout-from-month the stages read two directories of symlinks instead.
HPO_DATA="${TARGET_DATA}"
BACKTEST_DATA="${TARGET_DATA}"
SPLIT_ROOT=""
if [ -n "${HOLDOUT_PADDED}" ]; then
    SPLIT_ROOT="${OUTPUT_DIR}/splits/${SYMBOL}-${YEAR}-holdout-${HOLDOUT_PADDED}"
    HPO_DATA="${SPLIT_ROOT}/train"
    BACKTEST_DATA="${SPLIT_ROOT}/holdout"
fi

if [ -n "${BIN_DIR}" ]; then
    BIN_PATH="${BIN_DIR%/}"
else
    BIN_PATH="./target/release"
fi

# Arguments forwarded when re-executing under systemd-inhibit or nohup.
FORWARD_ARGS=(
    --symbol "${SYMBOL}"
    --year "${YEAR}"
    --dollar-bar "${DOLLAR_BAR}"
    --folds "${FOLDS}"
    --train-ratio "${TRAIN_RATIO}"
    --embargo-bars "${EMBARGO_BARS}"
    --candidates "${CANDIDATES}"
    --mc-iterations "${MC_ITERATIONS}"
    --output-dir "${OUTPUT_DIR}"
    --config "${CONFIG_PATH}"
    --strategy "${STRATEGY}"
)
[ -n "${MONTH}" ] && FORWARD_ARGS+=(--month "${MONTH}")
[ -n "${HOLDOUT_FROM}" ] && FORWARD_ARGS+=(--holdout-from-month "${HOLDOUT_FROM}")
[ -n "${PARAM_SPACE}" ] && FORWARD_ARGS+=(--param-space "${PARAM_SPACE}")
[ -n "${EXECUTION_CONTEXT}" ] && FORWARD_ARGS+=(--execution-context "${EXECUTION_CONTEXT}")
[ -n "${MIN_TRADES_PER_FOLD}" ] && FORWARD_ARGS+=(--min-trades-per-fold "${MIN_TRADES_PER_FOLD}")
[ -n "${CAPITAL}" ] && FORWARD_ARGS+=(--capital "${CAPITAL}")
[ -n "${BIN_DIR}" ] && FORWARD_ARGS+=(--bin-dir "${BIN_DIR}")
[ -n "${ARTIFACT_SUFFIX}" ] && FORWARD_ARGS+=(--artifact-suffix "${ARTIFACT_SUFFIX}")
[ "${GCP_BIGQUERY}" = "true" ] && FORWARD_ARGS+=(--gcp-bigquery)
[ "${STRICT_SINKS}" = "true" ] && FORWARD_ARGS+=(--strict-sinks)
[ "${UPLOAD_WINNER}" = "true" ] && FORWARD_ARGS+=(--upload-winner)
[ "${SKIP_FETCH}" = "true" ] && FORWARD_ARGS+=(--skip-fetch)
[ "${SKIP_HPO}" = "true" ] && FORWARD_ARGS+=(--skip-hpo)

# Self-wrap with systemd-inhibit if not detached and inhibitor is available
if [ "${DRY_RUN}" = "false" ] && [ "${NO_INHIBIT}" = "false" ] && [ "${DETACH}" = "false" ] && [ -z "${SYSTEMD_INHIBITED:-}" ] && command -v systemd-inhibit >/dev/null 2>&1; then
    export SYSTEMD_INHIBITED=1
    echo "[*] Activating systemd-inhibit (sleep/idle/lid-switch locked for duration of pipeline)..."
    exec systemd-inhibit \
        --what=idle:sleep:handle-lid-switch \
        --who="ATSNT Production Pipeline" \
        --why="ETL, HPO Walk-Forward, and Monte Carlo" \
        "$0" "${FORWARD_ARGS[@]}" --no-inhibit
fi

# Handle background detach
if [ "${DRY_RUN}" = "false" ] && [ "${DETACH}" = "true" ]; then
    echo "============================================================"
    echo " ATSNT - Launching Pipeline in Background"
    echo "============================================================"
    echo "Symbol:       ${SYMBOL}"
    echo "Strategy:     ${STRATEGY}"
    [ -n "${CAPITAL}" ] && echo "Capital:      \$${CAPITAL}"
    echo "Year:         ${YEAR}"
    echo "Log Output:   ${LOG_FILE}"
    echo "------------------------------------------------------------"

    CMD=(bash "${BASH_SOURCE[0]}")
    if [ "${NO_INHIBIT}" = "false" ] && command -v systemd-inhibit >/dev/null 2>&1; then
        CMD=(systemd-inhibit --what=idle:sleep:handle-lid-switch --who=ATSNT --why=Pipeline bash "${BASH_SOURCE[0]}")
    fi

    # Forward arguments excluding --detach
    nohup "${CMD[@]}" "${FORWARD_ARGS[@]}" --no-inhibit > "${LOG_FILE}" 2>&1 &

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
[ -n "${CAPITAL}" ] && echo "Capital:         \$${CAPITAL}"
echo "Data Directory:  ${OUTPUT_DIR}/${SYMBOL}"
[ -n "${MONTH_PADDED}" ] && echo "Data File:       ${TARGET_DATA}"
echo "HPO Config:      ${CONFIG_PATH}"
[ "${DRY_RUN}" = "true" ] && echo "Log File:        ${LOG_FILE}"
echo "Monte Carlo:     ${MC_ITERATIONS} iterations"
echo "BigQuery Export: ${GCP_BIGQUERY}"
echo "------------------------------------------------------------"

# Prints a greppable elapsed-time line for a pipeline stage started at epoch seconds $2.
stage_timing() {
    echo "[TIMING] stage=$1 seconds=$(( $(date +%s) - $2 ))"
}

# Runs a pipeline stage command, or only prints it under --dry-run.
run_stage() {
    local name="$1"
    shift
    if [ "${DRY_RUN}" = "true" ]; then
        echo "[DRY-RUN] $*"
        return 0
    fi
    local stage_start
    stage_start=$(date +%s)
    "$@"
    stage_timing "${name}" "${stage_start}"
}

# Splits the year's files by month into train/ and holdout/ symlink directories. In dry-run it
# only reports the split. Both sides must hold data, otherwise the run would be meaningless.
prepare_holdout_split() {
    [ -n "${HOLDOUT_PADDED}" ] || return 0
    local train=() holdout=() file name month
    for file in "${DATA_PATH}/${SYMBOL}-aggTrades-${YEAR}-"*.parquet; do
        [ -e "${file}" ] || continue
        name="$(basename "${file}")"
        if [[ "${name}" =~ ^${SYMBOL}-aggTrades-${YEAR}-([0-9]{2})(-[0-9]{2})?\.parquet$ ]]; then
            month="${BASH_REMATCH[1]}"
            if [ $((10#${month})) -lt $((10#${HOLDOUT_PADDED})) ]; then
                train+=("${file}")
            else
                holdout+=("${file}")
            fi
        fi
    done
    if [ "${#train[@]}" -eq 0 ]; then
        echo "[ERROR] --holdout-from-month ${HOLDOUT_PADDED}: no training data before month ${HOLDOUT_PADDED} in ${DATA_PATH}"
        exit 1
    fi
    if [ "${#holdout[@]}" -eq 0 ]; then
        echo "[ERROR] --holdout-from-month ${HOLDOUT_PADDED}: no held-out data from month ${HOLDOUT_PADDED} in ${DATA_PATH}"
        exit 1
    fi
    echo "[*] Holdout split: ${#train[@]} training file(s) (months 01-$(printf "%02d" $((10#${HOLDOUT_PADDED} - 1)))), ${#holdout[@]} held-out file(s) (months ${HOLDOUT_PADDED}-12)"
    if [ "${DRY_RUN}" = "true" ]; then
        echo "[DRY-RUN] would link them into ${SPLIT_ROOT}/{train,holdout}"
        return 0
    fi
    rm -rf "${SPLIT_ROOT}"
    mkdir -p "${SPLIT_ROOT}/train" "${SPLIT_ROOT}/holdout"
    for file in "${train[@]}"; do ln -s "$(cd "$(dirname "${file}")" && pwd)/$(basename "${file}")" "${SPLIT_ROOT}/train/"; done
    for file in "${holdout[@]}"; do ln -s "$(cd "$(dirname "${file}")" && pwd)/$(basename "${file}")" "${SPLIT_ROOT}/holdout/"; done
}

# Fails fast when --month points at a monthly file that does not exist, instead of
# silently reading other months.
require_target_data() {
    if [ -n "${MONTH_PADDED}" ] && [ ! -f "${TARGET_DATA}" ]; then
        echo "[ERROR] Monthly data file ${TARGET_DATA} not found."
        echo "        Monthly archives exist only for completed months (the current month is published as daily archives); use a completed month or omit --month."
        exit 1
    fi
}

# Step 0: Ensure binaries are compiled in release mode
if [ -n "${BIN_DIR}" ]; then
    echo "[0/3] [SKIPPED] Compilation (using prebuilt binaries in ${BIN_PATH})"
    if [ "${DRY_RUN}" = "false" ]; then
        for bin in fetch_data run_hpo backtest; do
            if [ ! -x "${BIN_PATH}/${bin}" ]; then
                echo "[ERROR] Prebuilt binary ${BIN_PATH}/${bin} not found or not executable."
                exit 1
            fi
        done
    fi
elif [ "${DRY_RUN}" = "true" ]; then
    echo "[0/3] Compiling optimized release binaries..."
    echo "[DRY-RUN] cargo build --release -p adapters --bin fetch_data -p backtest --bin run_hpo -p backtest --bin backtest"
else
    echo "[0/3] Compiling optimized release binaries..."
    cargo build --release \
        -p adapters --bin fetch_data \
        -p backtest --bin run_hpo \
        -p backtest --bin backtest
    echo "      [OK] Binaries built successfully."
fi

# Step 1: Binance ETL
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
    run_stage fetch "${BIN_PATH}/fetch_data" "${FETCH_ARGS[@]}"
fi

# With --month the HPO and backtest read exactly this file; it must exist now
# (in dry-run it can only be checked when fetch was skipped).
if [ "${DRY_RUN}" = "false" ] || [ "${SKIP_FETCH}" = "true" ]; then
    require_target_data
    prepare_holdout_split
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
        "--strategy" "${STRATEGY}"
        "--symbol" "${SYMBOL}"
        "--data" "${HPO_DATA}"
        "--dollar-bar" "${DOLLAR_BAR}"
        "--folds" "${FOLDS}"
        "--train-ratio" "${TRAIN_RATIO}"
        "--embargo-bars" "${EMBARGO_BARS}"
        "--candidates" "${CANDIDATES}"
        "--output-config" "${CONFIG_PATH}"
    )
    if [ -n "${PARAM_SPACE}" ]; then
        HPO_ARGS+=("--param-space" "${PARAM_SPACE}")
    fi
    if [ -n "${EXECUTION_CONTEXT}" ]; then
        HPO_ARGS+=("--execution-context" "${EXECUTION_CONTEXT}")
    fi
    if [ -n "${MIN_TRADES_PER_FOLD}" ]; then
        HPO_ARGS+=("--min-trades-per-fold" "${MIN_TRADES_PER_FOLD}")
    fi
    if [ -n "${CAPITAL}" ]; then
        HPO_ARGS+=("--capital" "${CAPITAL}")
    fi
    if [ "${GCP_BIGQUERY}" = "true" ]; then
        HPO_ARGS+=("--gcp-bigquery")
    fi
    if [ "${UPLOAD_WINNER}" = "true" ]; then
        HPO_ARGS+=("--gcs-upload")
    fi
    if [ -n "${ARTIFACT_SUFFIX}" ]; then
        HPO_ARGS+=("--artifact-suffix" "${ARTIFACT_SUFFIX}")
    fi
    if [ "${STRICT_SINKS}" = "true" ]; then
        HPO_ARGS+=("--strict-sinks")
    fi
    run_stage hpo "${BIN_PATH}/run_hpo" "${HPO_ARGS[@]}"
fi

# Step 3: Backtest + Discrete Event Monte Carlo Stress-Test
echo -e "\n[3/3] Running 1:1 Backtest & Monte Carlo Stress-Testing..."
BACKTEST_ARGS=(
    "--data" "${BACKTEST_DATA}"
    "--dollar-bar" "${DOLLAR_BAR}"
    "--config" "${CONFIG_PATH}"
    "--strategy" "${STRATEGY}"
    "--monte-carlo"
    "--mc-iterations" "${MC_ITERATIONS}"
)
if [ -n "${CAPITAL}" ]; then
    BACKTEST_ARGS+=("--capital" "${CAPITAL}")
fi
if [ "${GCP_BIGQUERY}" = "true" ]; then
    BACKTEST_ARGS+=("--gcp-bigquery")
fi
if [ "${STRICT_SINKS}" = "true" ]; then
    BACKTEST_ARGS+=("--strict-sinks")
fi
run_stage backtest "${BIN_PATH}/backtest" "${BACKTEST_ARGS[@]}"

if [ "${DRY_RUN}" = "true" ]; then
    echo ""
    echo "[DRY-RUN] No commands were executed."
    exit 0
fi

END_TOTAL=$(date +%s)
TOTAL_DURATION=$((END_TOTAL - START_TOTAL))

echo ""
echo "============================================================"
echo " [SUCCESS] Entire Production Pipeline Completed!"
echo " Total Elapsed Time:   ${TOTAL_DURATION}s"
echo " Winning Parameters:   ${CONFIG_PATH}"
if [ "${GCP_BIGQUERY}" = "true" ]; then
    echo " Run Analytics:        BigQuery atsnt_bi (v_runs, v_trades, v_equity)"
fi
echo " Next Production Step: Run real-time paper trading (if the backtest above printed a"
echo "   [GATE] line, paper_trading will refuse this artifact):"
echo "   cargo run --release -p backtest --bin paper_trading -- \\"
echo "     --symbol ${SYMBOL,,} --config ${CONFIG_PATH}"
echo "   (Add --baseline-run-id <backtest run id> to record which backtest justifies the session)"
echo "   (Note: Dollar bar threshold is auto-adopted from the deployment artifact)"
echo "============================================================"
