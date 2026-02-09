// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

//! Transaction recovery across epoch boundaries and committed hash persistence.

use crate::node::tx_submitter::TransactionSubmitter;
use crate::node::ConsensusNode;
use anyhow::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{error, info, trace, warn};

/// Recover transactions that were submitted in the previous epoch but not committed
pub(super) async fn recover_epoch_pending_transactions(node: &mut ConsensusNode) -> Result<usize> {
    let mut epoch_pending = node.epoch_pending_transactions.lock().await;
    if epoch_pending.is_empty() {
        return Ok(0);
    }

    info!(
        "🔄 [EPOCH RECOVERY] Checking {} transactions from previous epoch for recovery",
        epoch_pending.len()
    );

    // Load committed transaction hashes from previous epoch to avoid duplicates
    let committed_hashes =
        load_committed_transaction_hashes(&node.storage_path, node.current_epoch - 1).await;
    info!(
        "📋 [EPOCH RECOVERY] Loaded {} committed transaction hashes from epoch {}",
        committed_hashes.len(),
        node.current_epoch - 1
    );

    let mut transactions_to_recover = Vec::new();
    let mut skipped_duplicates = 0;

    // Filter out transactions that were already committed in the previous epoch
    for tx_data in epoch_pending.iter() {
        let tx_hash = crate::types::tx_hash::calculate_transaction_hash(tx_data);
        let hash_hex = hex::encode(&tx_hash);

        // Special debug logging for the problematic transaction
        if hash_hex.starts_with("44a535f2") {
            warn!(
                "🔍 [DEBUG] Found problematic transaction {} in recovery. Checking registry...",
                hash_hex
            );
            if committed_hashes.contains(&tx_hash) {
                warn!("🔍 [DEBUG] Transaction {} WAS found in committed registry - this should prevent duplicate!", hash_hex);
            } else {
                error!("🔍 [DEBUG] Transaction {} NOT found in registry - this explains why it was sent twice!", hash_hex);
            }
        }

        if committed_hashes.contains(&tx_hash) {
            info!("⏭️ [EPOCH RECOVERY] Skipping already committed transaction: {} (found in epoch {} registry)",
                  hash_hex, node.current_epoch - 1);
            skipped_duplicates += 1;
        } else {
            info!("🔄 [EPOCH RECOVERY] Will recover transaction: {} (not found in committed registry)",
                  hash_hex);
            transactions_to_recover.push(tx_data.clone());
        }
    }

    // Clear the pending list - we'll resubmit what needs recovery
    epoch_pending.clear();

    info!(
        "🔄 [EPOCH RECOVERY] Filtered duplicates: {} skipped, {} to recover",
        skipped_duplicates,
        transactions_to_recover.len()
    );

    if transactions_to_recover.is_empty() {
        info!("✅ [EPOCH RECOVERY] No transactions need recovery (all were already committed)");
        return Ok(0);
    }

    // Resubmit transactions to new epoch
    info!(
        "🚀 [EPOCH RECOVERY] Resubmitting {} transactions to new epoch",
        transactions_to_recover.len()
    );

    let mut recovered_count = 0;
    let mut failed_count = 0;
    let _total_count = transactions_to_recover.len();

    for tx_data in transactions_to_recover {
        if let Some(proxy) = &node.transaction_client_proxy {
            let tx_hash = crate::types::tx_hash::calculate_transaction_hash(&tx_data);
            let hash_hex = hex::encode(&tx_hash);

            match proxy.submit(vec![tx_data.clone()]).await {
                Ok(_) => {
                    recovered_count += 1;
                    info!(
                        "✅ [EPOCH RECOVERY] Successfully recovered transaction: {}",
                        hash_hex
                    );

                    // Track this transaction as successfully submitted in new epoch
                    if let Err(e) = save_committed_transaction_hash(
                        &node.storage_path,
                        node.current_epoch,
                        &tx_hash,
                    )
                    .await
                    {
                        warn!(
                            "⚠️ [EPOCH RECOVERY] Failed to save committed hash {}: {}",
                            hash_hex, e
                        );
                    }
                }
                Err(e) => {
                    failed_count += 1;
                    warn!(
                        "❌ [EPOCH RECOVERY] Failed to recover transaction {}: {}",
                        hash_hex, e
                    );
                    // Put back into pending queue for later retry
                    let mut pending = node.pending_transactions_queue.lock().await;
                    pending.push(tx_data);
                }
            }
        }
    }

    info!(
        "📊 [EPOCH RECOVERY] Results: {} recovered, {} failed, {} skipped (duplicates)",
        recovered_count, failed_count, skipped_duplicates
    );
    Ok(recovered_count)
}

/// Load committed transaction hashes from a specific epoch to avoid duplicate recovery
pub async fn load_committed_transaction_hashes(
    storage_path: &std::path::Path,
    epoch: u64,
) -> std::collections::HashSet<Vec<u8>> {
    let hashes_file = storage_path
        .join("epochs")
        .join(format!("epoch_{}", epoch))
        .join("committed_transaction_hashes.bin");

    if !hashes_file.exists() {
        trace!(
            "ℹ️ [TX HASH REGISTRY] No committed hashes file found for epoch {}",
            epoch
        );
        return std::collections::HashSet::new();
    }

    match load_transaction_hashes_from_file(&hashes_file).await {
        Ok(hashes) => {
            info!(
                "📋 [TX HASH REGISTRY] Loaded {} committed transaction hashes from epoch {}",
                hashes.len(),
                epoch
            );
            hashes
        }
        Err(e) => {
            warn!(
                "⚠️ [TX HASH REGISTRY] Failed to load committed hashes for epoch {}: {}",
                epoch, e
            );
            std::collections::HashSet::new()
        }
    }
}

/// Save a committed transaction hash to registry for duplicate prevention
pub async fn save_committed_transaction_hash(
    storage_path: &std::path::Path,
    epoch: u64,
    tx_hash: &[u8],
) -> Result<()> {
    let epoch_dir = storage_path.join("epochs").join(format!("epoch_{}", epoch));

    // Ensure epoch directory exists
    std::fs::create_dir_all(&epoch_dir)?;

    let hashes_file = epoch_dir.join("committed_transaction_hashes.bin");

    // Load existing hashes
    let mut hashes = if hashes_file.exists() {
        load_transaction_hashes_from_file(&hashes_file)
            .await
            .unwrap_or_default()
    } else {
        std::collections::HashSet::new()
    };

    // Add new hash
    hashes.insert(tx_hash.to_vec());

    // Save back to file
    save_transaction_hashes_to_file(&hashes_file, &hashes).await?;

    trace!(
        "💾 [TX HASH REGISTRY] Saved committed transaction hash to epoch {}",
        epoch
    );
    Ok(())
}

/// Load transaction hashes from binary file
async fn load_transaction_hashes_from_file(
    file_path: &std::path::Path,
) -> Result<std::collections::HashSet<Vec<u8>>> {
    use tokio::fs::File;

    let mut file = File::open(file_path).await?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).await?;

    let mut hashes = std::collections::HashSet::new();
    let mut cursor = std::io::Cursor::new(buffer);

    // Read count
    let mut count_buf = [0u8; 8];
    std::io::Read::read_exact(&mut cursor, &mut count_buf)?;
    let count = u64::from_le_bytes(count_buf);

    // Read hashes
    for _ in 0..count {
        let mut len_buf = [0u8; 4];
        std::io::Read::read_exact(&mut cursor, &mut len_buf)?;
        let len = u32::from_le_bytes(len_buf) as usize;

        let mut hash = vec![0u8; len];
        std::io::Read::read_exact(&mut cursor, &mut hash)?;
        hashes.insert(hash);
    }

    Ok(hashes)
}

/// Save transaction hashes to binary file
async fn save_transaction_hashes_to_file(
    file_path: &std::path::Path,
    hashes: &std::collections::HashSet<Vec<u8>>,
) -> Result<()> {
    use tokio::fs::File;

    let mut file = File::create(file_path).await?;
    let mut buffer = Vec::new();

    // Write count
    let count = hashes.len() as u64;
    buffer.extend_from_slice(&count.to_le_bytes());

    // Write hashes
    for hash in hashes {
        let len = hash.len() as u32;
        buffer.extend_from_slice(&len.to_le_bytes());
        buffer.extend_from_slice(hash);
    }

    file.write_all(&buffer).await?;
    file.flush().await?;

    Ok(())
}
