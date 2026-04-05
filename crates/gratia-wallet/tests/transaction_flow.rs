//! Integration tests for the complete transaction flow.
//!
//! Tests the end-to-end flow: wallet creation, mining rewards,
//! sending GRAT between wallets, and balance verification.

use gratia_wallet::WalletManager;
use gratia_wallet::keystore::SoftwareKeystore;
use gratia_core::types::{TransactionPayload, Lux};
use gratia_core::emission::{EmissionSchedule, LUX_PER_GRAT};

/// Helper: create a funded wallet with the given balance.
fn create_funded_wallet(balance_grat: u64) -> WalletManager<SoftwareKeystore> {
    let mut wm = WalletManager::new_software();
    wm.create_wallet().expect("wallet creation");
    wm.sync_balance(balance_grat * LUX_PER_GRAT);
    wm
}

#[test]
fn test_send_and_receive_basic() {
    let mut alice = create_funded_wallet(1000);
    let mut bob = create_funded_wallet(0);

    let bob_address = bob.address().unwrap();
    let _alice_address = alice.address().unwrap();

    // Alice sends 100 GRAT to Bob
    let amount = 100 * LUX_PER_GRAT;
    let fee = 1000; // 0.001 GRAT
    let tx = alice.send_transfer(bob_address, amount, fee).unwrap();

    // Verify Alice's balance decreased
    assert_eq!(alice.balance(), 1000 * LUX_PER_GRAT - amount - fee);

    // Verify transaction details
    assert!(matches!(tx.payload, TransactionPayload::Transfer { .. }));
    if let TransactionPayload::Transfer { to, amount: tx_amount } = &tx.payload {
        assert_eq!(*to, bob_address);
        assert_eq!(*tx_amount, amount);
    }

    // Simulate Bob receiving the transaction (crediting balance)
    if let TransactionPayload::Transfer { to, amount: tx_amount } = &tx.payload {
        if *to == bob_address {
            bob.sync_balance(bob.balance() + tx_amount);
        }
    }

    // Bob should now have 100 GRAT
    assert_eq!(bob.balance(), 100 * LUX_PER_GRAT);
}

#[test]
fn test_insufficient_balance_rejected() {
    let mut alice = create_funded_wallet(10);
    let bob = create_funded_wallet(0);
    let bob_address = bob.address().unwrap();

    // Try to send more than Alice has
    let result = alice.send_transfer(bob_address, 20 * LUX_PER_GRAT, 1000);
    assert!(result.is_err());

    // Balance unchanged
    assert_eq!(alice.balance(), 10 * LUX_PER_GRAT);
}

#[test]
fn test_mining_then_send() {
    let mut miner = create_funded_wallet(0);
    let recipient = create_funded_wallet(0);
    let recipient_address = recipient.address().unwrap();

    // Simulate mining: credit block rewards for 10 blocks at height 1-10
    let mut total_reward: Lux = 0;
    for height in 1..=10 {
        let reward = EmissionSchedule::per_miner_block_reward_lux(height, 1);
        total_reward += reward;
    }
    miner.sync_balance(total_reward);

    // Miner should have significant balance
    assert!(miner.balance() > 0);

    // Send half to recipient
    let send_amount = miner.balance() / 2;
    let _tx = miner.send_transfer(recipient_address, send_amount, 1000).unwrap();

    // Miner keeps roughly half minus fee
    let expected_remaining = total_reward - send_amount - 1000;
    assert_eq!(miner.balance(), expected_remaining);
}

#[test]
fn test_multiple_transactions_sequential() {
    let mut alice = create_funded_wallet(1000);
    let bob = create_funded_wallet(0);
    let bob_address = bob.address().unwrap();

    // Send 3 transactions
    for i in 1..=3 {
        let amount = 100 * LUX_PER_GRAT;
        let fee = 1000;
        let _tx = alice.send_transfer(bob_address, amount, fee).unwrap();
        let expected = (1000 - i * 100) as u64 * LUX_PER_GRAT - i as u64 * 1000;
        assert_eq!(alice.balance(), expected);
    }

    // Alice should have 700 GRAT minus 3 fees
    assert_eq!(alice.balance(), 700 * LUX_PER_GRAT - 3 * 1000);
}

#[test]
fn test_transaction_history_recorded() {
    let mut alice = create_funded_wallet(500);
    let bob = create_funded_wallet(0);
    let bob_address = bob.address().unwrap();

    assert!(alice.history().is_empty());

    alice.send_transfer(bob_address, 50 * LUX_PER_GRAT, 1000).unwrap();
    assert_eq!(alice.history().len(), 1);

    alice.send_transfer(bob_address, 25 * LUX_PER_GRAT, 1000).unwrap();
    assert_eq!(alice.history().len(), 2);
}

#[test]
fn test_emission_schedule_block_reward_decreases() {
    // Year 1 reward should be higher than year 2
    let year_1_reward = EmissionSchedule::block_reward_lux(1);
    let year_2_reward = EmissionSchedule::block_reward_lux(7_884_001); // First block of year 2

    assert!(year_1_reward > year_2_reward);
    // Year 2 should be ~75% of year 1
    let ratio = year_2_reward as f64 / year_1_reward as f64;
    assert!(ratio > 0.70 && ratio < 0.80, "Year 2/Year 1 ratio: {}", ratio);
}

#[test]
fn test_nfc_zero_fee_transaction() {
    let mut alice = create_funded_wallet(100);
    let bob = create_funded_wallet(0);
    let bob_address = bob.address().unwrap();

    // NFC transactions under 10 GRAT should be zero-fee per the spec
    let nfc_amount = 5 * LUX_PER_GRAT; // 5 GRAT — under threshold
    let nfc_fee = 0; // Zero fee for NFC under threshold

    let _tx = alice.send_transfer(bob_address, nfc_amount, nfc_fee).unwrap();

    // Alice should have exactly 95 GRAT (no fee deducted)
    assert_eq!(alice.balance(), 95 * LUX_PER_GRAT);
}
