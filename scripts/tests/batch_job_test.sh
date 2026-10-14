#!/usr/bin/env bash
# =============================================================================
# Tests for scripts/gcp/run_ephemeral_batch_hpo.sh in --dry-run mode.
#
# Dry-run renders the Cloud Batch job spec and prints the commands it would run.
# Stub gcloud/docker binaries record any invocation so the test proves that
# dry-run never touches GCP or Docker.
#
# Usage: bash scripts/tests/batch_job_test.sh
# =============================================================================

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ORCHESTRATOR="${SCRIPT_DIR}/../gcp/run_ephemeral_batch_hpo.sh"

WORK_DIR="$(mktemp -d "${TMPDIR:-${HOME}/.cache}/atsnt-batch-test.XXXXXX")"
trap 'rm -rf "${WORK_DIR}"' EXIT

STUB_DIR="${WORK_DIR}/stubs"
CALLS="${WORK_DIR}/calls.log"
mkdir -p "${STUB_DIR}"
for tool in gcloud docker bq; do
    printf '#!/usr/bin/env bash\necho "%s $*" >> "%s"\nexit 0\n' "${tool}" "${CALLS}" > "${STUB_DIR}/${tool}"
    chmod +x "${STUB_DIR}/${tool}"
done
: > "${CALLS}"

PASSED=0
FAILED=0
OUT=""
STATUS=0
JOB_JSON="${WORK_DIR}/job.json"

run() {
    rm -f "${JOB_JSON}"
    OUT="$(env -u GCP_PROJECT_ID -u GCP_REGION -u GCS_BUCKET_NAME PATH="${STUB_DIR}:${PATH}" bash "${ORCHESTRATOR}" --dry-run --job-spec-out "${JOB_JSON}" "$@" 2>&1)"
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

# Reads a value from the rendered job spec with a Python expression over `j`.
spec() { python3 -I -c "import json,sys; j=json.load(open(sys.argv[1])); print($1)" "${JOB_JSON}"; }

BASE=(--project my-proj --region europe-southwest1 --job-name hpo-test-1 --year 2026 --month 1)

# Rendered job spec: Spot VM, retries, sizing, service account, logging.
run "${BASE[@]}"
check "dry-run: exit 0" '[ "${STATUS}" -eq 0 ]'
check "dry-run: job spec written" '[ -s "${JOB_JSON}" ]'
check "spec: SPOT provisioning" '[ "$(spec "j[\"allocationPolicy\"][\"instances\"][0][\"policy\"][\"provisioningModel\"]")" = "SPOT" ]'
check "spec: default machine type" '[ "$(spec "j[\"allocationPolicy\"][\"instances\"][0][\"policy\"][\"machineType\"]")" = "c2-standard-60" ]'
check "spec: boot disk sized" '[ "$(spec "j[\"allocationPolicy\"][\"instances\"][0][\"policy\"][\"bootDisk\"][\"sizeGb\"]")" = "100" ]'
check "spec: retries" '[ "$(spec "j[\"taskGroups\"][0][\"taskSpec\"][\"maxRetryCount\"]")" = "2" ]'
check "spec: region restricted" '[ "$(spec "j[\"allocationPolicy\"][\"location\"][\"allowedLocations\"][0]")" = "regions/europe-southwest1" ]'
check "spec: dedicated service account" '[ "$(spec "j[\"allocationPolicy\"][\"serviceAccount\"][\"email\"]")" = "atsnt-batch@my-proj.iam.gserviceaccount.com" ]'
check "spec: logs to Cloud Logging" '[ "$(spec "j[\"logsPolicy\"][\"destination\"]")" = "CLOUD_LOGGING" ]'
check "spec: image from Artifact Registry" '[[ "$(spec "j[\"taskGroups\"][0][\"taskSpec\"][\"runnables\"][0][\"container\"][\"imageUri\"]")" == europe-southwest1-docker.pkg.dev/my-proj/atsnt/atsnt-batch:* ]]'
check "spec: container env project" '[ "$(spec "j[\"taskGroups\"][0][\"taskSpec\"][\"runnables\"][0][\"environment\"][\"variables\"][\"GCP_PROJECT_ID\"]")" = "my-proj" ]'
check "spec: container env bucket" '[ "$(spec "j[\"taskGroups\"][0][\"taskSpec\"][\"runnables\"][0][\"environment\"][\"variables\"][\"GCS_BUCKET_NAME\"]")" = "atsnt-lake-my-proj" ]'

ARGS_EXPR='" ".join(j["taskGroups"][0]["taskSpec"]["runnables"][0]["container"]["commands"])'
check "args: strict sinks + upload + bigquery" '[[ "$(spec "${ARGS_EXPR}")" == *"--gcp-bigquery"*"--strict-sinks"*"--upload-winner"* ]]'
check "args: artifact suffix is the job name" '[[ "$(spec "${ARGS_EXPR}")" == *"--artifact-suffix hpo-test-1"* ]]'
check "args: period forwarded" '[[ "$(spec "${ARGS_EXPR}")" == *"--year 2026 --month 1"* ]]'
check "args: config path in container" '[[ "$(spec "${ARGS_EXPR}")" == *"--config /app/configs/hpo_btcusdt_2026_01.json"* ]]'
check "plan: artifact object announced" 'has "gs://atsnt-lake-my-proj/artifacts/hpo_btcusdt_2026_01-hpo-test-1.json"'
check "plan: download target announced" 'has "configs/hpo_btcusdt_2026_01-hpo-test-1.json"'
check "plan: no market data copied to GCS" '! grep -E "storage cp .*(parquet|data)" <<< "${OUT}"'
check "dry-run: gcloud/docker never invoked" '[ ! -s "${CALLS}" ]'

# Extra pipeline arguments after -- are appended for the container.
run "${BASE[@]}" -- --candidates 720 --capital 200
check "passthrough: extra pipeline args" '[[ "$(spec "${ARGS_EXPR}")" == *"--candidates 720 --capital 200"* ]]'

# Overrides.
run "${BASE[@]}" --machine-type n2-standard-32 --standard --max-retries 0 --boot-disk-gb 200 --symbol ethusdt
check "override: machine type" '[ "$(spec "j[\"allocationPolicy\"][\"instances\"][0][\"policy\"][\"machineType\"]")" = "n2-standard-32" ]'
check "override: standard provisioning" '[ "$(spec "j[\"allocationPolicy\"][\"instances\"][0][\"policy\"][\"provisioningModel\"]")" = "STANDARD" ]'
check "override: retries" '[ "$(spec "j[\"taskGroups\"][0][\"taskSpec\"][\"maxRetryCount\"]")" = "0" ]'
check "override: boot disk" '[ "$(spec "j[\"allocationPolicy\"][\"instances\"][0][\"policy\"][\"bootDisk\"][\"sizeGb\"]")" = "200" ]'
check "override: symbol upper-cased" '[[ "$(spec "${ARGS_EXPR}")" == *"--symbol ETHUSDT"* ]] && has "hpo_ethusdt_2026_01-hpo-test-1.json"'

# Full-year run (no month) uses year-only names.
run --project my-proj --job-name hpo-year --year 2025
check "year: config name without month" '[[ "$(spec "${ARGS_EXPR}")" == *"--config /app/configs/hpo_btcusdt_2025.json"* ]] && ! [[ "$(spec "${ARGS_EXPR}")" == *"--month"* ]]'

# Validation.
run --region europe-southwest1 --job-name hpo-x --year 2026
check "validation: missing project rejected" '[ "${STATUS}" -ne 0 ] && has "--project"'
run --project my-proj --job-name Bad_Name --year 2026
check "validation: invalid job name rejected" '[ "${STATUS}" -ne 0 ] && has "job name"'
run --project my-proj --job-name hpo-x --year 2026 --month 13
check "validation: invalid month rejected" '[ "${STATUS}" -ne 0 ] && has "--month"'
run --project my-proj --job-name hpo-x --year 2026 --bogus
check "validation: unknown option rejected" '[ "${STATUS}" -ne 0 ] && has "Unknown option: --bogus"'

run --project my-proj --job-name hpo-x --year 2026 --max-retries 11
check "validation: retries above Batch limit rejected" '[ "${STATUS}" -ne 0 ] && has "--max-retries must be between 0 and 10"'
run --project my-proj --job-name hpo-x --year 2026 -- --config other.json
check "validation: reserved pipeline option rejected" '[ "${STATUS}" -ne 0 ] && has "Pipeline option --config is set by this script"'
run --project my-proj --job-name hpo-x --year 2026 -- --month 2
check "validation: period cannot be overridden after --" '[ "${STATUS}" -ne 0 ] && has "Pipeline option --month"'
run --project my-proj --job-name hpo-x --year 2026 -- --param-space $'a\nb'
check "validation: newline in pipeline args rejected" '[ "${STATUS}" -ne 0 ] && has "newlines or tabs"'
run --project my-proj --job-name hpo-x --year 2026 -- --param-space 'a "b" c\d'
check "escaping: quotes and backslashes survive in the job JSON" '[ "$(spec "j[\"taskGroups\"][0][\"taskSpec\"][\"runnables\"][0][\"container\"][\"commands\"][-1]")" = "a \"b\" c\\d" ]'

# Default job name is valid and derived from the period.
run --project my-proj --year 2026 --month 3
check "default job name: derived and valid" '[[ "$(spec "${ARGS_EXPR}")" =~ --artifact-suffix\ hpo-btcusdt-2026-03-[0-9]{8}-[0-9]{6} ]]'

check "dry-run: gcloud/docker never invoked (all cases)" '[ ! -s "${CALLS}" ]'

echo ""
echo "${PASSED} passed, ${FAILED} failed"
[ "${FAILED}" -eq 0 ]
