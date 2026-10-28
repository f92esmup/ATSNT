#!/usr/bin/env bash
# =============================================================================
# ATSNT - Ephemeral Cloud Batch Single-Job Joint Portfolio HPO Orchestrator
#
# Runs the unified multi-asset Spot trading strategy optimization on a SINGLE
# disposable Cloud Batch VM (Spot n2-highcpu-32 by default):
#   1. Validates preflight quotas and GCS bucket permissions.
#   2. Builds and pushes the batch image with multi-asset capabilities.
#   3. Submits EXACTLY 1 Cloud Batch job running the joint portfolio pipeline
#      (fetching BTC, ETH, SOL -> joint Walk-Forward HPO -> 1:1 portfolio simulation).
#   4. Waits for the single job, streams logs from Cloud Logging.
#   5. Downloads the authoritative winning Portfolio Deployment Artifact locally.
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
cd "${PROJECT_ROOT}"

PROJECT_ID="${GCP_PROJECT_ID:-mi-facturador-bot-01}"
REGION="${GCP_REGION:-europe-southwest1}"
BUCKET_NAME="${GCS_BUCKET_NAME:-atsnt-lake-${PROJECT_ID}}"
DATASET_ID="${GCP_DATASET_ID:-atsnt_bi}"
PREEMPTION_EXIT_CODE="50001"
REPOSITORY="atsnt"
IMAGE_NAME="atsnt-batch"
IMAGE_TAG=""
SERVICE_ACCOUNT=""
MACHINE_TYPE="n2-custom-30-30720"
PROVISIONING="SPOT"
MAX_RETRIES="2"
BOOT_DISK_GB="100"
MAX_RUN_SECONDS="14400"
POLL_SECONDS="30"
YEAR="$(date +%Y)"
MONTH=""
STRATEGY="quantum_well_swing"
CAPITAL="500"
CANDIDATES="30"
FOLDS="3"
BTC_DOLLAR_BAR=""
ETH_DOLLAR_BAR=""
SOL_DOLLAR_BAR=""
BTC_DOLLAR_BARS=""
ETH_DOLLAR_BARS=""
SOL_DOLLAR_BARS=""
PARAM_SPACE="configs/param_space_asymmetric_spot.json"
JOB_NAME=""
DOWNLOAD_DIR="configs"
SKIP_BUILD=false
NO_WAIT=false
DRY_RUN=false
JOB_SPEC_OUT=""
EXTRA_ARGS=()

show_help() {
    cat << EOF
ATSNT - Ephemeral Single-Job Joint Portfolio HPO Orchestrator

Usage:
  $(basename "$0") [OPTIONS] [-- <extra run_joint_portfolio_pipeline.sh options>]

Options:
      --project <ID>         GCP project ID (default: ${PROJECT_ID})
      --region <REGION>      Cloud Batch region (default: ${REGION})
      --bucket <NAME>        GCS bucket name (default: ${BUCKET_NAME})
  -y, --year <YYYY>          Year to process (default: ${YEAR})
  -m, --month <MM>           Optional single completed month (1-12)
  -k, --capital <AMOUNT>     Initial shared capital in USDT (default: ${CAPITAL})
  -c, --candidates <N>       Portfolio parameter candidates (default: ${CANDIDATES})
  -f, --folds <N>            Walk-Forward validation folds (default: ${FOLDS})
      --btc-dollar-bar <N>   BTC dollar bar aggregation threshold (default: ${BTC_DOLLAR_BAR})
      --eth-dollar-bar <N>   ETH dollar bar aggregation threshold (default: ${ETH_DOLLAR_BAR})
      --sol-dollar-bar <N>   SOL dollar bar aggregation threshold (default: ${SOL_DOLLAR_BAR})
      --job-name <NAME>      Batch job name (default: hpo-portfolio-<year>[-<MM>]-<timestamp>)
      --machine-type <TYPE>  Compute Engine VM type (default: ${MACHINE_TYPE})
      --standard             Use standard VM provisioning instead of Spot
      --max-retries <N>      Preemption retry count (default: ${MAX_RETRIES})
      --boot-disk-gb <N>     VM boot disk size in GB (default: ${BOOT_DISK_GB})
      --max-run-seconds <N>  Maximum task execution duration in seconds (default: ${MAX_RUN_SECONDS})
      --service-account <E>  Batch VM service account email
      --image-tag <TAG>      Docker image tag
      --skip-build           Skip Docker build/push and reuse existing image
      --download-dir <DIR>   Destination directory for winning artifact (default: ${DOWNLOAD_DIR})
      --no-wait              Submit single job and exit immediately without waiting
      --dry-run              Render exact job spec and plan without making GCP/Docker calls
      --job-spec-out <PATH>  Write rendered JSON job spec to file
  -h, --help                 Show this help message and exit
EOF
}

die() {
    echo "[ERROR] $*" >&2
    exit 1
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --project) PROJECT_ID="$2"; shift 2 ;;
        --region) REGION="$2"; shift 2 ;;
        --bucket) BUCKET_NAME="$2"; shift 2 ;;
        -y|--year) YEAR="$2"; shift 2 ;;
        -m|--month) MONTH="$2"; shift 2 ;;
        -k|--capital) CAPITAL="$2"; shift 2 ;;
        -c|--candidates) CANDIDATES="$2"; shift 2 ;;
        -f|--folds) FOLDS="$2"; shift 2 ;;
        --btc-dollar-bar) BTC_DOLLAR_BAR="$2"; shift 2 ;;
        --eth-dollar-bar) ETH_DOLLAR_BAR="$2"; shift 2 ;;
        --sol-dollar-bar) SOL_DOLLAR_BAR="$2"; shift 2 ;;
        --btc-dollar-bars) BTC_DOLLAR_BARS="$2"; shift 2 ;;
        --eth-dollar-bars) ETH_DOLLAR_BARS="$2"; shift 2 ;;
        --sol-dollar-bars) SOL_DOLLAR_BARS="$2"; shift 2 ;;
        --job-name) JOB_NAME="$2"; shift 2 ;;
        --machine-type) MACHINE_TYPE="$2"; shift 2 ;;
        --standard) PROVISIONING="STANDARD"; shift ;;
        --max-retries) MAX_RETRIES="$2"; shift 2 ;;
        --boot-disk-gb) BOOT_DISK_GB="$2"; shift 2 ;;
        --max-run-seconds) MAX_RUN_SECONDS="$2"; shift 2 ;;
        --service-account) SERVICE_ACCOUNT="$2"; shift 2 ;;
        --image-tag) IMAGE_TAG="$2"; shift 2 ;;
        --skip-build) SKIP_BUILD=true; shift ;;
        --download-dir) DOWNLOAD_DIR="$2"; shift 2 ;;
        --no-wait) NO_WAIT=true; shift ;;
        --dry-run) DRY_RUN=true; shift ;;
        --job-spec-out) JOB_SPEC_OUT="$2"; shift 2 ;;
        -h|--help) show_help; exit 0 ;;
        --) shift; EXTRA_ARGS=("$@"); break ;;
        *)
            echo "[ERROR] Unknown option: $1" >&2
            exit 1
            ;;
    esac
done

[ -n "${PROJECT_ID}" ] || die "--project (or GCP_PROJECT_ID) is required."
[[ "${YEAR}" =~ ^[0-9]{4}$ ]] || die "--year must be YYYY (got '${YEAR}')."

MONTH_PADDED=""
if [ -n "${MONTH}" ]; then
    if ! [[ "${MONTH}" =~ ^[0-9]{1,2}$ ]] || [ $((10#${MONTH})) -lt 1 ] || [ $((10#${MONTH})) -gt 12 ]; then
        die "--month must be between 1 and 12 (got '${MONTH}')."
    fi
    MONTH_PADDED="$(printf "%02d" "$((10#${MONTH}))")"
fi

PERIOD="${YEAR}${MONTH_PADDED:+_${MONTH_PADDED}}"
if [ -z "${JOB_NAME}" ]; then
    JOB_NAME="hpo-portfolio-${YEAR}${MONTH_PADDED:+-${MONTH_PADDED}}-$(date -u +%Y%m%d-%H%M%S)"
fi

[[ "${JOB_NAME}" =~ ^[a-z]([a-z0-9-]{0,61}[a-z0-9])?$ ]] \
    || die "Invalid job name '${JOB_NAME}': use lowercase letters, digits and hyphens (max 63)."

SERVICE_ACCOUNT="${SERVICE_ACCOUNT:-atsnt-batch@${PROJECT_ID}.iam.gserviceaccount.com}"

if [ -z "${IMAGE_TAG}" ]; then
    IMAGE_TAG="$(git rev-parse --short=12 HEAD 2>/dev/null || echo latest)"
    if [ -n "$(git status --porcelain 2>/dev/null)" ]; then
        IMAGE_TAG="${IMAGE_TAG}-dirty"
    fi
fi
IMAGE_URI="${REGION}-docker.pkg.dev/${PROJECT_ID}/${REPOSITORY}/${IMAGE_NAME}:${IMAGE_TAG}"

CONFIG_STEM="portfolio_deployment_artifact"
ARTIFACT_OBJECT="artifacts/${CONFIG_STEM}-${JOB_NAME}.json"
ARTIFACT_URI="gs://${BUCKET_NAME}/${ARTIFACT_OBJECT}"
LOCAL_ARTIFACT="${DOWNLOAD_DIR}/portfolio_deployment_artifact.json"

PIPELINE_ARGS=(
    "--bin-dir" "/app/bin"
    "--output-dir" "/app/work/data"
    "--config" "/app/work/portfolio_deployment_artifact.json"
    "--strategy" "${STRATEGY}"
    "--param-space" "/app/${PARAM_SPACE}"
    "--year" "${YEAR}"
    "--capital" "${CAPITAL}"
    "--candidates" "${CANDIDATES}"
    "--folds" "${FOLDS}"
    "--spot"
    "--bnb-discount"
    "--upload-winner"
    "--artifact-suffix" "${JOB_NAME}"
    "--strict-sinks"
)
[ -n "${BTC_DOLLAR_BAR}" ] && PIPELINE_ARGS+=(--btc-dollar-bar "${BTC_DOLLAR_BAR}")
[ -n "${ETH_DOLLAR_BAR}" ] && PIPELINE_ARGS+=(--eth-dollar-bar "${ETH_DOLLAR_BAR}")
[ -n "${SOL_DOLLAR_BAR}" ] && PIPELINE_ARGS+=(--sol-dollar-bar "${SOL_DOLLAR_BAR}")
[ -n "${BTC_DOLLAR_BARS}" ] && PIPELINE_ARGS+=(--btc-dollar-bars "${BTC_DOLLAR_BARS}")
[ -n "${ETH_DOLLAR_BARS}" ] && PIPELINE_ARGS+=(--eth-dollar-bars "${ETH_DOLLAR_BARS}")
[ -n "${SOL_DOLLAR_BARS}" ] && PIPELINE_ARGS+=(--sol-dollar-bars "${SOL_DOLLAR_BARS}")
[ -n "${MONTH}" ] && PIPELINE_ARGS+=(--month "${MONTH}")
if [ "${#EXTRA_ARGS[@]}" -gt 0 ]; then
    PIPELINE_ARGS+=("${EXTRA_ARGS[@]}")
fi

json_string() {
    local s="$1"
    s="${s//\\/\\\\}"
    s="${s//\"/\\\"}"
    printf '"%s"' "${s}"
}

json_array() {
    local first=true item
    printf '['
    for item in "$@"; do
        [ "${first}" = "true" ] || printf ', '
        json_string "${item}"
        first=false
    done
    printf ']'
}

render_job_spec() {
    cat << EOF
{
  "taskGroups": [
    {
      "taskCount": 1,
      "taskSpec": {
        "runnables": [
          {
            "container": {
              "imageUri": $(json_string "${IMAGE_URI}"),
              "entrypoint": "/app/scripts/run_joint_portfolio_pipeline.sh",
              "commands": $(json_array "${PIPELINE_ARGS[@]}")
            },
            "environment": {
              "variables": {
                "GCP_PROJECT_ID": $(json_string "${PROJECT_ID}"),
                "GCP_DATASET_ID": $(json_string "${DATASET_ID}"),
                "GCS_BUCKET_NAME": $(json_string "${BUCKET_NAME}")
              }
            }
          }
        ],
        "maxRetryCount": ${MAX_RETRIES},
        "lifecyclePolicies": [
          { "action": "RETRY_TASK", "actionCondition": { "exitCodes": [${PREEMPTION_EXIT_CODE}] } }
        ],
        "maxRunDuration": "${MAX_RUN_SECONDS}s"
      }
    }
  ],
  "allocationPolicy": {
    "instances": [
      {
        "policy": {
          "machineType": $(json_string "${MACHINE_TYPE}"),
          "provisioningModel": "${PROVISIONING}",
          "bootDisk": { "sizeGb": ${BOOT_DISK_GB} }
        }
      }
    ],
    "location": { "allowedLocations": [$(json_string "regions/${REGION}")] },
    "serviceAccount": { "email": $(json_string "${SERVICE_ACCOUNT}") }
  },
  "labels": { "app": "atsnt", "component": "joint-portfolio-hpo" },
  "logsPolicy": { "destination": "CLOUD_LOGGING" }
}
EOF
}

SPEC_FILE="$(mktemp "${TMPDIR:-/tmp}/atsnt-joint-job.XXXXXX.json")"
trap 'rm -f "${SPEC_FILE}"' EXIT
render_job_spec > "${SPEC_FILE}"
if [ -n "${JOB_SPEC_OUT}" ]; then
    cp "${SPEC_FILE}" "${JOB_SPEC_OUT}"
fi

echo "============================================================"
echo " ATSNT - Ephemeral Cloud Batch Single Joint Portfolio HPO"
echo "============================================================"
echo "Project:        ${PROJECT_ID}"
echo "Region:         ${REGION}"
echo "Batch Job:      ${JOB_NAME} (EXACTLY 1 JOB)"
echo "Machine:        ${MACHINE_TYPE} (${PROVISIONING}, ${BOOT_DISK_GB} GB SSD, ${MAX_RETRIES} retries on preemption)"
echo "Image:          ${IMAGE_URI}"
echo "Strategy:       ${STRATEGY} (BTC/ETH/SOL Joint Portfolio)"
echo "Capital:        \$${CAPITAL} USDT (Spot 1x leverage, 0% funding, 0.075% BNB fees)"
echo "Candidates:     ${CANDIDATES} across ${FOLDS} Walk-Forward folds"
echo "Winner (GCS):   ${ARTIFACT_URI}"
echo "Winner (local): ${LOCAL_ARTIFACT}"
echo "Market data:    VM local disk only (destroyed with VM)"
echo "------------------------------------------------------------"

if [ "${DRY_RUN}" = "true" ]; then
    echo "[DRY-RUN] Exact 1 Job Specification:"
    cat "${SPEC_FILE}"
    echo ""
    echo "[DRY-RUN] gcloud storage objects describe ${ARTIFACT_URI} (must not exist)"
    if [ "${SKIP_BUILD}" = "false" ]; then
        echo "[DRY-RUN] docker build --build-arg GIT_SHA=$(git rev-parse --short=12 HEAD 2>/dev/null || echo latest) -t ${IMAGE_URI} ."
        echo "[DRY-RUN] docker push ${IMAGE_URI}"
    fi
    echo "[DRY-RUN] gcloud batch jobs submit ${JOB_NAME} --project=${PROJECT_ID} --location=${REGION} --config=${SPEC_FILE}"
    echo "[DRY-RUN] gcloud storage cp ${ARTIFACT_URI} ${LOCAL_ARTIFACT}"
    echo "[DRY-RUN] Verification complete. Exactly 1 job planned. No commands executed."
    exit 0
fi

command -v gcloud >/dev/null 2>&1 || die "gcloud CLI not found."

# Preflight: Check artifact does not already exist
echo "[1/4] Checking that ${ARTIFACT_URI} does not exist..."
if DESCRIBE_OUT="$(gcloud storage objects describe "${ARTIFACT_URI}" --project="${PROJECT_ID}" 2>&1)"; then
    die "Artifact ${ARTIFACT_URI} already exists. Specify a new --job-name."
fi
echo "      [OK] Artifact does not exist yet."

# Preflight: Quota check for machine type
echo "[1/4b] Checking CPU quota for ${MACHINE_TYPE} in ${REGION}..."
ZONES=""
GUEST_CPUS=""
for zone in $(gcloud compute zones list --project="${PROJECT_ID}" \
    --filter="region~/${REGION}\$" --format="value(name)" 2>/dev/null || true); do
    if cpus="$(gcloud compute machine-types describe "${MACHINE_TYPE}" --zone="${zone}" \
        --project="${PROJECT_ID}" --format="value(guestCpus)" 2>/dev/null)" && [ -n "${cpus}" ]; then
        ZONES="${ZONES}${zone} "
        GUEST_CPUS="${cpus}"
    fi
done
[ -n "${ZONES}" ] || die "Machine type ${MACHINE_TYPE} is not offered in ${REGION}."
echo "       [OK] Available in: ${ZONES}(${GUEST_CPUS} vCPU)"

limit_cpus() {
    awk -F, -v metric="$2" '$1 == metric { print int($2) }' <<< "$1"
}
free_cpus() {
    awk -F, -v metric="$2" '$1 == metric { print int($2 - $3) }' <<< "$1"
}

GLOBAL_QUOTAS="$(gcloud compute project-info describe --project="${PROJECT_ID}" --flatten=quotas \
    --format="csv[no-heading](quotas.metric,quotas.limit,quotas.usage)" 2>/dev/null || true)"
GLOBAL_LIMIT="$(limit_cpus "${GLOBAL_QUOTAS}" "CPUS_ALL_REGIONS")"
GLOBAL_FREE="$(free_cpus "${GLOBAL_QUOTAS}" "CPUS_ALL_REGIONS")"

if [ -n "${GLOBAL_LIMIT}" ] && [ "${GLOBAL_LIMIT}" -lt "${GUEST_CPUS}" ]; then
    die "Quota CPUS_ALL_REGIONS has total limit ${GLOBAL_LIMIT} vCPU but ${MACHINE_TYPE} needs ${GUEST_CPUS}."
fi
if [ -n "${GLOBAL_FREE}" ] && [ "${GLOBAL_FREE}" -lt "${GUEST_CPUS}" ]; then
    die "Quota CPUS_ALL_REGIONS currently only has ${GLOBAL_FREE} free vCPU (due to running VMs), but ${MACHINE_TYPE} needs ${GUEST_CPUS}. Use --machine-type n2-custom-${GLOBAL_FREE}-30720 or stop running VMs."
fi
echo "       [OK] Global CPU quota allows ${GUEST_CPUS} vCPU (Free: ${GLOBAL_FREE}/${GLOBAL_LIMIT})."

# Preflight: Build and push image
if [ "${SKIP_BUILD}" = "false" ]; then
    command -v docker >/dev/null 2>&1 || die "docker not found (or use --skip-build)."
    echo "[2/4] Building and pushing image ${IMAGE_URI}..."
    gcloud auth configure-docker "${REGION}-docker.pkg.dev" --quiet >/dev/null
    docker build --build-arg GIT_SHA="$(git rev-parse --short=12 HEAD 2>/dev/null || true)" -t "${IMAGE_URI}" .
    docker push "${IMAGE_URI}"
    echo "      [OK] Docker image pushed successfully."
else
    echo "[2/4] [SKIPPED] Image build (using ${IMAGE_URI})"
fi

# Submit single Cloud Batch Job
echo "[3/4] Submitting EXACTLY 1 Cloud Batch Job ${JOB_NAME}..."
gcloud batch jobs submit "${JOB_NAME}" \
    --project="${PROJECT_ID}" \
    --location="${REGION}" \
    --config="${SPEC_FILE}"

if [ "${NO_WAIT}" = "true" ]; then
    echo "[OK] Single job submitted. Follow it with:"
    echo "  gcloud batch jobs describe ${JOB_NAME} --project=${PROJECT_ID} --location=${REGION}"
    exit 0
fi

# Wait for Job Completion
echo "[4/4] Waiting for job ${JOB_NAME} (polling every ${POLL_SECONDS}s)..."
STATE=""
DESCRIBE_FAILURES=0
while true; do
    if STATE="$(gcloud batch jobs describe "${JOB_NAME}" --project="${PROJECT_ID}" \
        --location="${REGION}" --format="value(status.state)" 2>/dev/null)"; then
        DESCRIBE_FAILURES=0
    else
        DESCRIBE_FAILURES=$((DESCRIBE_FAILURES + 1))
        STATE="UNKNOWN"
        if [ "${DESCRIBE_FAILURES}" -ge 10 ]; then
            die "Could not describe job ${JOB_NAME} 10 times in a row."
        fi
    fi
    echo "      $(date -u +%H:%M:%S) state=${STATE}"
    case "${STATE}" in
        SUCCEEDED|FAILED|CANCELLED|DELETION_IN_PROGRESS) break ;;
    esac
    sleep "${POLL_SECONDS}"
done

JOB_UID="$(gcloud batch jobs describe "${JOB_NAME}" --project="${PROJECT_ID}" \
    --location="${REGION}" --format="value(uid)" 2>/dev/null || true)"
echo "------------------------------------------------------------"
echo " Task logs (Cloud Logging, job uid ${JOB_UID:-unknown})"
echo "------------------------------------------------------------"
if [ -n "${JOB_UID}" ]; then
    gcloud logging read \
        "logName=\"projects/${PROJECT_ID}/logs/batch_task_logs\" AND labels.job_uid=\"${JOB_UID}\"" \
        --project="${PROJECT_ID}" --order=asc --limit=100000 --format="value(textPayload)" || true
fi

if [ "${STATE}" != "SUCCEEDED" ]; then
    die "Batch job ${JOB_NAME} finished with state ${STATE}."
fi

# Download winning artifact
echo "[5/5] Downloading winning portfolio deployment artifact..."
mkdir -p "${DOWNLOAD_DIR}"
gcloud storage cp "${ARTIFACT_URI}" "${LOCAL_ARTIFACT}"

echo "============================================================"
echo " [SUCCESS] Single Joint Portfolio HPO Completed!"
echo " Winning Artifact: ${LOCAL_ARTIFACT}"
echo "============================================================"
