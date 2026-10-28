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
CONFIG_FILE="${1:-configs/portfolio_deployment_artifact.json}"

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

# The unit, not this argument, decides what the VM runs: deploying one artifact while the unit
# starts another would silently trade the wrong configuration.
UNIT_FILE="docs/deployment/atsnt.service"
CONFIG_BASENAME="$(basename "${CONFIG_FILE}")"
# Only the active ExecStart line counts (not comments), and the name is compared literally.
# `|| true`: under `set -e` a unit without ExecStart= must reach the error below, not exit silently.
EXEC_START="$(grep -E '^ExecStart=' "${UNIT_FILE}" || true)"
case " ${EXEC_START} " in
    *"/configs/${CONFIG_BASENAME} "*) ;;
    *)
        echo "[-] Error: the ExecStart of ${UNIT_FILE} does not start '${CONFIG_BASENAME}'."
        echo "    Point its --config at the artifact you deploy, then re-run."
        exit 1
        ;;
esac

if echo "${EXEC_START}" | grep -q "paper_trading_portfolio"; then
    BIN_NAME="paper_trading_portfolio"
else
    BIN_NAME="paper_trading"
fi

echo "[*] Step 1: Compiling '${BIN_NAME}' locally in release mode..."
cargo build --release -p backtest --bin "${BIN_NAME}"
LOCAL_BIN="target/release/${BIN_NAME}"
if [ ! -f "${LOCAL_BIN}" ]; then
    echo "[-] Error: Local binary '${LOCAL_BIN}' was not produced."
    exit 1
fi

echo "[*] Step 2: Checking VM status..."
gcloud compute ssh "${INSTANCE_NAME}" --zone="${ZONE}" --command="systemctl is-active atsnt.service || true"

echo "[*] Step 3: Gracefully stopping running engine (KillSignal=SIGINT liquidation)..."
gcloud compute ssh "${INSTANCE_NAME}" --zone="${ZONE}" --command="sudo systemctl stop atsnt.service"

echo "[*] Step 4: Backing up previous binary and preparing remote directories..."
gcloud compute ssh "${INSTANCE_NAME}" --zone="${ZONE}" --command="
    mkdir -p ${REMOTE_DIR}/configs
    if [ -f ${REMOTE_DIR}/${BIN_NAME} ]; then
        cp ${REMOTE_DIR}/${BIN_NAME} ${REMOTE_DIR}/${BIN_NAME}.bak
        echo '[*] Backup preserved: ${REMOTE_DIR}/${BIN_NAME}.bak'
    fi
"

echo "[*] Step 5: Transferring binary, strategy artifact, and systemd unit..."
gcloud compute scp "${LOCAL_BIN}" "${INSTANCE_NAME}:${REMOTE_DIR}/${BIN_NAME}.new" --zone="${ZONE}"
gcloud compute scp "${CONFIG_FILE}" "${INSTANCE_NAME}:${REMOTE_DIR}/configs/${CONFIG_BASENAME}" --zone="${ZONE}"
gcloud compute scp docs/deployment/atsnt.service "${INSTANCE_NAME}:${REMOTE_DIR}/atsnt.service" --zone="${ZONE}"

echo "[*] Step 6: Atomically replacing binary and updating systemd service..."
gcloud compute ssh "${INSTANCE_NAME}" --zone="${ZONE}" --command="
    chmod +x ${REMOTE_DIR}/${BIN_NAME}.new
    mv ${REMOTE_DIR}/${BIN_NAME}.new ${REMOTE_DIR}/${BIN_NAME}
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
