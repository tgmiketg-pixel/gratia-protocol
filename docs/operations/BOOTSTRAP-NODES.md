# Bootstrap Node Operations

## What Bootstrap Nodes Do

Bootstrap nodes are headless relay servers that help phones discover each other across the internet. They participate in Kademlia DHT and gossipsub message relay. They do NOT participate in consensus, mining, or block production.

Phones connect to ALL configured bootstrap nodes on startup. If ANY one responds, the phone is connected to the network. Multiple bootstrap nodes in different regions provide geographic redundancy and lower latency for phones worldwide.

## Current Nodes

| # | Region | Provider | IP | Node Index | PeerId | Status |
|---|--------|----------|----|------------|--------|--------|
| 1 | Miami (US East) | Vultr $5/mo | 45.77.95.111 | 1 | 12D3KooWRUqRqDGpQwLtxMP6iGfKEjZYWnkgkiW5BLPyxAeB8gLF | Active |
| 2 | TBD | TBD | TBD | 2 | TBD | Not provisioned |

### Recommended Second Node Location

Pick one based on target user base:

| Region | Vultr Location | Why |
|--------|---------------|-----|
| Frankfurt | fra | Covers Europe, Middle East, Africa. Low latency to ~2B people. |
| Singapore | sgp | Covers Southeast Asia, India, Oceania. Huge smartphone market. |
| Tokyo | nrt | Covers East Asia. Slightly less geographic diversity than Singapore. |

**Recommendation:** Frankfurt first (covers the most underserved regions relative to Miami), then Singapore as a third node.

## Peering Configuration

Bootstrap nodes peer with each other so their Kademlia DHTs stay in sync. A phone that connects to any single bootstrap node will discover peers known to all bootstrap nodes.

Each node's systemd service includes `--peer` flags pointing to every other bootstrap node:

```
Node 1 (Miami):     --peer <Node2 QUIC multiaddr> --peer <Node2 TCP multiaddr>
Node 2 (Frankfurt): --peer <Node1 QUIC multiaddr> --peer <Node1 TCP multiaddr>
Node 3 (Singapore): --peer <Node1 QUIC> --peer <Node1 TCP> --peer <Node2 QUIC> --peer <Node2 TCP>
```

Multiaddr format:
- QUIC: `/ip4/<IP>/udp/9000/quic-v1/p2p/<PeerId>`
- TCP:  `/ip4/<IP>/tcp/9001/p2p/<PeerId>`

Both QUIC and TCP are listed because some phones (Samsung A06 without SIM) cannot use QUIC/UDP to external IPs.

## Systemd Service Configuration

Service file location: `/etc/systemd/system/gratia-bootstrap.service`

### Node 1 (Miami) - Current

```ini
[Unit]
Description=Gratia Bootstrap Node #1
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=root
ExecStart=/opt/gratia-bootstrap/target/release/gratia-bootstrap \
  --data-dir /opt/gratia-bootstrap \
  --node-index 1 \
  --port 9000 \
  --health-port 8080
Restart=always
RestartSec=5
Environment=RUST_LOG=info,gratia_network=debug
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
```

When a second node is provisioned, add `--peer` flags:

```ini
ExecStart=/opt/gratia-bootstrap/target/release/gratia-bootstrap \
  --data-dir /opt/gratia-bootstrap \
  --node-index 1 \
  --port 9000 \
  --health-port 8080 \
  --peer /ip4/<NODE2_IP>/udp/9000/quic-v1/p2p/<NODE2_PEERID> \
  --peer /ip4/<NODE2_IP>/tcp/9001/p2p/<NODE2_PEERID>
```

### Node 2 (Template)

```ini
[Unit]
Description=Gratia Bootstrap Node #2
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=root
ExecStart=/opt/gratia-bootstrap/target/release/gratia-bootstrap \
  --data-dir /opt/gratia-bootstrap \
  --node-index 2 \
  --port 9000 \
  --health-port 8080 \
  --peer /ip4/45.77.95.111/udp/9000/quic-v1/p2p/12D3KooWRUqRqDGpQwLtxMP6iGfKEjZYWnkgkiW5BLPyxAeB8gLF \
  --peer /ip4/45.77.95.111/tcp/9001/p2p/12D3KooWRUqRqDGpQwLtxMP6iGfKEjZYWnkgkiW5BLPyxAeB8gLF
Restart=always
RestartSec=5
Environment=RUST_LOG=info,gratia_network=debug
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
```

## Adding a New Bootstrap Node

### Quick Steps

1. Provision a VPS ($5/mo, Ubuntu 22.04+, 1 GB RAM minimum)
2. Run the deployment script:
   ```bash
   ./scripts/deploy-bootstrap.sh \
     --host root@NEW_IP \
     --ssh-key ~/.ssh/gratia_bootstrap \
     --node-index N \
     --peer "/ip4/45.77.95.111/udp/9000/quic-v1/p2p/12D3KooWRUqRqDGpQwLtxMP6iGfKEjZYWnkgkiW5BLPyxAeB8gLF" \
     --peer "/ip4/45.77.95.111/tcp/9001/p2p/12D3KooWRUqRqDGpQwLtxMP6iGfKEjZYWnkgkiW5BLPyxAeB8gLF"
   ```
3. Get the PeerId from the new node's logs:
   ```bash
   ssh -i ~/.ssh/gratia_bootstrap root@NEW_IP \
     'journalctl -u gratia-bootstrap -n 50 | grep -i peer'
   ```
4. Update every existing bootstrap node to peer back to the new one (add `--peer` flags, `systemctl daemon-reload && systemctl restart gratia-bootstrap`)
5. Update `crates/gratia-ffi/src/lib.rs` — add the new `BootstrapNode` entry with IP and PeerId
6. Rebuild Android app: `./scripts/build-android.sh`
7. Update this document with the new node's details

### Critical: Back Up the Identity Key

Each bootstrap node generates a persistent libp2p keypair on first run, stored at `<data-dir>/libp2p_identity.key`. If this file is lost, the PeerId changes and every phone needs an app update. Always back up this file:

```bash
scp -i ~/.ssh/gratia_bootstrap root@NODE_IP:/opt/gratia-bootstrap/libp2p_identity.key ./backups/bootstrap-N-identity.key
```

## Monitoring

### Health Endpoint

Each bootstrap node exposes an HTTP health endpoint on port 8080:

```bash
curl http://45.77.95.111:8080
# {"status":"ok","peers":3,"blocks_relayed":142,"txs_relayed":57,"version":"0.1.0"}
```

Fields:
- `status` — always "ok" if the node is running
- `peers` — number of currently connected peers (phones + other bootstrap nodes)
- `blocks_relayed` — total blocks relayed since last restart
- `txs_relayed` — total transactions relayed since last restart
- `version` — binary version

### Simple Monitoring Script

```bash
#!/usr/bin/env bash
# Check all bootstrap nodes. Run from cron every 5 minutes.
NODES=("45.77.95.111" "SECOND_NODE_IP")
for ip in "${NODES[@]}"; do
    if ! curl -sf --connect-timeout 5 "http://${ip}:8080" > /dev/null; then
        echo "ALERT: Bootstrap node ${ip} is DOWN"
        # Add notification here (email, Telegram, etc.)
    fi
done
```

### Log Inspection

```bash
# Live logs
ssh -i ~/.ssh/gratia_bootstrap root@45.77.95.111 'journalctl -u gratia-bootstrap -f'

# Last 100 lines
ssh -i ~/.ssh/gratia_bootstrap root@45.77.95.111 'journalctl -u gratia-bootstrap -n 100 --no-pager'

# Since last hour
ssh -i ~/.ssh/gratia_bootstrap root@45.77.95.111 'journalctl -u gratia-bootstrap --since "1 hour ago" --no-pager'
```

## CLI Reference

| Flag | Default | Description |
|------|---------|-------------|
| `--port` | 9000 | QUIC listen port (TCP = port+1) |
| `--health-port` | 8080 | Health check HTTP port |
| `--data-dir` | /opt/gratia-bootstrap | Persistent data directory (keypair, peer cache) |
| `--node-index` | 1 | Unique node index (1=Miami, 2=second, etc.) |
| `--peer` | (none) | Other bootstrap node multiaddr (repeatable) |

## Firewall Rules

Every bootstrap node needs these ports open:

| Port | Protocol | Purpose |
|------|----------|---------|
| 22 | TCP | SSH access |
| 9000 | UDP | libp2p QUIC transport |
| 9001 | TCP | libp2p TCP fallback |
| 8080 | TCP | Health check endpoint |

```bash
ufw allow 22/tcp
ufw allow 9000/udp
ufw allow 9001/tcp
ufw allow 8080/tcp
ufw enable
```
