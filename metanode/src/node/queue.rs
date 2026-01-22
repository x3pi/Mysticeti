// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use tracing::{info, warn, error, trace};
use tokio::sync::Mutex;
use std::path::Path;
use std::io::{Write, Read};
use consensus_core::SystemTransaction;
use crate::node::ConsensusNode;
use crate::tx_submitter::TransactionSubmitter; // Added TransactionSubmitter trait

pub async fn queue_transaction(
    pending_queue: &Mutex<Vec<Vec<u8>>>,
    storage_path: &Path,
    tx_data: Vec<u8>
) -> Result<()> {
    let mut queue = pending_queue.lock().await;
    queue.push(tx_data);
    info!("📦 [TX FLOW] Queued transaction: size={}", queue.len());

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
    trace!("💾 Persisted {} txs to {}", queue.len(), queue_path.display());
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
    
    info!("📤 [TX FLOW] Submitting {} queued transactions", original_count);
    
    // Dedup
    let mut transactions_with_hash: Vec<(Vec<u8>, Vec<u8>)> = queue.iter()
        .map(|tx| (tx.clone(), crate::tx_hash::calculate_transaction_hash(tx)))
        .collect();
    
    transactions_with_hash.sort_by(|(_, a), (_, b)| a.cmp(b));
    transactions_with_hash.dedup_by(|a, b| a.1 == b.1);
    
    let transactions: Vec<Vec<u8>> = transactions_with_hash.into_iter().map(|(tx, _)| tx).collect();
    queue.clear();
    drop(queue); // Release lock

    let mut successful_count = 0;
    let mut requeued_count = 0;
    
    for tx_data in transactions {
        if SystemTransaction::from_bytes(&tx_data).is_ok() || !crate::tx_hash::verify_transaction_protobuf(&tx_data) {
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
            match node.transaction_client_proxy.as_ref().unwrap().submit(vec![tx_data.clone()]).await {
                Ok(_) => {
                    successful_count += 1;
                    submitted = true;
                    break;
                },
                Err(e) => {
                    retry += 1;
                    let delay = 200 * (1 << (retry - 1));
                    warn!("⚠️ Failed to submit (attempt {}): {}. Retry in {}ms", retry, e, delay);
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