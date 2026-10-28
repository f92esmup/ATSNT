#!/usr/bin/env bash
# =============================================================================
# ATSNT - Runner for the 3 remaining HPO Matrix jobs with 8 vCPUs (zero quota contention)
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
cd "${PROJECT_ROOT}"

PROJECT_ID="mi-facturador-bot-01"
REGION="europe-southwest1"
BUCKET_NAME="atsnt-lake-${PROJECT_ID}"
YEAR="2026"
CANDIDATES="100"
IMAGE_TAG="latest"

echo "============================================================"
echo " Submitting Remaining 3 Jobs with n2-highcpu-8 (Concurrently)"
echo "============================================================"

REMAINING=(
    "BTCUSDT 25000000 configs/param_space_langevin_btc.json"
    "ETHUSDT 3000000 configs/param_space_langevin_eth.json"
    "SOLUSDT 2000000 configs/param_space_langevin_sol.json"
)

TIMESTAMP="$(date -u +%Y%m%d-%H%M%S)"
SUBMITTED_JOBS=()
JOB_ARTIFACTS=()

mkdir -p configs/matrix

for entry in "${REMAINING[@]}"; do
    read -r SYM BAR SPACE <<< "${entry}"
    JOB_NAME="hpo-${SYM,,}-${BAR}-${TIMESTAMP}"
    CONFIG_STEM="hpo_${SYM,,}_${YEAR}"
    ARTIFACT_OBJECT="artifacts/${CONFIG_STEM}-${JOB_NAME}.json"
    LOCAL_DEST="configs/matrix/${CONFIG_STEM}-${JOB_NAME}.json"
    
    echo " -> Submitting job: ${JOB_NAME} (${SYM} @ \$${BAR} Dollar Bars on 8 vCPUs)..."
    
    "${SCRIPT_DIR}/run_ephemeral_batch_hpo.sh" \
        --project "${PROJECT_ID}" \
        --region "${REGION}" \
        --symbol "${SYM}" \
        --year "${YEAR}" \
        --job-name "${JOB_NAME}" \
        --image-tag "${IMAGE_TAG}" \
        --machine-type "n2-highcpu-8" \
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
echo " [OK] All 3 jobs submitted to Cloud Batch!"
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
    echo "${RUNNING_COUNT} running/scheduled, ${SUCCEEDED_COUNT} succeeded, ${FAILED_COUNT} failed (${#SUBMITTED_JOBS[@]} total)"
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
        echo "  [WARN] Artifact not found: gs://${BUCKET_NAME}/${GCS_PATH}"
    fi
done

echo "============================================================"
echo " [SUCCESS] All 3 remaining jobs completed!"
echo "============================================================"
