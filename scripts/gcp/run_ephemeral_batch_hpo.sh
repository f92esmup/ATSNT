#!/usr/bin/env bash
# =============================================================================
# ATSNT - Ephemeral Cloud Batch HPO Orchestrator
#
# Runs the production pipeline (fetch -> HPO -> backtest + Monte Carlo) on a
# disposable Cloud Batch VM:
#   1. Builds and pushes the batch image to Artifact Registry.
#   2. Submits a Batch job (Spot by default) running the image with
#      --gcp-bigquery --strict-sinks --upload-winner.
#   3. Waits for the job, prints its task logs from Cloud Logging.
#   4. Downloads the winning artifact from gs://<bucket>/artifacts/ into configs/.
#
# Market data lives only on the VM's disk and is destroyed with it: nothing but
# the ~2 KB winning artifact is written to GCS. Metrics go to BigQuery.
#
# Prerequisites: scripts/gcp/setup_gcp_resources.sh (APIs, Artifact Registry
# repository, Batch service account and its permissions); the submitting user
# needs roles/iam.serviceAccountUser on the atsnt-batch service account; the
# region's default VPC must allow egress to data.binance.vision.
#
# Before building, it aborts when the winner artifact object already exists (the
# job uploads create-only, so a reused --job-name would keep the old winner) and
# when the service account lacks roles/bigquery.dataEditor on the tables the
# pipeline writes. Both checks are read-only and need python3 and bq.
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
cd "${PROJECT_ROOT}"

PROJECT_ID="${GCP_PROJECT_ID:-}"
REGION="${GCP_REGION:-europe-southwest1}"
BUCKET_NAME="${GCS_BUCKET_NAME:-}"
DATASET_ID="${GCP_DATASET_ID:-atsnt_bi}"
# Tables the pipeline streams rows into (same list as setup_gcp_resources.sh).
BATCH_WRITE_TABLES=(runs run_metrics trades run_series)
# Cloud Batch exit code for a task that failed because its Spot VM was preempted.
# A RETRY_TASK lifecycle policy retries only the listed exit codes; "tasks that fail
# with any unspecified exit codes are not retried", so other failures fail at once.
# https://docs.cloud.google.com/batch/docs/automate-task-retries
# https://docs.cloud.google.com/batch/docs/troubleshooting (exit code 50001)
PREEMPTION_EXIT_CODE="50001"
REPOSITORY="atsnt"
IMAGE_NAME="atsnt-batch"
IMAGE_TAG=""
SERVICE_ACCOUNT=""
MACHINE_TYPE="n2-highcpu-64"
PROVISIONING="SPOT"
MAX_RETRIES="2"
BOOT_DISK_GB="100"
MAX_RUN_SECONDS="14400"
POLL_SECONDS="30"
SYMBOL="BTCUSDT"
YEAR="$(date +%Y)"
MONTH=""
JOB_NAME=""
DOWNLOAD_DIR="configs"
SKIP_BUILD=false
NO_WAIT=false
DRY_RUN=false
JOB_SPEC_OUT=""
PIPELINE_EXTRA=()

show_help() {
    cat << EOF
ATSNT - Ephemeral Cloud Batch HPO Orchestrator

Usage:
  $(basename "$0") [OPTIONS] [-- <extra run_production_pipeline.sh options>]

Options:
      --project <ID>         GCP project (default: \$GCP_PROJECT_ID)
      --region <REGION>      Batch / Artifact Registry region (default: ${REGION})
      --bucket <NAME>        Artifact bucket (default: atsnt-lake-<project>)
  -s, --symbol <SYM>         Trading pair symbol (default: ${SYMBOL})
  -y, --year <YYYY>          Year to process (default: ${YEAR})
  -m, --month <MM>           Single completed month (default: whole year to date)
      --job-name <NAME>      Batch job name, also the artifact suffix; use a new one per
                             run, an existing winner artifact aborts the submission
                             (default: hpo-<sym>-<year>[-<MM>]-<YYYYMMDD-HHMMSS>)
      --machine-type <TYPE>  VM machine type (default: ${MACHINE_TYPE})
      --standard             Use a standard VM instead of Spot
      --max-retries <N>      Task retries after Spot preemption; any other failure
                             fails the job at once (default: ${MAX_RETRIES})
      --boot-disk-gb <N>     VM boot disk size; holds the market data (default: ${BOOT_DISK_GB})
      --max-run-seconds <N>  Hard limit for the task (default: ${MAX_RUN_SECONDS})
      --service-account <E>  VM service account (default: atsnt-batch@<project>.iam.gserviceaccount.com)
      --image-tag <TAG>      Image tag (default: git short SHA, '-dirty' if uncommitted changes)
      --skip-build           Reuse an already pushed image tag
      --download-dir <DIR>   Where the winning artifact is downloaded (default: ${DOWNLOAD_DIR})
      --no-wait              Submit and exit without waiting, logs or download
      --dry-run              Render the job spec and print the plan; call no gcloud/bq/docker
      --job-spec-out <PATH>  Also write the rendered job spec to PATH
  -h, --help                 Show this help message and exit

Examples:
  # January 2026 on a 60-vCPU Spot VM with 720 candidates:
  $(basename "$0") --project my-proj --year 2026 --month 1 -- --candidates 720 --capital 200

  # Inspect the job spec without touching GCP:
  $(basename "$0") --project my-proj --year 2026 --month 1 --dry-run
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
        -s|--symbol) SYMBOL="${2^^}"; shift 2 ;;
        -y|--year) YEAR="$2"; shift 2 ;;
        -m|--month) MONTH="$2"; shift 2 ;;
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
        --) shift; PIPELINE_EXTRA=("$@"); break ;;
        *)
            echo "[ERROR] Unknown option: $1" >&2
            echo "Run '$(basename "$0") --help' for usage." >&2
            exit 1
            ;;
    esac
done

# --- Validation (no GCP calls) ------------------------------------------------
[ -n "${PROJECT_ID}" ] || die "--project (or GCP_PROJECT_ID) is required."
[[ "${YEAR}" =~ ^[0-9]{4}$ ]] || die "--year must be YYYY (got '${YEAR}')."
for n in MAX_RETRIES BOOT_DISK_GB MAX_RUN_SECONDS; do
    [[ "${!n}" =~ ^[0-9]+$ ]] || die "${n,,} must be a non-negative integer (got '${!n}')."
done
# Cloud Batch accepts 0-10 task retries.
[ "${MAX_RETRIES}" -le 10 ] || die "--max-retries must be between 0 and 10 (got '${MAX_RETRIES}')."

# The orchestrator owns the options that decide where the winner is uploaded and
# downloaded from; overriding them after -- would make the announced paths wrong.
for arg in "${PIPELINE_EXTRA[@]}"; do
    case "${arg}" in
        -s|--symbol|-y|--year|-m|--month|--config|--artifact-suffix|--upload-winner|--strict-sinks|--gcp-bigquery|--bin-dir|-o|--output-dir|-d|--detach|--dry-run)
            die "Pipeline option ${arg} is set by this script; use the orchestrator option instead (see --help)."
            ;;
    esac
    case "${arg}" in
        *$'\n'*|*$'\r'*|*$'\t'*) die "Pipeline arguments must not contain newlines or tabs." ;;
    esac
done

MONTH_PADDED=""
if [ -n "${MONTH}" ]; then
    if ! [[ "${MONTH}" =~ ^[0-9]{1,2}$ ]] || [ $((10#${MONTH})) -lt 1 ] || [ $((10#${MONTH})) -gt 12 ]; then
        die "--month must be between 1 and 12 (got '${MONTH}')."
    fi
    MONTH_PADDED="$(printf "%02d" "$((10#${MONTH}))")"
fi

PERIOD="${YEAR}${MONTH_PADDED:+_${MONTH_PADDED}}"
if [ -z "${JOB_NAME}" ]; then
    JOB_NAME="hpo-${SYMBOL,,}-${YEAR}${MONTH_PADDED:+-${MONTH_PADDED}}-$(date -u +%Y%m%d-%H%M%S)"
fi
# Cloud Batch job IDs: lowercase letter first, then lowercase letters, digits or hyphens, max 63.
[[ "${JOB_NAME}" =~ ^[a-z]([a-z0-9-]{0,61}[a-z0-9])?$ ]] \
    || die "Invalid job name '${JOB_NAME}': use lowercase letters, digits and hyphens (max 63, starting with a letter)."

BUCKET_NAME="${BUCKET_NAME:-atsnt-lake-${PROJECT_ID}}"
SERVICE_ACCOUNT="${SERVICE_ACCOUNT:-atsnt-batch@${PROJECT_ID}.iam.gserviceaccount.com}"
if [ -z "${IMAGE_TAG}" ]; then
    IMAGE_TAG="$(git rev-parse --short=12 HEAD 2>/dev/null || echo latest)"
    if [ -n "$(git status --porcelain 2>/dev/null)" ]; then
        IMAGE_TAG="${IMAGE_TAG}-dirty"
    fi
fi
IMAGE_URI="${REGION}-docker.pkg.dev/${PROJECT_ID}/${REPOSITORY}/${IMAGE_NAME}:${IMAGE_TAG}"

CONFIG_STEM="hpo_${SYMBOL,,}_${PERIOD}"
ARTIFACT_OBJECT="artifacts/${CONFIG_STEM}-${JOB_NAME}.json"
ARTIFACT_URI="gs://${BUCKET_NAME}/${ARTIFACT_OBJECT}"
LOCAL_ARTIFACT="${DOWNLOAD_DIR}/${CONFIG_STEM}-${JOB_NAME}.json"

# Arguments for scripts/run_production_pipeline.sh inside the container.
PIPELINE_ARGS=(
    --symbol "${SYMBOL}"
    --year "${YEAR}"
)
[ -n "${MONTH_PADDED}" ] && PIPELINE_ARGS+=(--month "$((10#${MONTH_PADDED}))")
PIPELINE_ARGS+=(
    --config "/app/configs/${CONFIG_STEM}.json"
    --gcp-bigquery
    --strict-sinks
    --upload-winner
    --artifact-suffix "${JOB_NAME}"
)
if [ "${#PIPELINE_EXTRA[@]}" -gt 0 ]; then
    PIPELINE_ARGS+=("${PIPELINE_EXTRA[@]}")
fi

# --- Job spec -----------------------------------------------------------------
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
  "labels": { "app": "atsnt", "component": "hpo" },
  "logsPolicy": { "destination": "CLOUD_LOGGING" }
}
EOF
}

SPEC_FILE="$(mktemp "${TMPDIR:-/tmp}/atsnt-batch-job.XXXXXX.json")"
trap 'rm -f "${SPEC_FILE}"' EXIT
render_job_spec > "${SPEC_FILE}"
if [ -n "${JOB_SPEC_OUT}" ]; then
    cp "${SPEC_FILE}" "${JOB_SPEC_OUT}"
fi

echo "============================================================"
echo " ATSNT - Ephemeral Cloud Batch HPO"
echo "============================================================"
echo "Project:        ${PROJECT_ID}"
echo "Region:         ${REGION}"
echo "Job:            ${JOB_NAME}"
echo "Machine:        ${MACHINE_TYPE} (${PROVISIONING}, ${BOOT_DISK_GB} GB disk, ${MAX_RETRIES} retries on preemption)"
echo "Image:          ${IMAGE_URI}"
echo "Pipeline args:  ${PIPELINE_ARGS[*]}"
echo "Winner (GCS):   ${ARTIFACT_URI}"
echo "Winner (local): ${LOCAL_ARTIFACT}"
echo "Market data:    VM disk only (destroyed with the VM)"
echo "------------------------------------------------------------"

if [ "${DRY_RUN}" = "true" ]; then
    echo "[DRY-RUN] Job spec:"
    cat "${SPEC_FILE}"
    echo ""
    echo "[DRY-RUN] gcloud storage objects describe ${ARTIFACT_URI}  (must not exist)"
    for table in "${BATCH_WRITE_TABLES[@]}"; do
        echo "[DRY-RUN] bq get-iam-policy --format=prettyjson ${PROJECT_ID}:${DATASET_ID}.${table}  (${SERVICE_ACCOUNT} needs roles/bigquery.dataEditor)"
    done
    echo "[DRY-RUN] bq show --format=prettyjson ${PROJECT_ID}:${DATASET_ID}  (dataset-level grant)"
    for table in "${BATCH_WRITE_TABLES[@]}"; do
        echo "[DRY-RUN] bq show --schema --format=prettyjson ${PROJECT_ID}:${DATASET_ID}.${table}  (columns of scripts/gcp/bigquery_schema.sql)"
    done
    if [ "${SKIP_BUILD}" = "false" ]; then
        echo "[DRY-RUN] docker build --build-arg GIT_SHA=<sha> -t ${IMAGE_URI} ."
        echo "[DRY-RUN] docker push ${IMAGE_URI}"
    fi
    echo "[DRY-RUN] gcloud batch jobs submit ${JOB_NAME} --project=${PROJECT_ID} --location=${REGION} --config=<job spec>"
    echo "[DRY-RUN] gcloud storage cp ${ARTIFACT_URI} ${LOCAL_ARTIFACT}"
    echo "[DRY-RUN] No commands were executed."
    exit 0
fi

command -v gcloud >/dev/null 2>&1 || die "gcloud CLI not found."
command -v bq >/dev/null 2>&1 || die "bq CLI not found (needed to check the BigQuery grants)."
command -v python3 >/dev/null 2>&1 || die "python3 not found (needed to read the BigQuery IAM policies)."

# --- Preflight: the winner artifact does not exist yet -------------------------
# run_hpo uploads create-only: with a reused job name the upload is refused and
# the old artifact would be downloaded as if it were this run's winner.
echo "[1/5] (a) Checking that ${ARTIFACT_URI} does not exist..."
if DESCRIBE_OUT="$(gcloud storage objects describe "${ARTIFACT_URI}" --project="${PROJECT_ID}" 2>&1)"; then
    die "Winner artifact ${ARTIFACT_URI} already exists (job name ${JOB_NAME} was used before). Pass a new --job-name."
elif ! grep -qiE "not found|404|matched no objects|No URLs matched" <<< "${DESCRIBE_OUT}"; then
    die "Could not check whether ${ARTIFACT_URI} exists: ${DESCRIBE_OUT}"
fi
echo "      [OK] Not uploaded yet."

# --- Preflight: the service account can write the BigQuery tables --------------
# Accepted: roles/bigquery.dataEditor (or dataOwner) on each table, or a
# dataset-level WRITER/OWNER/dataEditor/dataOwner entry. Project-level grants are
# not inspected.
echo "[1/5] (b) Checking BigQuery write grants of ${SERVICE_ACCOUNT} on ${DATASET_ID}..."
# Exit 0 when the JSON on stdin grants SA ($1) write access. Mode ($2): "table"
# reads an IAM policy, "dataset" reads the access list of `bq show`.
grants_write() {
    python3 -I -c '
import json, sys
sa, mode = sys.argv[1], sys.argv[2]
doc = json.load(sys.stdin)
if mode == "table":
    roles = {"roles/bigquery.dataEditor", "roles/bigquery.dataOwner"}
    ok = any(b.get("role") in roles and "serviceAccount:" + sa in b.get("members", [])
             for b in doc.get("bindings", []))
else:
    roles = {"WRITER", "OWNER", "roles/bigquery.dataEditor", "roles/bigquery.dataOwner"}
    ok = any(e.get("role") in roles
             and (e.get("userByEmail") == sa or e.get("iamMember") == "serviceAccount:" + sa)
             for e in doc.get("access", []))
sys.exit(0 if ok else 1)
' "$1" "$2"
}
DATASET_REF="${PROJECT_ID}:${DATASET_ID}"
DATASET_JSON="$(bq show --format=prettyjson "${DATASET_REF}" 2>&1)" \
    || die "Could not read dataset ${DATASET_REF}: ${DATASET_JSON}"
if grants_write "${SERVICE_ACCOUNT}" dataset <<< "${DATASET_JSON}"; then
    echo "      [OK] Dataset-level write access."
else
    MISSING_TABLES=()
    for table in "${BATCH_WRITE_TABLES[@]}"; do
        POLICY="$(bq get-iam-policy --format=prettyjson "${DATASET_REF}.${table}" 2>&1)" \
            || die "Could not read the IAM policy of ${DATASET_REF}.${table}: ${POLICY}"
        grants_write "${SERVICE_ACCOUNT}" table <<< "${POLICY}" || MISSING_TABLES+=("${table}")
    done
    if [ "${#MISSING_TABLES[@]}" -gt 0 ]; then
        {
            echo "[ERROR] ${SERVICE_ACCOUNT} lacks roles/bigquery.dataEditor on: ${MISSING_TABLES[*]}"
            echo "        The job would fail with a BigQuery 403 after the whole pipeline ran."
            echo "        Recreating a table (e.g. a schema migration) drops its table-level grants."
            echo "        Re-apply them (as in scripts/gcp/setup_gcp_resources.sh):"
            for table in "${MISSING_TABLES[@]}"; do
                echo "  bq add-iam-policy-binding --member=serviceAccount:${SERVICE_ACCOUNT} --role=roles/bigquery.dataEditor ${DATASET_REF}.${table}"
            done
        } >&2
        exit 1
    fi
    echo "      [OK] Table-level write access on: ${BATCH_WRITE_TABLES[*]}"
fi

# --- Preflight: the tables have every column the engine writes ------------------
# A column added to the DDL (ALTER TABLE ... ADD COLUMN IF NOT EXISTS) reaches a deployed
# dataset only when the DDL is re-run; until then the rows that carry it are rejected
# after the whole search. The expected columns come from the DDL itself.
SCHEMA_SQL="${SCRIPT_DIR}/bigquery_schema.sql"
echo "[1/5] (c) Checking the BigQuery columns of ${DATASET_ID} against ${SCHEMA_SQL#"${PROJECT_ROOT}/"}..."
MISSING_COLUMNS=()
for table in "${BATCH_WRITE_TABLES[@]}"; do
    LIVE_SCHEMA="$(bq show --schema --format=prettyjson "${DATASET_REF}.${table}" 2>&1)" \
        || die "Could not read the schema of ${DATASET_REF}.${table}: ${LIVE_SCHEMA}"
    EXPECTED="$(python3 -I "${SCRIPT_DIR}/ddl_columns.py" "${SCHEMA_SQL}" "${table}")" \
        || die "Could not read the columns of ${table} from ${SCHEMA_SQL}"
    MISSING="$(python3 -I -c '
import json, sys
live = {c["name"] for c in json.loads(sys.argv[1])}
print(" ".join(n for n in sys.argv[2].split() if n not in live))
' "${LIVE_SCHEMA}" "${EXPECTED}")" || die "Could not parse the schema of ${DATASET_REF}.${table}"
    [ -n "${MISSING}" ] && MISSING_COLUMNS+=("${table}: ${MISSING}")
done
if [ "${#MISSING_COLUMNS[@]}" -gt 0 ]; then
    {
        echo "[ERROR] The deployed tables lack columns the engine writes:"
        for entry in "${MISSING_COLUMNS[@]}"; do echo "        ${entry}"; done
        echo "        Rows carrying them would be rejected after the whole search. Apply the"
        echo "        idempotent DDL (it only adds columns and refreshes views):"
        echo "  bq query --use_legacy_sql=false --project_id=${PROJECT_ID} < ${SCHEMA_SQL#"${PROJECT_ROOT}/"}"
    } >&2
    exit 1
fi
echo "      [OK] The tables have every column of ${SCHEMA_SQL#"${PROJECT_ROOT}/"}"

# --- Preflight: machine type available in the region, CPU quota free ----------
echo "[1/5] (d) Checking that ${MACHINE_TYPE} exists in ${REGION}..."
# `machine-types list` omits custom types (e.g. n2-custom-30-30720), so describe
# the type in each zone of the region instead.
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
[ -n "${ZONES}" ] || die "Machine type ${MACHINE_TYPE} is not offered in ${REGION}. Pick another with --machine-type or --region."
echo "      [OK] Available in: ${ZONES}(${GUEST_CPUS} vCPU)"

# Free and limit vCPUs of a quota metric: "<scope> describe" output, metric name.
limit_cpus() {
    awk -F, -v metric="$2" '$1 == metric { print int($2) }' <<< "$1"
}
free_cpus() {
    awk -F, -v metric="$2" '$1 == metric { print int($2 - $3) }' <<< "$1"
}
# Without the quota Batch keeps the job QUEUED forever (CODE_GCE_QUOTA_EXCEEDED).
GLOBAL_QUOTAS="$(gcloud compute project-info describe --project="${PROJECT_ID}" --flatten=quotas \
    --format="csv[no-heading](quotas.metric,quotas.limit,quotas.usage)" 2>/dev/null || true)"
REGION_QUOTAS="$(gcloud compute regions describe "${REGION}" --project="${PROJECT_ID}" --flatten=quotas \
    --format="csv[no-heading](quotas.metric,quotas.limit,quotas.usage)" 2>/dev/null || true)"
# Regional vCPUs are charged to the family metric (N2_CPUS, C2_CPUS...) when it
# exists, else to CPUS (N1); Spot uses PREEMPTIBLE_CPUS when that quota is granted.
REGION_METRIC="$(tr '[:lower:]' '[:upper:]' <<< "${MACHINE_TYPE%%-*}")_CPUS"
grep -q "^${REGION_METRIC}," <<< "${REGION_QUOTAS}" || REGION_METRIC="CPUS"
if [ "${PROVISIONING}" = "SPOT" ] && awk -F, '$1 == "PREEMPTIBLE_CPUS" && $2 > 0 { found = 1 } END { exit !found }' <<< "${REGION_QUOTAS}"; then
    REGION_METRIC="PREEMPTIBLE_CPUS"
fi
for check in "CPUS_ALL_REGIONS:${GLOBAL_QUOTAS}" "${REGION_METRIC}:${REGION_QUOTAS}"; do
    metric="${check%%:*}"
    limit="$(limit_cpus "${check#*:}" "${metric}")"
    free="$(free_cpus "${check#*:}" "${metric}")"
    if [ -z "${limit}" ]; then
        echo "      [WARN] Could not read quota ${metric}; skipping the check."
    elif [ "${limit}" -lt "${GUEST_CPUS}" ]; then
        die "Quota ${metric} has total limit ${limit} vCPU but ${MACHINE_TYPE} needs ${GUEST_CPUS}. Request a quota increase or pick a smaller --machine-type."
    elif [ "${free}" -lt "${GUEST_CPUS}" ]; then
        echo "      [INFO] Quota ${metric} currently has ${free} free vCPU (active jobs running). Job will queue in Cloud Batch until capacity is released."
    fi
done
echo "      [OK] CPU quota allows ${GUEST_CPUS} vCPU."

# --- Build and push image -----------------------------------------------------
if [ "${SKIP_BUILD}" = "false" ]; then
    command -v docker >/dev/null 2>&1 || die "docker not found (or use --skip-build with an existing --image-tag)."
    echo "[2/5] Building and pushing ${IMAGE_URI}..."
    gcloud auth configure-docker "${REGION}-docker.pkg.dev" --quiet >/dev/null
    docker build --build-arg GIT_SHA="$(git rev-parse --short=12 HEAD 2>/dev/null || true)" -t "${IMAGE_URI}" .
    docker push "${IMAGE_URI}"
else
    echo "[2/5] [SKIPPED] Image build (using ${IMAGE_URI})"
fi

# --- Submit -------------------------------------------------------------------
echo "[3/5] Submitting Batch job ${JOB_NAME}..."
gcloud batch jobs submit "${JOB_NAME}" \
    --project="${PROJECT_ID}" \
    --location="${REGION}" \
    --config="${SPEC_FILE}"

if [ "${NO_WAIT}" = "true" ]; then
    echo "[OK] Submitted. Follow it with:"
    echo "  gcloud batch jobs describe ${JOB_NAME} --project=${PROJECT_ID} --location=${REGION}"
    echo "Download the winner when it succeeds with:"
    echo "  gcloud storage cp ${ARTIFACT_URI} ${LOCAL_ARTIFACT}"
    exit 0
fi

# --- Wait ---------------------------------------------------------------------
echo "[4/5] Waiting for job ${JOB_NAME} (polling every ${POLL_SECONDS}s)..."
STATE=""
DESCRIBE_FAILURES=0
MAX_DESCRIBE_FAILURES=10
while true; do
    if STATE="$(gcloud batch jobs describe "${JOB_NAME}" --project="${PROJECT_ID}" \
        --location="${REGION}" --format="value(status.state)" 2>/dev/null)"; then
        DESCRIBE_FAILURES=0
    else
        DESCRIBE_FAILURES=$((DESCRIBE_FAILURES + 1))
        STATE="UNKNOWN"
        if [ "${DESCRIBE_FAILURES}" -ge "${MAX_DESCRIBE_FAILURES}" ]; then
            die "Could not read the state of job ${JOB_NAME} ${MAX_DESCRIBE_FAILURES} times in a row (deleted job or lost credentials?)."
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
if [ -z "${JOB_UID}" ]; then
    echo "[WARN] Could not read the job uid; task logs are not available."
else
gcloud logging read \
    "logName=\"projects/${PROJECT_ID}/logs/batch_task_logs\" AND labels.job_uid=\"${JOB_UID}\"" \
    --project="${PROJECT_ID}" --order=asc --limit=100000 --format="value(textPayload)" || \
    echo "[WARN] Could not read task logs; they may take a minute to appear in Cloud Logging."
fi

if [ "${STATE}" != "SUCCEEDED" ]; then
    die "Batch job ${JOB_NAME} finished with state ${STATE}."
fi

# --- Download winner ----------------------------------------------------------
echo "[5/5] Downloading winning artifact..."
mkdir -p "${DOWNLOAD_DIR}"
if gcloud storage objects describe "${ARTIFACT_URI}" --project="${PROJECT_ID}" >/dev/null 2>&1; then
    gcloud storage cp "${ARTIFACT_URI}" "${LOCAL_ARTIFACT}"
else
    echo "[INFO] No winner artifact uploaded to GCS (e.g. --skip-hpo run). Using local artifact."
fi

echo "============================================================"
echo " [SUCCESS] Ephemeral HPO completed; the VM and its market data are gone."
echo " Winning artifact:  ${LOCAL_ARTIFACT}"
echo " Run analytics:     BigQuery ${DATASET_ID} (v_runs, v_trades, v_equity)"
echo " Next: ./scripts/gcp/deploy_to_vm.sh ${LOCAL_ARTIFACT}"
echo "============================================================"
