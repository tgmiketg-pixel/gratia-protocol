#!/usr/bin/env bash
# deploy-bootstrap.sh — Deploy a Gratia bootstrap node to a remote VPS.
#
# This script copies the project source to a remote server, builds the
# gratia-bootstrap binary, installs a systemd service, and verifies the
# health endpoint responds.
#
# Usage:
#   ./scripts/deploy-bootstrap.sh \
#     --host root@NEW_IP \
#     --ssh-key ~/.ssh/gratia_bootstrap \
#     --node-index 2 \
#     --peer "/ip4/45.77.95.111/udp/9000/quic-v1/p2p/12D3KooWRUqRqDGpQwLtxMP6iGfKEjZYWnkgkiW5BLPyxAeB8gLF" \
#     --peer "/ip4/45.77.95.111/tcp/9001/p2p/12D3KooWRUqRqDGpQwLtxMP6iGfKEjZYWnkgkiW5BLPyxAeB8gLF"
#
# Prerequisites:
#   - SSH access to the target server (key-based auth)
#   - Server has Ubuntu 22.04+ with at least 1 GB RAM
#   - Ports 9000/udp, 9001/tcp, 8080/tcp open in cloud firewall

set -euo pipefail

# ─── Argument parsing ────────────────────────────────────────────────
HOST=""
SSH_KEY=""
NODE_INDEX=""
PEERS=()
PORT=9000
HEALTH_PORT=8080
DATA_DIR="/opt/gratia-bootstrap"
REMOTE_SRC="/opt/gratia-src"
DRY_RUN=false

usage() {
    cat <<EOF
Usage: $0 --host USER@IP --ssh-key PATH --node-index N [--peer MULTIADDR ...] [OPTIONS]

Required:
  --host          SSH destination (e.g. root@1.2.3.4)
  --ssh-key       Path to SSH private key
  --node-index    Unique bootstrap node index (1, 2, 3, ...)

Optional:
  --peer          Multiaddr of another bootstrap node (repeatable)
  --port          QUIC listen port (default: 9000, TCP = port+1)
  --health-port   Health check HTTP port (default: 8080)
  --data-dir      Persistent data directory on remote (default: /opt/gratia-bootstrap)
  --dry-run       Print commands without executing
EOF
    exit 1
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --host)       HOST="$2"; shift 2 ;;
        --ssh-key)    SSH_KEY="$2"; shift 2 ;;
        --node-index) NODE_INDEX="$2"; shift 2 ;;
        --peer)       PEERS+=("$2"); shift 2 ;;
        --port)       PORT="$2"; shift 2 ;;
        --health-port) HEALTH_PORT="$2"; shift 2 ;;
        --data-dir)   DATA_DIR="$2"; shift 2 ;;
        --dry-run)    DRY_RUN=true; shift ;;
        -h|--help)    usage ;;
        *)            echo "Unknown argument: $1"; usage ;;
    esac
done

if [[ -z "$HOST" || -z "$SSH_KEY" || -z "$NODE_INDEX" ]]; then
    echo "ERROR: --host, --ssh-key, and --node-index are required."
    usage
fi

SSH_CMD="ssh -i $SSH_KEY -o StrictHostKeyChecking=accept-new"
SCP_CMD="scp -i $SSH_KEY -o StrictHostKeyChecking=accept-new"

run() {
    echo ">>> $*"
    if [[ "$DRY_RUN" == "false" ]]; then
        "$@"
    fi
}

remote() {
    run $SSH_CMD "$HOST" "$@"
}

# Extract IP from HOST (strip user@ prefix)
REMOTE_IP="${HOST#*@}"

echo "============================================="
echo " Gratia Bootstrap Node Deployment"
echo "============================================="
echo " Host:        $HOST"
echo " SSH key:     $SSH_KEY"
echo " Node index:  $NODE_INDEX"
echo " Port:        $PORT (QUIC), $((PORT+1)) (TCP)"
echo " Health port: $HEALTH_PORT"
echo " Data dir:    $DATA_DIR"
echo " Peers:       ${#PEERS[@]}"
echo "============================================="
echo ""

# ─── Step 1: Server setup ───────────────────────────────────────────
echo "=== Step 1: Server setup ==="
remote bash -c "'
    apt-get update -qq && apt-get install -y -qq build-essential pkg-config libssl-dev clang > /dev/null 2>&1
    mkdir -p $DATA_DIR
    mkdir -p $REMOTE_SRC
'"

# ─── Step 2: Install Rust if missing ────────────────────────────────
echo "=== Step 2: Ensure Rust is installed ==="
remote bash -c "'
    if ! command -v cargo &>/dev/null; then
        curl --proto \"=https\" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    fi
    source \$HOME/.cargo/env
    rustup update stable --no-self-update 2>/dev/null || true
    rustc --version
'"

# ─── Step 3: Copy source files ──────────────────────────────────────
echo "=== Step 3: Copying source to remote ==="

# Determine project root (script lives in scripts/)
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Copy workspace Cargo files and the crates needed for gratia-bootstrap
run $SCP_CMD -r \
    "$PROJECT_ROOT/Cargo.toml" \
    "$PROJECT_ROOT/Cargo.lock" \
    "$HOST:$REMOTE_SRC/"

run $SCP_CMD -r \
    "$PROJECT_ROOT/crates" \
    "$HOST:$REMOTE_SRC/"

# ─── Step 4: Build on remote ────────────────────────────────────────
echo "=== Step 4: Building gratia-bootstrap on remote ==="
remote bash -c "'
    source \$HOME/.cargo/env
    cd $REMOTE_SRC
    cargo build --release -p gratia-bootstrap 2>&1 | tail -5
    cp target/release/gratia-bootstrap $DATA_DIR/target/release/gratia-bootstrap 2>/dev/null || \
        (mkdir -p $DATA_DIR/target/release && cp target/release/gratia-bootstrap $DATA_DIR/target/release/)
    echo \"Binary size: \$(du -h $DATA_DIR/target/release/gratia-bootstrap | cut -f1)\"
'"

# ─── Step 5: Create systemd service ─────────────────────────────────
echo "=== Step 5: Installing systemd service ==="

# Build --peer flags
PEER_FLAGS=""
for p in "${PEERS[@]}"; do
    PEER_FLAGS+="  --peer $p \\\\\\n"
done

# Generate the service file
SERVICE_CONTENT="[Unit]
Description=Gratia Bootstrap Node #${NODE_INDEX}
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=root
ExecStart=${DATA_DIR}/target/release/gratia-bootstrap \\
  --data-dir ${DATA_DIR} \\
  --node-index ${NODE_INDEX} \\
  --port ${PORT} \\
  --health-port ${HEALTH_PORT}"

# Append peer flags
for p in "${PEERS[@]}"; do
    SERVICE_CONTENT+=" \\
  --peer ${p}"
done

SERVICE_CONTENT+="
Restart=always
RestartSec=5
Environment=RUST_LOG=info,gratia_network=debug
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target"

remote bash -c "'
    cat > /etc/systemd/system/gratia-bootstrap.service << \"SERVICEEOF\"
${SERVICE_CONTENT}
SERVICEEOF
    systemctl daemon-reload
    systemctl enable gratia-bootstrap
'"

# ─── Step 6: Configure firewall ─────────────────────────────────────
echo "=== Step 6: Configuring firewall ==="
remote bash -c "'
    ufw allow 22/tcp   >/dev/null 2>&1 || true
    ufw allow ${PORT}/udp  >/dev/null 2>&1 || true
    ufw allow $((PORT+1))/tcp >/dev/null 2>&1 || true
    ufw allow ${HEALTH_PORT}/tcp >/dev/null 2>&1 || true
    echo y | ufw enable 2>/dev/null || true
    ufw status
'"

# ─── Step 7: Start the service ──────────────────────────────────────
echo "=== Step 7: Starting service ==="
remote bash -c "'
    systemctl stop gratia-bootstrap 2>/dev/null || true
    systemctl start gratia-bootstrap
    sleep 2
    systemctl status gratia-bootstrap --no-pager
'"

# ─── Step 8: Verify health endpoint ─────────────────────────────────
echo "=== Step 8: Verifying health endpoint ==="
echo "Waiting 5 seconds for node to start..."
sleep 5

HEALTH_URL="http://${REMOTE_IP}:${HEALTH_PORT}"
echo "Checking $HEALTH_URL ..."

if HEALTH_RESPONSE=$(curl -sf --connect-timeout 10 "$HEALTH_URL" 2>&1); then
    echo "Health check passed: $HEALTH_RESPONSE"
else
    echo "WARNING: Health check failed. The node may still be starting."
    echo "Try manually: curl $HEALTH_URL"
    echo "Check logs:   ssh -i $SSH_KEY $HOST 'journalctl -u gratia-bootstrap -n 50'"
fi

# ─── Step 9: Get PeerId ─────────────────────────────────────────────
echo ""
echo "=== Step 9: Retrieve PeerId ==="
remote bash -c "'
    journalctl -u gratia-bootstrap -n 50 --no-pager 2>/dev/null | grep -i \"peer\|listen\|identity\" || echo \"(check logs manually for PeerId)\"
'"

echo ""
echo "============================================="
echo " Deployment complete!"
echo "============================================="
echo ""
echo "NEXT STEPS:"
echo "  1. Get the PeerId from the logs above (12D3KooW...)"
echo "  2. Update crates/gratia-ffi/src/lib.rs — uncomment the second BootstrapNode"
echo "     and fill in ip=\"${REMOTE_IP}\" and peer_id=\"<PeerId from logs>\""
echo "  3. Update Miami bootstrap to peer back:"
echo "     ssh -i ~/.ssh/gratia_bootstrap root@45.77.95.111"
echo "     Edit /etc/systemd/system/gratia-bootstrap.service — add:"
echo "       --peer /ip4/${REMOTE_IP}/udp/${PORT}/quic-v1/p2p/<NEW_PEER_ID>"
echo "       --peer /ip4/${REMOTE_IP}/tcp/$((PORT+1))/p2p/<NEW_PEER_ID>"
echo "     systemctl daemon-reload && systemctl restart gratia-bootstrap"
echo "  4. Rebuild Android app: ./scripts/build-android.sh"
echo ""
