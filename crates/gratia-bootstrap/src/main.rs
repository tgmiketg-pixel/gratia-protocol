//! Gratia Bootstrap Node
//!
//! A headless relay node that helps phones discover each other on the internet.
//! It does NOT participate in consensus, mining, or block production — it only
//! relays gossipsub messages and participates in Kademlia DHT peer discovery.
//!
//! Usage:
//!   gratia-bootstrap [--port PORT] [--health-port PORT]
//!
//! Default: listens on UDP 9000 (QUIC) for libp2p, HTTP 8080 for health checks.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

use gratia_core::types::{BlockHash, NodeId};
use gratia_network::{NetworkConfig, NetworkEvent, NetworkManager, NoBlockProvider};

// WHY: Fixed node ID for the bootstrap server. This allows phones to hardcode
// the bootstrap peer address including the peer ID. Changing this would require
// updating all phones with a new app build.
fn bootstrap_node_id() -> NodeId {
    let mut id = [0u8; 32];
    id[0] = 0xB0; // "B0" for Bootstrap
    id[1] = 0x07;
    id[31] = 0x01;
    NodeId(id)
}

#[tokio::main]
async fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,gratia_network=debug".parse().unwrap()),
        )
        .init();

    let port: u16 = std::env::args()
        .skip_while(|a| a != "--port")
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(9000);

    let health_port: u16 = std::env::args()
        .skip_while(|a| a != "--health-port")
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);

    tracing::info!("=== Gratia Bootstrap Node ===");
    tracing::info!("QUIC listen port: {}", port);
    tracing::info!("TCP listen port: {}", port + 1);
    tracing::info!("Health check port: {}", health_port);

    let node_id = bootstrap_node_id();

    let mut config = NetworkConfig::new(node_id);
    // WHY: Persist the bootstrap's libp2p identity so the PeerId survives
    // server restarts. Without this, every restart generates a new PeerId,
    // and phones with the old PeerId hardcoded can't connect — the QUIC
    // handshake fails because libp2p rejects PeerId mismatches. This was
    // causing the A06 to fail every bootstrap connection attempt.
    config.data_dir = Some("/opt/gratia-bootstrap".to_string());
    // WHY: Listen on both QUIC (UDP) and TCP. Some phones (Samsung A06 without
    // SIM card) can't do UDP/QUIC to external IPs. TCP works everywhere.
    // UFW firewall rules needed: sudo ufw allow 9000/udp && sudo ufw allow 9001/tcp
    config.transport.listen_addresses = vec![
        format!("/ip4/0.0.0.0/udp/{}/quic-v1", port),
        format!("/ip4/0.0.0.0/tcp/{}", port + 1),
    ];
    // WHY: Bootstrap node doesn't need to connect to other bootstraps — it IS the bootstrap.
    config.bootstrap_peers = Vec::new();
    // WHY: Bootstrap can cache more peers than mobile nodes since it has more RAM.
    config.max_cached_peers = 5000;
    // WHY: Server can handle many more connections than a phone.
    config.transport.max_peers = 500;
    config.transport.max_inbound = 400;
    config.transport.max_outbound = 100;

    let mut network = NetworkManager::new(config);
    // WHY: Bootstrap node has no state to serve. NoBlockProvider returns empty
    // responses for GetBlocks requests. Phones sync blocks from each other.
    network.set_block_provider(Arc::new(NoBlockProvider));

    let mut event_rx = match network.start().await {
        Ok(rx) => rx,
        Err(e) => {
            tracing::error!("Failed to start network: {}", e);
            std::process::exit(1);
        }
    };

    tracing::info!("Network started. Listening for peers...");

    // Counters for health endpoint
    let peers_connected = Arc::new(AtomicU64::new(0));
    let blocks_relayed = Arc::new(AtomicU64::new(0));
    let txs_relayed = Arc::new(AtomicU64::new(0));

    // Spawn health check HTTP server
    let pc = peers_connected.clone();
    let br = blocks_relayed.clone();
    let tr = txs_relayed.clone();
    tokio::spawn(async move {
        run_health_server(health_port, pc, br, tr).await;
    });

    // Main event loop — just log events, the network layer handles relaying
    let mut peer_count: u64 = 0;
    loop {
        match event_rx.recv().await {
            Some(NetworkEvent::PeerConnected { peer_id, node_id, is_inbound: _ }) => {
                peer_count += 1;
                peers_connected.store(peer_count, Ordering::Relaxed);
                tracing::info!(
                    %peer_id,
                    node_id = ?node_id,
                    total_peers = peer_count,
                    "Peer connected"
                );
            }
            Some(NetworkEvent::PeerDisconnected { peer_id }) => {
                peer_count = peer_count.saturating_sub(1);
                peers_connected.store(peer_count, Ordering::Relaxed);
                tracing::info!(
                    %peer_id,
                    total_peers = peer_count,
                    "Peer disconnected"
                );
            }
            Some(NetworkEvent::BlockReceived(block, _source)) => {
                blocks_relayed.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(
                    height = block.header.height,
                    "Block relayed"
                );
            }
            Some(NetworkEvent::TransactionReceived(tx)) => {
                txs_relayed.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(
                    hash = hex::encode(tx.hash.0),
                    "Transaction relayed"
                );
            }
            Some(NetworkEvent::SyncStateChanged(state)) => {
                tracing::debug!(?state, "Sync state changed");
            }
            Some(NetworkEvent::SyncBlocksReceived(blocks)) => {
                tracing::debug!(count = blocks.len(), "Sync blocks received");
            }
            Some(NetworkEvent::AttestationReceived(_)) => {
                tracing::debug!("Attestation relayed");
            }
            Some(NetworkEvent::NodeAnnounced(ann)) => {
                tracing::info!(
                    node_id = %ann.node_id,
                    score = ann.presence_score,
                    "Node announced"
                );
            }
            Some(NetworkEvent::LuxPostReceived(post)) => {
                tracing::debug!(
                    hash = %post.hash,
                    author = %post.author,
                    "Lux post relayed"
                );
            }
            Some(NetworkEvent::ValidatorSignatureReceived(sig)) => {
                tracing::debug!(
                    height = sig.height,
                    validator = ?sig.signature.validator,
                    "Validator signature relayed"
                );
            }
            None => {
                tracing::warn!("Event channel closed — shutting down");
                break;
            }
        }
    }
}

/// Simple HTTP health check server.
/// GET / returns JSON with node status.
async fn run_health_server(
    port: u16,
    peers: Arc<AtomicU64>,
    blocks: Arc<AtomicU64>,
    txs: Arc<AtomicU64>,
) {
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("Failed to bind health server on {}: {}", addr, e);
            return;
        }
    };
    tracing::info!("Health check server listening on http://0.0.0.0:{}", port);

    loop {
        if let Ok((mut stream, _)) = listener.accept().await {
            let p = peers.load(Ordering::Relaxed);
            let b = blocks.load(Ordering::Relaxed);
            let t = txs.load(Ordering::Relaxed);

            let body = format!(
                r#"{{"status":"ok","peers":{},"blocks_relayed":{},"txs_relayed":{},"version":"0.1.0"}}"#,
                p, b, t
            );

            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\n\r\n{}",
                body.len(),
                body
            );

            let _ = stream.write_all(response.as_bytes()).await;
        }
    }
}
