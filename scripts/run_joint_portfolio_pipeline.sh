#!/usr/bin/env bash
# =============================================================================
# ATSNT - Automated Joint Multi-Asset Portfolio Staging Pipeline
#
# Executes a unified, single-run multi-asset portfolio workflow:
#   1. Binance Historical ETL (fetch_data) for BTCUSDT, ETHUSDT, SOLUSDT
#   2. Joint Multi-Asset Walk-Forward HPO (run_portfolio_hpo) over EventMerger stream
#   3. Full-sample 1:1 Discrete Event Simulation Verification (run_portfolio)
#   4. Authoritative Portfolio Deployment Artifact generation and GCS upload
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${PROJECT_ROOT}"

STRATEGY="quantum_well_swing"
YEAR="$(date +%Y)"
MONTH=""
CAPITAL="500"
CANDIDATES="30"
FOLDS="3"
TRAIN_RATIO="0.70"
EMBARGO_BARS="20"
BTC_DOLLAR_BAR=""
ETH_DOLLAR_BAR=""
SOL_DOLLAR_BAR=""
BTC_DOLLAR_BARS=""
ETH_DOLLAR_BARS=""
SOL_DOLLAR_BARS=""
SPOT=true
BNB_DISCOUNT=true
OUTPUT_DIR="data/historical"
CONFIG_PATH="configs/portfolio_deployment_artifact.json"
PARAM_SPACE="configs/param_space_asymmetric_spot.json"
BIN_DIR=""
SKIP_FETCH=false
SKIP_HPO=false
DRY_RUN=false
UPLOAD_WINNER=false
ARTIFACT_SUFFIX=""
STRICT_SINKS=false

show_help() {
    cat << EOF
ATSNT - Automated Joint Multi-Asset Portfolio Pipeline

Usage:
  $(basename "$0") [OPTIONS]

Options:
      --strategy <NAME>        Strategy identifier (default: ${STRATEGY})
  -y, --year <YYYY>            Year to process (default: ${YEAR})
  -m, --month <MM>             Optional single completed month (1-12)
  -k, --capital <AMOUNT>       Initial shared capital in USDT (default: ${CAPITAL})
  -c, --candidates <N>         Portfolio parameter candidates to evaluate (default: ${CANDIDATES})
  -f, --folds <N>              Walk-Forward validation folds (default: ${FOLDS})
  -r, --train-ratio <FLOAT>    In-Sample train ratio (default: ${TRAIN_RATIO})
  -e, --embargo-bars <N>       Embargo bars between folds (default: ${EMBARGO_BARS})
      --btc-dollar-bar <N>     BTC dollar bar aggregation threshold (default: ${BTC_DOLLAR_BAR})
      --eth-dollar-bar <N>     ETH dollar bar aggregation threshold (default: ${ETH_DOLLAR_BAR})
      --sol-dollar-bar <N>     SOL dollar bar aggregation threshold (default: ${SOL_DOLLAR_BAR})
      --spot                   Enforce Spot market mechanics (1x lev, 0% funding, default: ${SPOT})
      --bnb-discount           Apply 25% BNB fee discount (0.075% Spot fees, default: ${BNB_DISCOUNT})
      --param-space <PATH>     JSON parameter space override (default: ${PARAM_SPACE})
  -o, --output-dir <DIR>       Destination directory for Parquet data (default: ${OUTPUT_DIR})
      --config <PATH>          Path for winning Portfolio Deployment Artifact JSON (default: ${CONFIG_PATH})
      --bin-dir <DIR>          Precompiled release binaries directory
      --skip-fetch             Skip data download/conversion
      --skip-hpo               Skip HPO and only run backtest verification with --config
      --upload-winner          Upload winning artifact to GCS
      --artifact-suffix <SFX>  Suffix for uploaded artifact
      --strict-sinks           Abort on GCS upload failure
      --dry-run                Print planned commands without executing
  -h, --help                   Show this help message
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --strategy) STRATEGY="$2"; shift 2 ;;
        -y|--year) YEAR="$2"; shift 2 ;;
        -m|--month) MONTH="$2"; shift 2 ;;
        -k|--capital) CAPITAL="$2"; shift 2 ;;
        -c|--candidates) CANDIDATES="$2"; shift 2 ;;
        -f|--folds) FOLDS="$2"; shift 2 ;;
        -r|--train-ratio) TRAIN_RATIO="$2"; shift 2 ;;
        -e|--embargo-bars) EMBARGO_BARS="$2"; shift 2 ;;
        --btc-dollar-bar) BTC_DOLLAR_BAR="$2"; shift 2 ;;
        --eth-dollar-bar) ETH_DOLLAR_BAR="$2"; shift 2 ;;
        --sol-dollar-bar) SOL_DOLLAR_BAR="$2"; shift 2 ;;
        --btc-dollar-bars) BTC_DOLLAR_BARS="$2"; shift 2 ;;
        --eth-dollar-bars) ETH_DOLLAR_BARS="$2"; shift 2 ;;
        --sol-dollar-bars) SOL_DOLLAR_BARS="$2"; shift 2 ;;
        --spot) SPOT=true; shift ;;
        --no-spot) SPOT=false; shift ;;
        --bnb-discount) BNB_DISCOUNT=true; shift ;;
        --no-bnb-discount) BNB_DISCOUNT=false; shift ;;
        --param-space) PARAM_SPACE="$2"; shift 2 ;;
        -o|--output-dir) OUTPUT_DIR="$2"; shift 2 ;;
        --config|--output-config) CONFIG_PATH="$2"; shift 2 ;;
        --bin-dir) BIN_DIR="$2"; shift 2 ;;
        --skip-fetch) SKIP_FETCH=true; shift ;;
        --skip-hpo) SKIP_HPO=true; shift ;;
        --upload-winner) UPLOAD_WINNER=true; shift ;;
        --artifact-suffix) ARTIFACT_SUFFIX="$2"; shift 2 ;;
        --strict-sinks) STRICT_SINKS=true; shift ;;
        --dry-run) DRY_RUN=true; shift ;;
        --no-inhibit) shift ;; # Cloud Batch compatibility
        -h|--help) show_help; exit 0 ;;
        *)
            echo "[ERROR] Unknown option: $1" >&2
            exit 1
            ;;
    esac
done

if [ -n "${BIN_DIR}" ]; then
    BIN_PATH="${BIN_DIR}"
else
    BIN_PATH="${PROJECT_ROOT}/target/release"
fi

MONTH_PADDED=""
if [ -n "${MONTH}" ]; then
    MONTH_PADDED="$(printf "%02d" "$((10#${MONTH}))")"
fi

SYMBOLS=("BTCUSDT" "ETHUSDT" "SOLUSDT")

echo "============================================================"
echo " ATSNT - Joint Multi-Asset Portfolio Staging Pipeline"
echo "============================================================"
echo "Strategy:        ${STRATEGY}"
echo "Year:            ${YEAR}${MONTH_PADDED:+ (Month ${MONTH_PADDED})}"
echo "Initial Capital: \$${CAPITAL} USDT"
echo "Candidates:      ${CANDIDATES}"
echo "Validation:      ${FOLDS} Walk-Forward Folds (Train: ${TRAIN_RATIO}, Embargo: ${EMBARGO_BARS} bars)"
echo "Dollar Bars:     BTC: \$${BTC_DOLLAR_BAR} | ETH: \$${ETH_DOLLAR_BAR} | SOL: \$${SOL_DOLLAR_BAR}"
echo "Spot Mode:       ${SPOT} (BNB Fee Discount: ${BNB_DISCOUNT})"
echo "Param Space:     ${PARAM_SPACE}"
echo "Data Directory:  ${OUTPUT_DIR}"
echo "Winning Config:  ${CONFIG_PATH}"
echo "Upload to GCS:   ${UPLOAD_WINNER}"
echo "------------------------------------------------------------"

run_cmd() {
    if [ "${DRY_RUN}" = "true" ]; then
        echo "[DRY-RUN] $*"
    else
        "$@"
    fi
}

# Step 0: Ensure binaries exist
if [ -z "${BIN_DIR}" ] && [ "${DRY_RUN}" = "false" ]; then
    echo "[0/3] Compiling release binaries (fetch_data, run_portfolio_hpo, run_portfolio)..."
    cargo build --release -p adapters --bin fetch_data -p backtest --bin run_portfolio_hpo -p backtest --bin run_portfolio
    echo "      [OK] Binaries compiled successfully."
fi

# Step 1: Ingest Data for all 3 Assets
if [ "${SKIP_FETCH}" = "true" ]; then
    echo -e "\n[1/3] [SKIPPED] Historical ETL (Using existing Parquet data in ${OUTPUT_DIR})"
else
    echo -e "\n[1/3] Running Historical ETL for BTC, ETH, and SOL in parallel..."
    FETCH_PIDS=()
    for sym in "${SYMBOLS[@]}"; do
        FETCH_ARGS=(
            "--symbol" "${sym}"
            "--year" "${YEAR}"
            "--output-dir" "${OUTPUT_DIR}"
        )
        [ -n "${MONTH}" ] && FETCH_ARGS+=("--month" "${MONTH}")

        if [ "${DRY_RUN}" = "true" ]; then
            echo "[DRY-RUN] ${BIN_PATH}/fetch_data ${FETCH_ARGS[*]}"
        else
            echo "      [*] Spawning fetch_data for ${sym}..."
            "${BIN_PATH}/fetch_data" "${FETCH_ARGS[@]}" &
            FETCH_PIDS+=($!)
        fi
    done

    if [ "${DRY_RUN}" = "false" ]; then
        for pid in "${FETCH_PIDS[@]}"; do
            wait "${pid}"
        done
        echo "      [OK] All 3 assets fetched and converted to Parquet."
    fi
fi

# Step 2: Joint Multi-Asset Walk-Forward HPO
BTC_DATA="${OUTPUT_DIR}/BTCUSDT"
ETH_DATA="${OUTPUT_DIR}/ETHUSDT"
SOL_DATA="${OUTPUT_DIR}/SOLUSDT"

if [ -n "${MONTH_PADDED}" ]; then
    BTC_MONTHLY="${OUTPUT_DIR}/BTCUSDT/BTCUSDT-aggTrades-${YEAR}-${MONTH_PADDED}.parquet"
    if [ -f "${BTC_MONTHLY}" ]; then
        BTC_DATA="${BTC_MONTHLY}"
        ETH_DATA="${OUTPUT_DIR}/ETHUSDT/ETHUSDT-aggTrades-${YEAR}-${MONTH_PADDED}.parquet"
        SOL_DATA="${OUTPUT_DIR}/SOLUSDT/SOLUSDT-aggTrades-${YEAR}-${MONTH_PADDED}.parquet"
    fi
fi

if [ "${SKIP_HPO}" = "true" ]; then
    echo -e "\n[2/3] [SKIPPED] Joint Portfolio HPO (Using existing artifact: ${CONFIG_PATH})"
else
    echo -e "\n[2/3] Running Joint Multi-Asset Walk-Forward Portfolio HPO..."
    HPO_ARGS=(
        "--strategy" "${STRATEGY}"
        "--btc-data" "${BTC_DATA}"
        "--eth-data" "${ETH_DATA}"
        "--sol-data" "${SOL_DATA}"
        "--capital" "${CAPITAL}"
        "--candidates" "${CANDIDATES}"
        "--folds" "${FOLDS}"
        "--train-ratio" "${TRAIN_RATIO}"
        "--embargo-bars" "${EMBARGO_BARS}"
        "--output-config" "${CONFIG_PATH}"
    )
    [ -n "${BTC_DOLLAR_BAR}" ] && HPO_ARGS+=("--btc-dollar-bar" "${BTC_DOLLAR_BAR}")
    [ -n "${ETH_DOLLAR_BAR}" ] && HPO_ARGS+=("--eth-dollar-bar" "${ETH_DOLLAR_BAR}")
    [ -n "${SOL_DOLLAR_BAR}" ] && HPO_ARGS+=("--sol-dollar-bar" "${SOL_DOLLAR_BAR}")
    [ -n "${BTC_DOLLAR_BARS}" ] && HPO_ARGS+=("--btc-dollar-bars" "${BTC_DOLLAR_BARS}")
    [ -n "${ETH_DOLLAR_BARS}" ] && HPO_ARGS+=("--eth-dollar-bars" "${ETH_DOLLAR_BARS}")
    [ -n "${SOL_DOLLAR_BARS}" ] && HPO_ARGS+=("--sol-dollar-bars" "${SOL_DOLLAR_BARS}")
    [ -f "${PARAM_SPACE}" ] && HPO_ARGS+=("--param-space" "${PARAM_SPACE}")
    [ "${SPOT}" = "true" ] && HPO_ARGS+=("--spot")
    [ "${BNB_DISCOUNT}" = "true" ] && HPO_ARGS+=("--bnb-discount")
    [ "${UPLOAD_WINNER}" = "true" ] && HPO_ARGS+=("--gcs-upload")
    [ -n "${ARTIFACT_SUFFIX}" ] && HPO_ARGS+=("--artifact-suffix" "${ARTIFACT_SUFFIX}")
    [ "${STRICT_SINKS}" = "true" ] && HPO_ARGS+=("--strict-sinks")

    run_cmd "${BIN_PATH}/run_portfolio_hpo" "${HPO_ARGS[@]}"
fi

# Step 3: Full Discrete Event Simulation Verification
echo -e "\n[3/3] Running Full Discrete Event Simulation Verification with Winning Artifact..."
BACKTEST_ARGS=(
    "--strategy" "${STRATEGY}"
    "--portfolio-config" "${CONFIG_PATH}"
    "--btc-data" "${BTC_DATA}"
    "--eth-data" "${ETH_DATA}"
    "--sol-data" "${SOL_DATA}"
)
[ "${SPOT}" = "true" ] && BACKTEST_ARGS+=("--spot")
[ "${BNB_DISCOUNT}" = "true" ] && BACKTEST_ARGS+=("--bnb-discount")

run_cmd "${BIN_PATH}/run_portfolio" "${BACKTEST_ARGS[@]}"

echo ""
echo "============================================================"
echo " [SUCCESS] Joint Multi-Asset Portfolio Pipeline Completed!"
echo " Winning Artifact: ${CONFIG_PATH}"
echo "============================================================"
