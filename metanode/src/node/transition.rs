// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use crate::config::NodeConfig;
use crate::node::executor_client::ExecutorClient;
use crate::node::tx_submitter::TransactionSubmitter;
use crate::node::{ConsensusNode, NodeMode};
use anyhow::Result;
use consensus_core::{
    CommitConsumerArgs, ConsensusAuthority, NetworkType, SystemTransactionProvider,
}; // Removed unused ReconfigState, DefaultSystemTransactionProvider
use prometheus::Registry;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::sleep;
use tracing::{error, info, trace, warn}; // Added TransactionSubmitter trait
                                         // Removed unused RocksDBStore import

pub async fn transition_to_epoch_from_system_tx(
    node: &mut ConsensusNode,
    new_epoch: u64,
    new_epoch_timestamp_ms: u64,
    synced_global_exec_index: u64, // CHANGED: Use global_exec_index (u64) instead of commit_index (u32)
    config: &NodeConfig,
) -> Result<()> {
    // CRITICAL FIX: Prevent duplicate epoch transitions
    // Multiple EndOfEpoch transactions can trigger multiple transitions to the same epoch
    // This causes RocksDB lock conflicts when trying to open the same DB path twice
    if node.current_epoch >= new_epoch {
        info!(
            "ℹ️ [TRANSITION SKIP] Already at epoch {} (requested: {}). Skipping duplicate transition.",
            node.current_epoch, new_epoch
        );
        return Ok(());
    }

    if node.is_transitioning.swap(true, Ordering::SeqCst) {
        warn!("⚠️ Transition already in progress, skipping.");
        node.is_transitioning.store(false, Ordering::SeqCst);
        return Ok(());
    }

    info!(
        "🔄 TRANSITION: epoch {} -> {}",
        node.current_epoch, new_epoch
    );
    // No sleep needed here - proceed immediately with transition

    // Reset flag guard
    struct Guard(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Guard {
        fn drop(&mut self) {
            if self.0.load(Ordering::SeqCst) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
    }
    let _guard = Guard(node.is_transitioning.clone());

    node.close_user_certs().await;

    // Wait for processor
    let timeout_secs = if config.epoch_transition_optimization == "fast" {
        5
    } else {
        10
    };
    let _ =
        wait_for_commit_processor_completion(node, synced_global_exec_index as u32, timeout_secs)
            .await;

    // Check executor read is enabled
    if !config.executor_read_enabled {
        anyhow::bail!("Executor read disabled");
    }

    // Deterministic calc for verification only - should match Go's last block
    let calculated_last_block = crate::consensus::checkpoint::calculate_global_exec_index(
        node.current_epoch,
        synced_global_exec_index as u32, // Cast for checkpoint calculation
        node.last_global_exec_index,
    );

    // UNIFIED COMMITTEE SOURCE: Use CommitteeSource for fork-safe committee fetching
    // This ensures BOTH SyncOnly and Validator modes use the same logic
    let committee_source = crate::node::committee_source::CommitteeSource::discover(config).await?;

    // Validate epoch consistency
    if !committee_source.validate_epoch(new_epoch) {
        warn!(
            "⚠️ [TRANSITION] Epoch mismatch detected. Expected={}, Source={}. Proceeding with source epoch.",
            new_epoch, committee_source.epoch
        );
    }

    // Use the unified source's executor client
    let executor_client =
        committee_source.create_executor_client(&config.executor_send_socket_path);

    // Get synced_index from the authoritative source
    let synced_index = if committee_source.last_block > 0 {
        info!(
            "📊 [SYNC] Using committee source last block: {} (from {})",
            committee_source.last_block,
            if committee_source.is_peer {
                "peer"
            } else {
                "local"
            }
        );
        committee_source.last_block
    } else if let Ok(go_last) = executor_client.get_last_block_number().await {
        info!("📊 [SYNC] Go last committed block: {}", go_last);
        go_last
    } else {
        warn!(
            "❌ [SYNC] Failed to get last block from Go, using node last_global_exec_index {}",
            node.last_global_exec_index
        );
        node.last_global_exec_index
    };

    if calculated_last_block != synced_index + 1 {
        warn!("⚠️ [SYNC] Calculated last block {} doesn't match Go's last block {} + 1. Using Go's value.",
            calculated_last_block, synced_index);
    }

    info!(
        "📊 Snapshot: Last committed block from Go: {}",
        synced_index
    );

    // Stop old authority
    if let Some(auth) = node.authority.take() {
        auth.stop().await;
    }

    // Update state
    node.current_epoch = new_epoch;
    node.current_commit_index.store(0, Ordering::SeqCst);

    {
        let mut g = node.shared_last_global_exec_index.lock().await;
        *g = synced_index;
    }
    node.last_global_exec_index = synced_index;
    node.update_execution_lock_epoch(new_epoch).await;

    // CRITICAL: Use epoch_timestamp from CommitteeSource for consistency
    // This ensures genesis block hash matches across all nodes
    let verified_epoch_timestamp_ms = committee_source.get_epoch_timestamp();
    if verified_epoch_timestamp_ms != new_epoch_timestamp_ms && verified_epoch_timestamp_ms > 0 {
        warn!(
            "⚠️ [TRANSITION] Epoch timestamp mismatch! Passed={}, Source={}. Using source timestamp for fork prevention.",
            new_epoch_timestamp_ms, verified_epoch_timestamp_ms
        );
    }
    let epoch_timestamp_to_use = if verified_epoch_timestamp_ms > 0 {
        verified_epoch_timestamp_ms
    } else {
        new_epoch_timestamp_ms
    };

    node.system_transaction_provider
        .update_epoch(new_epoch, epoch_timestamp_to_use)
        .await;

    // Prepare DB
    let db_path = node
        .storage_path
        .join("epochs")
        .join(format!("epoch_{}", new_epoch))
        .join("consensus_db");
    if db_path.exists() {
        let _ = std::fs::remove_dir_all(&db_path);
    }
    std::fs::create_dir_all(&db_path)?;

    // Fetch committee from unified source FOR THE NEW EPOCH
    // CRITICAL: Pass new_epoch to ensure we get the correct validator set
    info!(
        "📋 [COMMITTEE] Fetching committee for epoch {} from {} (epoch={}, block={})",
        new_epoch,
        committee_source.socket_path,
        committee_source.epoch,
        committee_source.last_block
    );
    let committee = committee_source
        .fetch_committee(&config.executor_send_socket_path, new_epoch)
        .await?;
    node.check_and_update_node_mode(&committee, config).await?;

    // FIX: Use protocol_key matching for consistent identity
    let own_protocol_pubkey = node.protocol_keypair.public();
    if let Some((idx, _)) = committee
        .authorities()
        .find(|(_, a)| a.protocol_key == own_protocol_pubkey)
    {
        node.own_index = idx;
        info!(
            "✅ [TRANSITION] Found self in new committee at index {}",
            idx
        );
    } else {
        node.own_index = consensus_config::AuthorityIndex::ZERO;
        info!("ℹ️ [TRANSITION] Not in new committee (protocol_key not found)");
    }

    // Only setup consensus components if we're in Validator mode
    // SyncOnly nodes don't need CommitProcessor or Authority
    if matches!(node.node_mode, NodeMode::Validator) {
        // Setup new processor for Validator mode
        let (commit_consumer, commit_receiver, mut block_receiver) = CommitConsumerArgs::new(0, 0);
        let epoch_cb = crate::consensus::commit_callbacks::create_epoch_transition_callback(
            node.epoch_transition_sender.clone(),
        );

        let exec_client_proc = if node.executor_commit_enabled {
            Some(Arc::new(ExecutorClient::new_with_initial_index(
                true,
                true,
                config.executor_send_socket_path.clone(),
                config.executor_receive_socket_path.clone(),
                synced_index + 1,
                Some(node.storage_path.clone()), // Enable persistence for commit client
            )))
        } else {
            None
        };

        let mut processor =
            crate::consensus::commit_processor::CommitProcessor::new(commit_receiver)
                .with_commit_index_callback(
                    crate::consensus::commit_callbacks::create_commit_index_callback(
                        node.current_commit_index.clone(),
                    ),
                )
                .with_global_exec_index_callback(
                    crate::consensus::commit_callbacks::create_global_exec_index_callback(
                        node.shared_last_global_exec_index.clone(),
                    ),
                )
                .with_shared_last_global_exec_index(node.shared_last_global_exec_index.clone())
                .with_epoch_info(new_epoch, synced_index)
                .with_is_transitioning(node.is_transitioning.clone())
                .with_pending_transactions_queue(node.pending_transactions_queue.clone())
                .with_epoch_transition_callback(epoch_cb);

        if let Some(c) = exec_client_proc {
            processor = processor.with_executor_client(c);
        }

        tokio::spawn(async move {
            let _ = processor.run().await;
        });
        tokio::spawn(async move { while block_receiver.recv().await.is_some() {} });

        // Start Authority for Validator mode
        let mut params = node.parameters.clone();
        params.db_path = db_path;
        node.boot_counter += 1;

        node.authority =
            Some(
                ConsensusAuthority::start(
                    NetworkType::Tonic,
                    epoch_timestamp_to_use, // CRITICAL: Use verified timestamp from CommitteeSource
                    node.own_index,
                    committee,
                    params,
                    node.protocol_config.clone(),
                    node.protocol_keypair.clone(),
                    node.network_keypair.clone(),
                    node.clock.clone(),
                    node.transaction_verifier.clone(),
                    commit_consumer,
                    Registry::new(),
                    node.boot_counter,
                    Some(node.system_transaction_provider.clone()
                        as Arc<dyn SystemTransactionProvider>),
                )
                .await,
            );

        // Update proxy for Validator mode
        if let Some(auth) = &node.authority {
            if let Some(proxy) = &node.transaction_client_proxy {
                proxy.set_client(auth.transaction_client()).await;
            } else {
                node.transaction_client_proxy = Some(Arc::new(
                    crate::node::tx_submitter::TransactionClientProxy::new(
                        auth.transaction_client(),
                    ),
                ));
            }
        }
    } else {
        // SyncOnly mode: Clear authority and proxy
        info!("🔄 [EPOCH TRANSITION] SyncOnly mode - skipping consensus setup");
        node.authority = None;
        node.transaction_client_proxy = None;
    }

    // Wait for consensus to stabilize with proper synchronization instead of fixed sleep
    if wait_for_consensus_ready(node).await {
        info!("✅ Consensus ready.");
    }

    // Recover transactions from previous epoch that were not committed
    let _ = recover_epoch_pending_transactions(node).await;

    node.is_transitioning.store(false, Ordering::SeqCst);
    let _ = node.submit_queued_transactions().await;

    node.reset_reconfig_state().await;

    // Notify Go
    let _ = executor_client
        .advance_epoch(new_epoch, new_epoch_timestamp_ms)
        .await;

    // FORK-SAFETY: Verify Go and Rust epochs match after transition
    // This catches any epoch desync early to prevent forks
    match executor_client.get_current_epoch().await {
        Ok(go_epoch) => {
            if go_epoch != new_epoch {
                warn!(
                    "⚠️ [EPOCH VERIFY] Go-Rust epoch mismatch! Rust: {}, Go: {}. \
                     This could indicate a fork risk. Consider investigating.",
                    new_epoch, go_epoch
                );
            } else {
                info!("✅ [EPOCH VERIFY] Go-Rust epoch consistent: {}", new_epoch);
            }
        }
        Err(e) => {
            warn!("⚠️ [EPOCH VERIFY] Failed to verify epoch with Go: {}", e);
        }
    }

    // FORK-SAFETY: Sync timestamp from Go to ensure consistency
    // CRITICAL: Retry with delay to allow Go to update its state
    // Avoid using stale timestamp from old epoch
    let go_epoch_timestamp_ms = match sync_epoch_timestamp_from_go(
        &executor_client,
        new_epoch,
        new_epoch_timestamp_ms,
    )
    .await
    {
        Ok(timestamp) => {
            if timestamp != new_epoch_timestamp_ms {
                warn!(
                    "⚠️ [EPOCH TIMESTAMP SYNC] Timestamp mismatch after transition: \
                     Local calculated: {}ms, Go reported: {}ms, diff: {}ms. \
                     Using Go's timestamp to prevent fork.",
                    new_epoch_timestamp_ms,
                    timestamp,
                    (timestamp as i64 - new_epoch_timestamp_ms as i64).abs()
                );
                timestamp
            } else {
                info!(
                    "✅ [EPOCH TIMESTAMP SYNC] Timestamp consistent between local and Go: {}ms",
                    new_epoch_timestamp_ms
                );
                timestamp
            }
        }
        Err(e) => {
            // Check if this is a "not implemented" error (endpoint missing)
            if e.to_string().contains("not found") || e.to_string().contains("Unexpected response")
            {
                info!("ℹ️ [EPOCH TIMESTAMP SYNC] Go endpoint not implemented yet, using local calculation: {}ms", new_epoch_timestamp_ms);
            } else {
                warn!("⚠️ [EPOCH TIMESTAMP SYNC] Failed to sync timestamp from Go: {}. Using local calculation.", e);
            }
            new_epoch_timestamp_ms
        }
    };

    // Update SystemTransactionProvider with verified timestamp
    node.system_transaction_provider
        .update_epoch(new_epoch, go_epoch_timestamp_ms)
        .await;

    // SNAPSHOT TRIGGER: Create LVM snapshot after successful epoch transition
    if config.enable_lvm_snapshot {
        if let Some(bin_path) = &config.lvm_snapshot_bin_path {
            let delay_seconds = config.lvm_snapshot_delay_seconds;
            let snapshot_epoch = new_epoch.saturating_sub(1); // Snapshot the COMPLETED epoch
            let bin_path_clone = bin_path.clone();

            info!(
                "📸 [LVM SNAPSHOT] Scheduling snapshot creation for epoch {} in {} seconds...",
                snapshot_epoch, delay_seconds
            );

            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(delay_seconds)).await;
                trigger_lvm_snapshot(&bin_path_clone, snapshot_epoch).await;
            });
        } else {
            warn!(
                "⚠️ [LVM SNAPSHOT] enable_lvm_snapshot=true but lvm_snapshot_bin_path is not set!"
            );
        }
    }

    Ok(())
}

async fn wait_for_commit_processor_completion(
    node: &ConsensusNode,
    target: u32,
    max_wait: u64,
) -> Result<()> {
    let start = std::time::Instant::now();
    loop {
        let current = node.current_commit_index.load(Ordering::SeqCst);
        if current >= target {
            return Ok(());
        }
        if start.elapsed().as_secs() >= max_wait {
            return Err(anyhow::anyhow!("Timeout"));
        }
        // Polling sleep: Wait 100ms before checking commit index again
        // This is acceptable for infrequent epoch transitions where precise timing isn't critical
        sleep(Duration::from_millis(100)).await;
    }
}

/// Wait for consensus to become ready with retries instead of fixed sleep
/// This replaces the unreliable 1000ms sleep with proper synchronization
async fn wait_for_consensus_ready(node: &ConsensusNode) -> bool {
    let max_attempts = 20; // Up to 2 seconds with 100ms intervals
    let retry_delay = Duration::from_millis(100);

    for attempt in 1..=max_attempts {
        if test_consensus_readiness(node).await {
            return true;
        }

        if attempt < max_attempts {
            trace!(
                "⏳ Consensus not ready yet (attempt {}/{}), waiting...",
                attempt,
                max_attempts
            );
            sleep(retry_delay).await;
        }
    }

    warn!(
        "⚠️ Consensus failed to become ready after {} attempts",
        max_attempts
    );
    false
}

async fn test_consensus_readiness(node: &ConsensusNode) -> bool {
    if let Some(proxy) = &node.transaction_client_proxy {
        match proxy.submit(vec![vec![0u8; 64]]).await {
            Ok(_) => true,
            Err(_) => false,
        }
    } else {
        false
    }
}

/// Sync epoch timestamp from Go with retry logic to avoid stale timestamps
/// CRITICAL: Prevents using timestamp from old epoch after transition
/// Recover transactions that were submitted in the previous epoch but not committed
async fn recover_epoch_pending_transactions(node: &mut ConsensusNode) -> Result<usize> {
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

async fn sync_epoch_timestamp_from_go(
    executor_client: &ExecutorClient,
    expected_epoch: u64,
    expected_timestamp: u64,
) -> Result<u64> {
    const MAX_RETRIES: u32 = 5;
    const RETRY_DELAY_MS: u64 = 200;

    for attempt in 1..=MAX_RETRIES {
        // First check if Go has transitioned to expected epoch
        match executor_client.get_current_epoch().await {
            Ok(go_current_epoch) => {
                if go_current_epoch != expected_epoch {
                    if attempt == MAX_RETRIES {
                        return Err(anyhow::anyhow!(
                            "Go still in epoch {} after {} attempts, expected epoch {}",
                            go_current_epoch,
                            MAX_RETRIES,
                            expected_epoch
                        ));
                    }
                    warn!(
                        "⚠️ [EPOCH SYNC] Go still in epoch {} (attempt {}/{}), expected {}. Retrying...",
                        go_current_epoch, attempt, MAX_RETRIES, expected_epoch
                    );
                    sleep(Duration::from_millis(RETRY_DELAY_MS)).await;
                    continue;
                }
            }
            Err(e) => {
                warn!(
                    "⚠️ [EPOCH SYNC] Failed to get current epoch from Go (attempt {}/{}): {}",
                    attempt, MAX_RETRIES, e
                );
                if attempt == MAX_RETRIES {
                    return Err(anyhow::anyhow!(
                        "Failed to verify Go epoch after transition: {}",
                        e
                    ));
                }
                sleep(Duration::from_millis(RETRY_DELAY_MS)).await;
                continue;
            }
        }

        // Now get timestamp and validate it's reasonable
        match executor_client.get_epoch_start_timestamp().await {
            Ok(go_timestamp) => {
                // Validate timestamp is not from old epoch (should be close to expected)
                // Timestamp should be within reasonable range of expected timestamp
                let timestamp_diff = (go_timestamp as i64 - expected_timestamp as i64).abs() as u64;

                if timestamp_diff > 10000 {
                    // 10 seconds tolerance
                    warn!(
                        "⚠️ [EPOCH SYNC] Go timestamp {}ms differs from expected {}ms by {}ms (attempt {}/{}). \
                         This may indicate stale timestamp from old epoch.",
                        go_timestamp, expected_timestamp, timestamp_diff, attempt, MAX_RETRIES
                    );

                    if attempt == MAX_RETRIES {
                        // At final attempt, accept the timestamp but log warning
                        warn!(
                            "⚠️ [EPOCH SYNC] Using Go timestamp despite large difference. \
                               This may cause epoch timing issues."
                        );
                        return Ok(go_timestamp);
                    }

                    sleep(Duration::from_millis(RETRY_DELAY_MS)).await;
                    continue;
                }

                info!(
                    "✅ [EPOCH SYNC] Successfully synced timestamp from Go: {}ms (diff: {}ms)",
                    go_timestamp, timestamp_diff
                );
                return Ok(go_timestamp);
            }
            Err(e) => {
                warn!(
                    "⚠️ [EPOCH SYNC] Failed to get timestamp from Go (attempt {}/{}): {}",
                    attempt, MAX_RETRIES, e
                );
                if attempt == MAX_RETRIES {
                    return Err(anyhow::anyhow!(
                        "Failed to get timestamp from Go after transition: {}",
                        e
                    ));
                }
                sleep(Duration::from_millis(RETRY_DELAY_MS)).await;
                continue;
            }
        }
    }

    Err(anyhow::anyhow!(
        "Failed to sync epoch timestamp from Go after {} attempts",
        MAX_RETRIES
    ))
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

/// Trigger LVM snapshot creation by calling the external lvm-snap-rsync binary
/// This is called asynchronously after epoch transition to avoid blocking consensus
async fn trigger_lvm_snapshot(bin_path: &std::path::Path, epoch_id: u64) {
    use std::process::Command;

    info!(
        "📸 [LVM SNAPSHOT] Creating snapshot for epoch {} using {}",
        epoch_id,
        bin_path.display()
    );

    // Run the snapshot command with sudo (required for LVM operations)
    // The binary expects --id <epoch_number> argument
    let result = Command::new("sudo")
        .arg(bin_path)
        .arg("--id")
        .arg(epoch_id.to_string())
        .output();

    match result {
        Ok(output) => {
            if output.status.success() {
                info!(
                    "✅ [LVM SNAPSHOT] Successfully created snapshot for epoch {}",
                    epoch_id
                );
                if !output.stdout.is_empty() {
                    info!(
                        "📸 [LVM SNAPSHOT] Output: {}",
                        String::from_utf8_lossy(&output.stdout)
                    );
                }
            } else {
                error!(
                    "❌ [LVM SNAPSHOT] Failed to create snapshot for epoch {}: exit code {:?}",
                    epoch_id,
                    output.status.code()
                );
                if !output.stderr.is_empty() {
                    error!(
                        "❌ [LVM SNAPSHOT] Stderr: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }
        }
        Err(e) => {
            error!(
                "❌ [LVM SNAPSHOT] Failed to execute snapshot command for epoch {}: {}",
                epoch_id, e
            );
        }
    }
}
