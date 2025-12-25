// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use prost::Message;
use sha3::{Digest, Keccak256};
use tracing::warn;

// Include generated protobuf code
#[allow(dead_code)]
mod proto {
    include!(concat!(env!("OUT_DIR"), "/transaction.rs"));
}

use proto::{AccessTuple, Transaction, Transactions};

/// Calculate official transaction hash using Keccak256(TransactionHashData)
/// This matches the Go implementation exactly
/// 
/// Transaction data can be:
/// - A protobuf `Transactions` message (containing multiple Transaction)
/// - A single protobuf `Transaction` message
/// - Raw bytes (fallback to Keccak256 of raw data for non-protobuf data)
pub fn calculate_transaction_hash(tx_data: &[u8]) -> Vec<u8> {
    // Try to parse as Transactions (multiple transactions)
    if let Ok(transactions) = Transactions::decode(tx_data) {
        if !transactions.transactions.is_empty() {
            // Calculate hash for the first transaction (most common case)
            // If there are multiple transactions, we hash the first one
            return calculate_single_transaction_hash(&transactions.transactions[0]);
        }
    }
    
    // Try to parse as single Transaction
    if let Ok(tx) = Transaction::decode(tx_data) {
        return calculate_single_transaction_hash(&tx);
    }
    
    // If parsing fails, this might be non-protobuf data
    // Log a warning but still return a hash (using Keccak256 of raw data as fallback)
    warn!("Failed to parse transaction as protobuf, using raw data hash");
    let hash = Keccak256::digest(tx_data);
    hash.to_vec()
}

/// Calculate hash for a single Transaction using TransactionHashData
/// This is the official hash calculation that matches Go implementation
fn calculate_single_transaction_hash(tx: &Transaction) -> Vec<u8> {
    // Create TransactionHashData from Transaction
    let hash_data = proto::TransactionHashData {
        from_address: tx.from_address.clone(),
        to_address: tx.to_address.clone(),
        amount: tx.amount.clone(),
        max_gas: tx.max_gas,
        max_gas_price: tx.max_gas_price,
        max_time_use: tx.max_time_use,
        data: tx.data.clone(),
        r#type: tx.r#type,
        last_device_key: tx.last_device_key.clone(),
        new_device_key: tx.new_device_key.clone(),
        nonce: tx.nonce.clone(),
        chain_id: tx.chain_id,
        r: tx.r.clone(),
        s: tx.s.clone(),
        v: tx.v.clone(),
        gas_tip_cap: tx.gas_tip_cap.clone(),
        gas_fee_cap: tx.gas_fee_cap.clone(),
        access_list: tx
            .access_list
            .iter()
            .map(|at| AccessTuple {
                address: at.address.clone(),
                storage_keys: at.storage_keys.clone(),
            })
            .collect(),
    };

    // Encode TransactionHashData to protobuf bytes
    let mut buf = Vec::new();
    if let Err(e) = hash_data.encode(&mut buf) {
        warn!("Failed to encode TransactionHashData: {}", e);
        // Fallback: hash the raw transaction data
        let hash = Keccak256::digest(&tx.data);
        return hash.to_vec();
    }

    // Calculate Keccak256 hash of encoded TransactionHashData
    let hash = Keccak256::digest(&buf);
    hash.to_vec()
}

/// Calculate transaction hash and return hex string (first 8 bytes)
pub fn calculate_transaction_hash_hex(tx_data: &[u8]) -> String {
    let hash = calculate_transaction_hash(tx_data);
    hex::encode(&hash[..8.min(hash.len())])
}

/// Verify that transaction data is valid protobuf (Transaction or Transactions)
/// Returns true if data can be parsed as protobuf, false otherwise
/// This is used to ensure data integrity from Go sub node
pub fn verify_transaction_protobuf(tx_data: &[u8]) -> bool {
    // Try to parse as Transactions (multiple transactions)
    if Transactions::decode(tx_data).is_ok() {
        return true;
    }
    
    // Try to parse as single Transaction
    if Transaction::decode(tx_data).is_ok() {
        return true;
    }
    
    // Not valid protobuf
    false
}

