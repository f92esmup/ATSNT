#!/usr/bin/env bash
set -euo pipefail

PROJECT_ID="${GCP_PROJECT_ID:-$(gcloud config get-value project 2>/dev/null || echo "")}"
if [ -z "$PROJECT_ID" ]; then
    echo "[ERROR] GCP Project ID not found. Set GCP_PROJECT_ID or run 'gcloud config set project <ID>'."
    exit 1
fi

if [ ! -f .env ]; then
    echo "[ERROR] .env file not found."
    exit 1
fi

echo "============================================================"
echo "    ATSNT - Syncing Secrets to GCP Secret Manager"
echo "============================================================"
echo "[*] GCP Project ID: $PROJECT_ID"

# Read keys from .env
API_KEY=$(grep -E '^BINANCE_API_KEY=' .env | cut -d '=' -f2- | tr -d '"' | tr -d "'")
SECRET_KEY=$(grep -E '^BINANCE_SECRET_KEY=' .env | cut -d '=' -f2- | tr -d '"' | tr -d "'")

if [ -z "$API_KEY" ] || [ -z "$SECRET_KEY" ]; then
    echo "[ERROR] BINANCE_API_KEY or BINANCE_SECRET_KEY is empty in .env"
    exit 1
fi

upload_secret() {
    local secret_name="$1"
    local secret_val="$2"

    if gcloud secrets describe "$secret_name" --project="$PROJECT_ID" >/dev/null 2>&1; then
        echo "[*] Adding new version to existing secret: $secret_name"
        printf "%s" "$secret_val" | gcloud secrets versions add "$secret_name" --data-file=- --project="$PROJECT_ID" >/dev/null
    else
        echo "[*] Creating secret: $secret_name"
        printf "%s" "$secret_val" | gcloud secrets create "$secret_name" --data-file=- --replication-policy=automatic --project="$PROJECT_ID" >/dev/null
    fi
    echo "    [OK] Secret $secret_name synced successfully."
}

upload_secret "BINANCE_API_KEY" "$API_KEY"
upload_secret "BINANCE_SECRET_KEY" "$SECRET_KEY"

echo "============================================================"
echo "[SUCCESS] Secrets are now securely stored in GCP Secret Manager!"
echo "============================================================"
