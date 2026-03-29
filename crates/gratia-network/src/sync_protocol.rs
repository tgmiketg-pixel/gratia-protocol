//! libp2p request-response protocol for block synchronization.
//!
//! WHY: The original sync implementation wraps messages in gossipsub, meaning
//! every node receives every sync message even when it's addressed to a specific
//! peer. On a mobile network with 100+ nodes, this wastes significant bandwidth.
//!
//! This module implements a proper point-to-point request-response protocol:
//! - Sync requests go directly to the target peer (no broadcast)
//! - Responses come back on the same stream
//! - Bandwidth scales with O(peers syncing) not O(total nodes)
//!
//! The protocol is identified as `/gratia/sync/2` (v2 of the sync protocol).

use std::io;

use async_trait::async_trait;
use futures::prelude::*;
use libp2p::request_response::{self, Codec, ProtocolSupport};

use crate::sync::{SyncRequest, SyncResponse};

/// Protocol identifier for Gratia block sync v2.
/// WHY: Version 2 — replaces the gossipsub-wrapped sync messages (v1).
/// Peers that support v2 use request-response; peers that only support v1
/// fall back to gossipsub. This enables rolling upgrades.
pub const SYNC_PROTOCOL_V2: &str = "/gratia/sync/2";

/// Maximum message size: 16 MB.
/// WHY: 50 blocks * 256 KB each = ~12.5 MB max for a full sync response.
/// 16 MB provides headroom for serialization overhead while still preventing
/// OOM from malicious peers.
const MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

// ============================================================================
// Codec
// ============================================================================

/// Codec for serializing/deserializing sync request-response messages.
///
/// Uses bincode for compact binary encoding — same format as the v1 gossipsub
/// messages, so the `SyncRequest`/`SyncResponse` types are reused unchanged.
/// Messages are length-prefixed (4-byte big-endian u32) for framing.
#[derive(Debug, Clone, Default)]
pub struct SyncCodec;

#[async_trait]
impl Codec for SyncCodec {
    type Protocol = String;
    type Request = SyncRequest;
    type Response = SyncResponse;

    async fn read_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_length_prefixed::<SyncRequest, T>(io).await
    }

    async fn read_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_length_prefixed::<SyncResponse, T>(io).await
    }

    async fn write_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
        req: Self::Request,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_length_prefixed(io, &req).await
    }

    async fn write_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
        resp: Self::Response,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_length_prefixed(io, &resp).await
    }
}

// ============================================================================
// Wire format helpers
// ============================================================================

/// Read a length-prefixed bincode message from the stream.
async fn read_length_prefixed<M, T>(io: &mut T) -> io::Result<M>
where
    M: serde::de::DeserializeOwned,
    T: AsyncRead + Unpin + Send,
{
    let mut len_buf = [0u8; 4];
    io.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;

    if len > MAX_MESSAGE_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("message too large: {} bytes (max {})", len, MAX_MESSAGE_SIZE),
        ));
    }

    let mut buf = vec![0u8; len];
    io.read_exact(&mut buf).await?;

    bincode::deserialize(&buf)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

/// Write a length-prefixed bincode message to the stream.
async fn write_length_prefixed<M, T>(io: &mut T, msg: &M) -> io::Result<()>
where
    M: serde::Serialize,
    T: AsyncWrite + Unpin + Send,
{
    let bytes = bincode::serialize(msg)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

    let len = (bytes.len() as u32).to_be_bytes();
    io.write_all(&len).await?;
    io.write_all(&bytes).await?;
    io.close().await?;

    Ok(())
}

// ============================================================================
// Behaviour factory
// ============================================================================

/// Create the request-response behaviour for block sync.
///
/// WHY: Returns a configured behaviour that can be composed with gossipsub,
/// Kademlia, and other behaviours in the swarm. Supports both inbound and
/// outbound sync requests on the `/gratia/sync/2` protocol.
pub fn sync_behaviour() -> request_response::Behaviour<SyncCodec> {
    request_response::Behaviour::new(
        [(SYNC_PROTOCOL_V2.to_string(), ProtocolSupport::Full)],
        request_response::Config::default(),
    )
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use gratia_core::types::BlockHash;

    #[test]
    fn test_sync_request_bincode_roundtrip() {
        let req = SyncRequest::GetBlocks {
            from_height: 1,
            to_height: 50,
        };
        let bytes = bincode::serialize(&req).unwrap();
        let decoded: SyncRequest = bincode::deserialize(&bytes).unwrap();
        match decoded {
            SyncRequest::GetBlocks { from_height, to_height } => {
                assert_eq!(from_height, 1);
                assert_eq!(to_height, 50);
            }
            _ => panic!("Expected GetBlocks"),
        }
    }

    #[test]
    fn test_sync_response_bincode_roundtrip() {
        let resp = SyncResponse::ChainTip {
            height: 42,
            hash: BlockHash([0xAB; 32]),
        };
        let bytes = bincode::serialize(&resp).unwrap();
        let decoded: SyncResponse = bincode::deserialize(&bytes).unwrap();
        match decoded {
            SyncResponse::ChainTip { height, hash } => {
                assert_eq!(height, 42);
                assert_eq!(hash, BlockHash([0xAB; 32]));
            }
            _ => panic!("Expected ChainTip"),
        }
    }

    #[test]
    fn test_protocol_version_string() {
        assert_eq!(SYNC_PROTOCOL_V2, "/gratia/sync/2");
    }

    #[test]
    fn test_sync_behaviour_creates() {
        // Should not panic
        let _behaviour = sync_behaviour();
    }
}
