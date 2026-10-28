#!/usr/bin/env bash
# =============================================================================
# ATSNT - Multi-Asset Cloud Batch HPO Matrix Runner
#
# Launches parallel ephemeral Google Cloud Batch jobs exploring:
#   - Asset granularity (Dollar Bar threshold matrix)
#   - Broad parameter spaces (Stop-Loss ATR buffers, well depths, take-profits)
#
# Each job runs on a dedicated high-vCPU Spot VM in europe-southwest1, evaluates
# all candidates in parallel using Rayon, streams metrics to BigQuery, and uploads
# the winning artifact to Cloud Storage.
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
cd "${PROJECT_ROOT}"

PROJECT_ID="${GCP_PROJECT_ID:-mi-facturador-bot-01}"
REGION="${GCP_REGION:-europe-southwest1}"
BUCKET_NAME="${GCS_BUCKET_NAME:-atsnt-lake-${PROJECT_ID}}"
YEAR="${YEAR:-2026}"
CANDIDATES="${CANDIDATES:-100}"
IMAGE_TAG="${IMAGE_TAG:-latest}"
IMAGE_URI="${REGION}-docker.pkg.dev/${PROJECT_ID}/atsnt/atsnt-batch:${IMAGE_TAG}"

echo "============================================================"
echo "    ATSNT - Multi-Asset Cloud Batch HPO Matrix Runner       "
echo "============================================================"
echo "Project:    ${PROJECT_ID}"
echo "Region:     ${REGION}"
echo "Image:      ${IMAGE_URI}"
echo "Year:       ${YEAR}"
echo "Candidates: ${CANDIDATES} per job"
echo "------------------------------------------------------------"

# Define the evaluation matrix: Symbol | DollarBar | ParamSpace
# Format: "SYMBOL DOLLAR_BAR PARAM_SPACE"
MATRIX=(
    "BTCUSDT 15000000 configs/param_space_langevin_btc.json"
    "BTCUSDT 25000000 configs/param_space_langevin_btc.json"
    "ETHUSDT 3000000 configs/param_space_langevin_eth.json"
    "ETHUSDT 6000000 configs/param_space_langevin_eth.json"
    "ETHUSDT 10000000 configs/param_space_langevin_eth.json"
    "SOLUSDT 2000000 configs/param_space_langevin_sol.json"
    "SOLUSDT 4000000 configs/param_space_langevin_sol.json"
    "SOLUSDT 6000000 configs/param_space_langevin_sol.json"
)

TIMESTAMP="$(date -u +%Y%m%d-%H%M%S)"
SUBMITTED_JOBS=()
JOB_ARTIFACTS=()

echo "[*] Submitting ${#MATRIX[@]} Batch jobs concurrently..."

for entry in "${MATRIX[@]}"; do
    read -r SYM BAR SPACE <<< "${entry}"
    JOB_NAME="hpo-${SYM,,}-${BAR}-${TIMESTAMP}"
    CONFIG_STEM="hpo_${SYM,,}_${YEAR}"
    ARTIFACT_OBJECT="artifacts/${CONFIG_STEM}-${JOB_NAME}.json"
    LOCAL_DEST="configs/matrix/${CONFIG_STEM}-${JOB_NAME}.json"
    
    mkdir -p configs/matrix
    
    echo " -> Submitting job: ${JOB_NAME} (${SYM} @ \$${BAR} Dollar Bars)..."
    
    "${SCRIPT_DIR}/run_ephemeral_batch_hpo.sh" \
        --project "${PROJECT_ID}" \
        --region "${REGION}" \
        --symbol "${SYM}" \
        --year "${YEAR}" \
        --job-name "${JOB_NAME}" \
        --image-tag "${IMAGE_TAG}" \
        --machine-type "n2-highcpu-16" \
        --skip-build \
        --no-wait \
        -- \
        --strategy "langevin" \
        --dollar-bar "${BAR}" \
        --param-space "/app/${SPACE}" \
        --execution-context "/app/configs/execution_swing.json" \
        --candidates "${CANDIDATES}" \
        --min-trades-per-fold 1 \
        --folds 3
        
    SUBMITTED_JOBS+=("${JOB_NAME}")
    JOB_ARTIFACTS+=("${ARTIFACT_OBJECT}:${LOCAL_DEST}")
done

echo ""
echo "============================================================"
echo " [OK] All ${#SUBMITTED_JOBS[@]} Batch jobs successfully submitted!"
echo "============================================================"
for job in "${SUBMITTED_JOBS[@]}"; do
    echo "  - ${job}"
done
echo "------------------------------------------------------------"
echo "Monitoring job states in Cloud Batch..."

ALL_DONE=false
POLL_INTERVAL=30

while [ "${ALL_DONE}" = "false" ]; do
    sleep "${POLL_INTERVAL}"
    ALL_DONE=true
    RUNNING_COUNT=0
    SUCCEEDED_COUNT=0
    FAILED_COUNT=0
    
    echo -n "[$(date -u +%H:%M:%S)] Status: "
    for job in "${SUBMITTED_JOBS[@]}"; do
        STATE="$(gcloud batch jobs describe "${job}" --project="${PROJECT_ID}" --location="${REGION}" --format="value(status.state)" 2>/dev/null || echo "UNKNOWN")"
        case "${STATE}" in
            SUCCEEDED)
                SUCCEEDED_COUNT=$((SUCCEEDED_COUNT + 1))
                ;;
            FAILED|CANCELLED)
                FAILED_COUNT=$((FAILED_COUNT + 1))
                ;;
            *)
                RUNNING_COUNT=$((RUNNING_COUNT + 1))
                ALL_DONE=false
                ;;
        esac
    done
    echo "${RUNNING_COUNT} running/queued, ${SUCCEEDED_COUNT} succeeded, ${FAILED_COUNT} failed (${#SUBMITTED_JOBS[@]} total)"
done

echo "============================================================"
echo " Batch Execution Finished: ${SUCCEEDED_COUNT} Succeeded, ${FAILED_COUNT} Failed"
echo "============================================================"

echo "[*] Downloading winning deployment artifacts from Cloud Storage..."
for item in "${JOB_ARTIFACTS[@]}"; do
    GCS_PATH="${item%%:*}"
    LOCAL_PATH="${item#*:}"
    if gcloud storage cp "gs://${BUCKET_NAME}/${GCS_PATH}" "${LOCAL_PATH}" 2>/dev/null; then
        echo "  [OK] Downloaded: ${LOCAL_PATH}"
    else
        echo "  [WARN] Artifact not found (job may have failed or produced no winner): gs://${BUCKET_NAME}/${GCS_PATH}"
    fi
done

echo "============================================================"
echo " [SUCCESS] Cloud Batch HPO Matrix completed!"
echo "============================================================"
