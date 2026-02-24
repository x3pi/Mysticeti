// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use crate::node::tx_submitter::TransactionSubmitter;
use crate::node::ConsensusNode;
use anyhow::Result;
use consensus_core::SystemTransaction;
use std::io::{Read, Write};
use std::path::Path;
use tokio::sync::Mutex;
use tracing::{error, info, trace, warn}; // Added TransactionSubmitter trait

pub async fn queue_transaction(
    pending_queue: &Mutex<Vec<Vec<u8>>>,
    storage_path: &Path,
    tx_data: Vec<u8>,
) -> Result<()> {
    queue_transactions(pending_queue, storage_path, vec![tx_data]).await
}

pub async fn queue_transactions(
    pending_queue: &Mutex<Vec<Vec<u8>>>,
    storage_path: &Path,
    tx_data_list: Vec<Vec<u8>>,
) -> Result<()> {
    if tx_data_list.is_empty() {
        return Ok(());
    }

    let mut queue = pending_queue.lock().await;
    let added_len = tx_data_list.len();
    queue.extend(tx_data_list);
    info!(
        "📦 [TX FLOW] Queued {} transactions: total_size={}",
        added_len,
        queue.len()
    );

    if let Err(e) = persist_transaction_queue(&queue, storage_path).await {
        warn!("⚠️ [TX PERSISTENCE] Failed to persist queue: {}", e);
    }
    Ok(())
}

pub async fn persist_transaction_queue(queue: &[Vec<u8>], storage_path: &Path) -> Result<()> {
    let queue_path = storage_path.join("transaction_queue.bin");
    let mut file = std::fs::File::create(&queue_path)?;

    let count = queue.len() as u32;
    file.write_all(&count.to_le_bytes())?;

    for tx_data in queue {
        let len = tx_data.len() as u32;
        file.write_all(&len.to_le_bytes())?;
        file.write_all(tx_data)?;
    }
    file.flush()?;
    trace!(
        "💾 Persisted {} txs to {}",
        queue.len(),
        queue_path.display()
    );
    Ok(())
}

pub async fn load_transaction_queue_static(storage_path: &Path) -> Result<Vec<Vec<u8>>> {
    let queue_path = storage_path.join("transaction_queue.bin");
    if !queue_path.exists() {
        return Ok(Vec::new());
    }

    let mut file = std::fs::File::open(&queue_path)?;
    let mut queue = Vec::new();
    let mut count_buf = [0u8; 4];
    file.read_exact(&mut count_buf)?;
    let count = u32::from_le_bytes(count_buf);

    for _ in 0..count {
        let mut len_buf = [0u8; 4];
        file.read_exact(&mut len_buf)?;
        let len = u32::from_le_bytes(len_buf) as usize;

        let mut tx_data = vec![0u8; len];
        file.read_exact(&mut tx_data)?;
        queue.push(tx_data);
    }
    Ok(queue)
}

pub async fn submit_queued_transactions(node: &mut ConsensusNode) -> Result<usize> {
    let mut queue = node.pending_transactions_queue.lock().await;
    let original_count = queue.len();
    if original_count == 0 {
        return Ok(0);
    }

    info!(
        "📤 [TX FLOW] Submitting {} queued transactions",
        original_count
    );

    // Load committed transaction hashes to avoid resubmitting already committed transactions
    let committed_hashes = {
        let hashes_guard = node.committed_transaction_hashes.lock().await;
        hashes_guard.clone()
    };
    info!(
        "📋 [TX FLOW] Loaded {} committed transaction hashes from current epoch",
        committed_hashes.len()
    );

    // Filter out transactions that were already committed
    let mut filtered_transactions = Vec::new();
    let mut skipped_duplicates = 0;

    for tx_data in &*queue {
        let tx_hash = crate::types::tx_hash::calculate_transaction_hash(tx_data);
        if committed_hashes.contains(&tx_hash) {
            skipped_duplicates += 1;
            let hash_hex = hex::encode(&tx_hash);
            info!(
                "⏭️ [TX FLOW] Skipping already committed transaction: {}",
                hash_hex
            );
        } else {
            filtered_transactions.push(tx_data.clone());
        }
    }

    // Dedup among remaining transactions
    let mut transactions_with_hash: Vec<(Vec<u8>, Vec<u8>)> = filtered_transactions
        .into_iter()
        .map(|tx| {
            (
                tx.clone(),
                crate::types::tx_hash::calculate_transaction_hash(&tx),
            )
        })
        .collect();

    transactions_with_hash.sort_by(|(_, a), (_, b)| a.cmp(b));
    transactions_with_hash.dedup_by(|a, b| a.1 == b.1);

    let transactions: Vec<Vec<u8>> = transactions_with_hash
        .into_iter()
        .map(|(tx, _)| tx)
        .collect();
    queue.clear();
    drop(queue); // Release lock

    info!(
        "🔄 [TX FLOW] Filtered {} duplicates, submitting {} unique transactions",
        skipped_duplicates,
        transactions.len()
    );

    let mut successful_count = 0;
    let mut requeued_count = 0;

    for tx_data in transactions {
        if SystemTransaction::from_bytes(&tx_data).is_ok()
            || !crate::types::tx_hash::verify_transaction_protobuf(&tx_data)
        {
            continue;
        }

        if node.transaction_client_proxy.is_none() {
            let mut q = node.pending_transactions_queue.lock().await;
            q.push(tx_data);
            requeued_count += 1;
            continue;
        }

        let mut retry = 0;
        let max_retries = 20;
        let mut submitted = false;

        while retry < max_retries {
            let proxy = match node.transaction_client_proxy.as_ref() {
                Some(p) => p,
                None => {
                    // Should not happen given the check at line 122, but guard anyway
                    let mut q = node.pending_transactions_queue.lock().await;
                    q.push(tx_data.clone());
                    requeued_count += 1;
                    break;
                }
            };
            match proxy.submit(vec![tx_data.clone()]).await {
                Ok(_) => {
                    successful_count += 1;
                    submitted = true;

                    // NOTE: Hash tracking moved to commit processor
                    // Queue submissions are just temporary - only commit processing truly commits transactions

                    break;
                }
                Err(e) => {
                    retry += 1;
                    let delay = 200 * (1 << (retry - 1));
                    warn!(
                        "⚠️ Failed to submit (attempt {}): {}. Retry in {}ms",
                        retry, e, delay
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                }
            }
        }

        if !submitted {
            error!("❌ Failed to submit tx after retries. Re-queuing.");
            let mut q = node.pending_transactions_queue.lock().await;
            q.push(tx_data);
            requeued_count += 1;
        }
    }

    if successful_count > 0 {
        let queue_path = node.storage_path.join("transaction_queue.bin");
        if queue_path.exists() {
            let _ = std::fs::remove_file(&queue_path);
        }
    } else if requeued_count > 0 {
        // Re-persist if failed
        let q = node.pending_transactions_queue.lock().await;
        let _ = persist_transaction_queue(&q, &node.storage_path).await;
    }

    Ok(successful_count)
}
