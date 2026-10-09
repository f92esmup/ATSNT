#!/usr/bin/env bash
# =============================================================================
# ATSNT - Google Cloud Platform Resource Provisioning Script
#
# Sets up BigQuery dataset, tables, analytical views, and Cloud Storage bucket.
# Fully idempotent: safe to run multiple times.
#
# Usage:
#   export GCP_PROJECT_ID="your-project-id"
#   export GCP_REGION="US" # or us-central1
#   ./scripts/gcp/setup_gcp_resources.sh
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SQL_SCHEMA_FILE="${SCRIPT_DIR}/bigquery_schema.sql"

PROJECT_ID="${GCP_PROJECT_ID:-$(gcloud config get-value project 2>/dev/null || echo "")}"
REGION="${GCP_REGION:-EU}"
DATASET_ID="${GCP_DATASET_ID:-atsnt_bi}"
BUCKET_NAME="${GCS_BUCKET_NAME:-atsnt-lake-${PROJECT_ID}}"

echo "============================================================"
echo "    ATSNT - Google Cloud Platform Automated Provisioning    "
echo "============================================================"

if [ -z "${PROJECT_ID}" ] || [ "${PROJECT_ID}" = "(unset)" ]; then
    echo "[ERROR] GCP_PROJECT_ID is not set and could not be detected from gcloud."
    echo "        Please run: export GCP_PROJECT_ID=\"your-gcp-project-id\""
    exit 1
fi

echo "[*] GCP Project ID:  ${PROJECT_ID}"
echo "[*] Region:          ${REGION}"
echo "[*] BigQuery Dataset:${DATASET_ID}"
echo "[*] Cloud Storage:   gs://${BUCKET_NAME}"
echo "------------------------------------------------------------"

# 1. Enable Required GCP APIs
echo "[1/4] Enabling required Google Cloud APIs..."
gcloud services enable \
    bigquery.googleapis.com \
    storage.googleapis.com \
    --project="${PROJECT_ID}"

# 2. Provision Google Cloud Storage Bucket
echo "[2/4] Provisioning GCS Data Lake Bucket (gs://${BUCKET_NAME})..."
if ! gcloud storage buckets describe "gs://${BUCKET_NAME}" --project="${PROJECT_ID}" &>/dev/null; then
    echo "      Creating bucket gs://${BUCKET_NAME} in region ${REGION}..."
    gcloud storage buckets create "gs://${BUCKET_NAME}" \
        --project="${PROJECT_ID}" \
        --location="${REGION}" \
        --default-storage-class="STANDARD" \
        --uniform-bucket-level-access
    echo "      [OK] Bucket created successfully."
else
    echo "      [OK] Bucket gs://${BUCKET_NAME} already exists."
fi

# 3. Provision BigQuery Schema & Tables
echo "[3/4] Deploying BigQuery schemas and analytical views..."
if [ ! -f "${SQL_SCHEMA_FILE}" ]; then
    echo "[ERROR] SQL schema file not found at: ${SQL_SCHEMA_FILE}"
    exit 1
fi

sed "s/location = \"[^\"]*\"/location = \"${REGION}\"/g" "${SQL_SCHEMA_FILE}" | bq query \
    --project_id="${PROJECT_ID}" \
    --location="${REGION}" \
    --use_legacy_sql=false

echo "      [OK] BigQuery tables and views created in dataset '${DATASET_ID}'."

# 4. Display Quickstart Information
echo "[4/4] Environment Summary for ATSNT:"
echo "------------------------------------------------------------"
echo "Add the following environment variables to your shell or .env:"
echo ""
echo "export GCP_PROJECT_ID=\"${PROJECT_ID}\""
echo "export GCP_DATASET_ID=\"${DATASET_ID}\""
echo "export GCS_BUCKET_NAME=\"${BUCKET_NAME}\""
echo "export GCP_AUTH_TOKEN=\"\$(gcloud auth print-access-token)\""
echo ""
echo "============================================================"
echo " [SUCCESS] ATSNT Cloud-First infrastructure is ready!       "
echo "============================================================"
