#![no_main]
//! Fuzz target: Transaction deserialization and signature verification.
//!
//! Feeds random bytes as a Transaction, then runs verify_transaction()
//! on any successfully deserialized transaction. Must not panic on
//! malformed transactions — should return Err gracefully.

use libfuzzer_sys::fuzz_target;

use gratia_core::types::Transaction;
use gratia_wallet::transactions::verify_transaction;

fuzz_target!(|data: &[u8]| {
    // Try to deserialize as a Transaction
    if let Ok(tx) = bincode::deserialize::<Transaction>(data) {
        // If deserialization succeeds, attempt signature verification.
        // This MUST NOT panic — invalid signatures, malformed pubkeys,
        // wrong hash values should all return Err.
        let _ = verify_transaction(&tx);
    }
});
