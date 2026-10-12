#!/usr/bin/env bash
# ==============================================================================
# ATSNT - Google Cloud Compute Engine Deployment Automation Script
#
# Idempotent and safe deployment script:
# 1. Builds optimized release binary locally (avoiding VM OOM)
# 2. Performs graceful shutdown of running paper_trading engine (SIGINT liquidation)
# 3. Creates backup of existing VM binary
# 4. Deploys updated binary, canonical deployment artifact, and systemd service
# 5. Reloads and starts systemd service, validating startup telemetry
# ==============================================================================
set -euo pipefail

INSTANCE_NAME="${GCP_VM_INSTANCE:-atsnt-engine}"
ZONE="${GCP_VM_ZONE:-europe-west1-b}"
REMOTE_USER="${GCP_VM_USER:-f92esmup}"
REMOTE_DIR="/home/${REMOTE_USER}/atsnt"
CONFIG_FILE="${1:-configs/hpo_btcusdt_2026.json}"

echo "=============================================================================="
echo " ATSNT - Safe Cloud Deployment Pipeline -> VM: ${INSTANCE_NAME} (${ZONE})"
echo "=============================================================================="

# 1. Preflight Checks
if ! command -v gcloud &> /dev/null; then
    echo "[-] Error: 'gcloud' CLI is not installed or not in PATH."
    exit 1
fi

if [ ! -f "${CONFIG_FILE}" ]; then
    echo "[-] Error: Configuration artifact '${CONFIG_FILE}' not found."
    exit 1
fi

echo "[*] Step 1: Compiling 'paper_trading' locally in release mode..."
cargo build --release -p backtest --bin paper_trading
LOCAL_BIN="target/release/paper_trading"
if [ ! -f "${LOCAL_BIN}" ]; then
    echo "[-] Error: Local binary '${LOCAL_BIN}' was not produced."
    exit 1
fi

echo "[*] Step 2: Checking VM status..."
gcloud compute ssh "${INSTANCE_NAME}" --zone="${ZONE}" --command="systemctl is-active atsnt.service || true"

echo "[*] Step 3: Gracefully stopping running engine (KillSignal=SIGINT liquidation)..."
gcloud compute ssh "${INSTANCE_NAME}" --zone="${ZONE}" --command="sudo systemctl stop atsnt.service"

echo "[*] Step 4: Backing up previous binary and preparing remote directories..."
CONFIG_BASENAME=$(basename "${CONFIG_FILE}")
gcloud compute ssh "${INSTANCE_NAME}" --zone="${ZONE}" --command="
    mkdir -p ${REMOTE_DIR}/configs ${REMOTE_DIR}/storage/reports
    if [ -f ${REMOTE_DIR}/paper_trading ]; then
        cp ${REMOTE_DIR}/paper_trading ${REMOTE_DIR}/paper_trading.bak
        echo '[*] Backup preserved: ${REMOTE_DIR}/paper_trading.bak'
    fi
"

echo "[*] Step 5: Transferring binary, strategy artifact, and systemd unit..."
gcloud compute scp "${LOCAL_BIN}" "${INSTANCE_NAME}:${REMOTE_DIR}/paper_trading.new" --zone="${ZONE}"
gcloud compute scp "${CONFIG_FILE}" "${INSTANCE_NAME}:${REMOTE_DIR}/configs/${CONFIG_BASENAME}" --zone="${ZONE}"
gcloud compute scp docs/deployment/atsnt.service "${INSTANCE_NAME}:${REMOTE_DIR}/atsnt.service" --zone="${ZONE}"

echo "[*] Step 6: Atomically replacing binary and updating systemd service..."
gcloud compute ssh "${INSTANCE_NAME}" --zone="${ZONE}" --command="
    chmod +x ${REMOTE_DIR}/paper_trading.new
    mv ${REMOTE_DIR}/paper_trading.new ${REMOTE_DIR}/paper_trading
    sudo cp ${REMOTE_DIR}/atsnt.service /etc/systemd/system/atsnt.service
    sudo systemctl daemon-reload
"

echo "[*] Step 7: Starting new 'atsnt.service'..."
gcloud compute ssh "${INSTANCE_NAME}" --zone="${ZONE}" --command="sudo systemctl start atsnt.service"

sleep 3

echo "[*] Step 8: Verifying service health and startup logs..."
gcloud compute ssh "${INSTANCE_NAME}" --zone="${ZONE}" --command="systemctl status atsnt.service --no-pager"
echo ""
echo "=== Last 25 log lines from journalctl ==="
gcloud compute ssh "${INSTANCE_NAME}" --zone="${ZONE}" --command="journalctl -u atsnt.service -n 25 --no-pager"

echo "=============================================================================="
echo " [*] Deployment successfully completed and verified!"
echo "=============================================================================="
