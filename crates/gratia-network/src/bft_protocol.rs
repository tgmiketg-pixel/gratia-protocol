//! Direct peer-to-peer BFT signature delivery protocol.
//!
//! WHY: BFT signatures are consensus-critical messages that only need to reach
//! the block producer, not the entire network. Using gossipsub for signatures
//! introduces 0-10 seconds of delivery latency (waiting for heartbeat cycles)
//! and leaks consensus voting information to all observers.
//!
//! This module implements direct point-to-point signature delivery:
//! - Block producer includes its PeerId when broadcasting a block
//! - Committee members send their co-signatures directly to the producer
//! - Producer responds with an acknowledgment
//! - Sub-second delivery vs 0-10s gossipsub round-trip
//!
//! This matches how production BFT systems work (Tendermint, HotStuff, PBFT):
//! votes/signatures are sent directly to the leader, not broadcast.

use std::io;

use async_trait::async_trait;
use futures::prelude::*;
use libp2p::request_response::{self, Codec, ProtocolSupport};
use serde::{Deserialize, Serialize};

use gratia_core::types::ValidatorSignature;

/// Protocol identifier for direct BFT signature delivery.
pub const BFT_SIG_PROTOCOL: &str = "/gratia/bft-sig/1";

/// Maximum message size: 4 KB.
/// WHY: A BFT signature message is ~200 bytes (32-byte block hash + 8-byte
/// height + 32-byte validator ID + 64-byte Ed25519 sig + overhead). 4 KB
/// provides ample headroom while preventing abuse.
/// Maximum message size: 300 KB.
/// WHY: A block proposal can be up to 256 KB (max block size) + signature
/// overhead. 300 KB provides headroom. Signature-only messages are <1 KB.
const MAX_MESSAGE_SIZE: usize = 300 * 1024;

// ============================================================================
// Message types
// ============================================================================

/// BFT request — used for both directions of BFT communication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BftSignatureRequest {
    /// Producer → Validator: "Here's my block, please co-sign it."
    /// WHY: Direct block proposal eliminates gossipsub latency from the BFT
    /// critical path. The validator receives the block in <100ms, validates,
    /// and responds with their co-signature. This is how Tendermint/HotStuff
    /// work — the leader sends proposals directly to validators.
    BlockProposal {
        /// Serialized block header (compact, ~200 bytes).
        block_header_bytes: Vec<u8>,
        /// SHA-256 hash of the block header.
        block_hash: [u8; 32],
        /// Height of the proposed block.
        height: u64,
        /// The producer's own signature on the block.
        producer_signature: ValidatorSignature,
    },
    /// Validator → Producer: "Here's my co-signature for your block."
    CoSignature {
        /// SHA-256 hash of the block being signed.
        block_hash: [u8; 32],
        /// Height of the block being signed.
        height: u64,
        /// The validator's signature (NodeId + Ed25519 signature).
        signature: ValidatorSignature,
    },
}

/// BFT response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BftSignatureResponse {
    /// Signature/proposal accepted.
    Accepted,
    /// Block proposal accepted — here's my co-signature.
    /// WHY: The validator co-signs the block in the same round-trip as the
    /// proposal. The producer gets the signature in the response, not in a
    /// separate message. Single round-trip BFT.
    CoSigned {
        signature: ValidatorSignature,
    },
    /// Signature accepted and block has reached finality.
    Finalized,
    /// Rejected (wrong block, not a committee member, etc.)
    Rejected(String),
}

// ============================================================================
// Codec
// ============================================================================

/// Codec for BFT signature request-response messages.
/// Uses bincode with length-prefix framing (same pattern as SyncCodec).
#[derive(Debug, Clone, Default)]
pub struct BftSigCodec;

#[async_trait]
impl Codec for BftSigCodec {
    type Protocol = String;
    type Request = BftSignatureRequest;
    type Response = BftSignatureResponse;

    async fn read_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_length_prefixed::<BftSignatureRequest, T>(io).await
    }

    async fn read_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_length_prefixed::<BftSignatureResponse, T>(io).await
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
// Wire format helpers (same pattern as sync_protocol.rs)
// ============================================================================

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
            format!("BFT sig message too large: {} bytes (max {})", len, MAX_MESSAGE_SIZE),
        ));
    }

    let mut buf = vec![0u8; len];
    io.read_exact(&mut buf).await?;

    bincode::deserialize(&buf)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

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

/// Create the request-response behaviour for direct BFT signature delivery.
pub fn bft_sig_behaviour() -> request_response::Behaviour<BftSigCodec> {
    request_response::Behaviour::new(
        [(BFT_SIG_PROTOCOL.to_string(), ProtocolSupport::Full)],
        request_response::Config::default(),
    )
}
