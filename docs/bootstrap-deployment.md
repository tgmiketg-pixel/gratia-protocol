# Bootstrap Node Deployment Guide

## Overview

Bootstrap nodes are headless relay servers that help phones discover each other across the internet. They participate in Kademlia DHT and gossipsub message relay but do NOT participate in consensus, mining, or block production.

Multiple bootstrap nodes in different geographic regions eliminate single points of failure. Phones connect to ALL configured bootstrap nodes and consider themselves "bootstrapped" if ANY one responds.

## Current Bootstrap Nodes

| # | Region | Provider | IP | Status |
|---|--------|----------|----|--------|
| 1 | Miami (US East) | Vultr | 45.77.95.111 | Active |
| 2 | TBD (Europe/Asia) | TBD | TBD | Not provisioned |

## Provisioning a New Bootstrap Node

### 1. Create the VPS

- **Provider:** Vultr (or any VPS with a static IPv4)
- **Plan:** $5-6/mo (1 vCPU, 1 GB RAM, 25 GB SSD) is sufficient
- **OS:** Ubuntu 22.04 or 24.04
- **Region:** Choose for geographic diversity (Frankfurt, Singapore, Tokyo, etc.)

### 2. Initial Server Setup

```bash
# SSH in
ssh root@NEW_SERVER_IP

# Update and install basics
apt update && apt upgrade -y
apt install -y build-essential pkg-config libssl-dev

# Create data directory
mkdir -p /opt/gratia-bootstrap

# Firewall
ufw allow 22/tcp       # SSH
ufw allow 9000/udp     # QUIC (libp2p)
ufw allow 9001/tcp     # TCP fallback (libp2p)
ufw allow 8080/tcp     # Health check endpoint
ufw enable
```

### 3. Deploy the Binary

Cross-compile on your dev machine for Linux x86_64:

```bash
# From the gratia project root
rustup target add x86_64-unknown-linux-gnu
cargo build --release --target x86_64-unknown-linux-gnu -p gratia-bootstrap
```

Or compile directly on the server:

```bash
# Install Rust on the server
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env

# Clone and build
git clone <repo-url> /opt/gratia-src
cd /opt/gratia-src
cargo build --release -p gratia-bootstrap
cp target/release/gratia-bootstrap /usr/local/bin/
```

### 4. Run with Systemd

Create `/etc/systemd/system/gratia-bootstrap.service`:

```ini
[Unit]
Description=Gratia Bootstrap Node
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=root
ExecStart=/usr/local/bin/gratia-bootstrap \
  --data-dir /opt/gratia-bootstrap \
  --node-index 2 \
  --port 9000 \
  --health-port 8080 \
  --peer /ip4/45.77.95.111/udp/9000/quic-v1/p2p/12D3KooWRUqRqDGpQwLtxMP6iGfKEjZYWnkgkiW5BLPyxAeB8gLF \
  --peer /ip4/45.77.95.111/tcp/9001/p2p/12D3KooWRUqRqDGpQwLtxMP6iGfKEjZYWnkgkiW5BLPyxAeB8gLF
Restart=always
RestartSec=5
Environment=RUST_LOG=info,gratia_network=debug

[Install]
WantedBy=multi-user.target
```

```bash
systemctl daemon-reload
systemctl enable gratia-bootstrap
systemctl start gratia-bootstrap
```

### 5. Get the PeerId

The PeerId is generated on first run and persisted in `--data-dir`. Read it from the logs:

```bash
journalctl -u gratia-bootstrap -n 50 | grep -i "peer\|listen\|identity"
```

The PeerId will look like: `12D3KooW...` (Base58-encoded). This is the value phones need in order to connect.

You can also check the persisted keypair exists:

```bash
ls -la /opt/gratia-bootstrap/libp2p_keypair.bin
```

### 6. Verify It's Working

```bash
# Health check
curl http://localhost:8080
# Expected: {"status":"ok","peers":0,"blocks_relayed":0,"txs_relayed":0,"version":"0.1.0"}

# Check it's listening
ss -ulnp | grep 9000   # QUIC
ss -tlnp | grep 9001   # TCP

# Check logs for peer connections
journalctl -u gratia-bootstrap -f
```

### 7. Update the App Code

Once the new node is running and you have its PeerId, update `crates/gratia-ffi/src/lib.rs`:

Uncomment and fill in the second `BootstrapNode` entry in the `bootstrap_nodes` vec:

```rust
BootstrapNode {
    ip: "NEW_SERVER_IP",
    peer_id: "12D3KooW...",  // from step 5
},
```

Then rebuild the Android app (`build-android.sh`).

### 8. Update Miami Node to Peer Back

On the Miami server (45.77.95.111), update the systemd service to add `--peer` flags pointing to the new node:

```bash
# Edit /etc/systemd/system/gratia-bootstrap.service
# Add to ExecStart:
#   --peer /ip4/NEW_SERVER_IP/udp/9000/quic-v1/p2p/NEW_PEER_ID
#   --peer /ip4/NEW_SERVER_IP/tcp/9001/p2p/NEW_PEER_ID

systemctl daemon-reload
systemctl restart gratia-bootstrap
```

## CLI Reference

| Flag | Default | Description |
|------|---------|-------------|
| `--port` | 9000 | QUIC listen port (TCP = port+1) |
| `--health-port` | 8080 | Health check HTTP port |
| `--data-dir` | /opt/gratia-bootstrap | Persistent data directory (keypair, peer cache) |
| `--node-index` | 1 | Unique node index (affects internal NodeId) |
| `--peer` | (none) | Other bootstrap node multiaddr (repeatable) |

## Troubleshooting

**Phones can't connect via QUIC:**
- Check `ufw status` -- port 9000/udp must be open
- Some cloud providers have separate firewall rules (Vultr: Firewall tab in dashboard)

**Phones can't connect via TCP:**
- Check port 9001/tcp is open in both ufw and cloud firewall

**PeerId changed after redeployment:**
- The keypair is in `--data-dir/libp2p_keypair.bin`. If this file is lost, a new PeerId is generated and ALL phones need an app update. Back up this file.

**Bootstrap nodes can't peer with each other:**
- Verify `--peer` multiaddrs include the correct PeerId
- Check both nodes' firewalls allow inbound from each other
