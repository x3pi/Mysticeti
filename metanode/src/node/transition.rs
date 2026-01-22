// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use tracing::{info, warn, trace}; // Removed unused error
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use crate::config::NodeConfig;
use crate::executor_client::ExecutorClient;
use crate::node::{ConsensusNode, NodeMode};
use consensus_core::{ConsensusAuthority, NetworkType, CommitConsumerArgs, SystemTransactionProvider}; // Removed unused ReconfigState, DefaultSystemTransactionProvider
use prometheus::Registry;
use tokio::time::sleep;
use crate::tx_submitter::TransactionSubmitter; // Added TransactionSubmitter trait
// Removed unused RocksDBStore import

pub async fn transition_to_epoch_from_system_tx(
    node: &mut ConsensusNode,
    new_epoch: u64,
    new_epoch_timestamp_ms: u64,
    commit_index: u32,
    config: &NodeConfig,
) -> Result<()> {
    if node.is_transitioning.swap(true, Ordering::SeqCst) {
        warn!("⚠️ Transition already in progress, skipping.");
        node.is_transitioning.store(false, Ordering::SeqCst);
        return Ok(());
    }

    info!("🔄 TRANSITION: epoch {} -> {}", node.current_epoch, new_epoch);
    // No sleep needed here - proceed immediately with transition

    // Reset flag guard
    struct Guard(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Guard { fn drop(&mut self) { if self.0.load(Ordering::SeqCst) { self.0.store(false, Ordering::SeqCst); } } }
    let _guard = Guard(node.is_transitioning.clone());

    node.close_user_certs().await;

    // Wait for processor
    let timeout_secs = if config.epoch_transition_optimization == "fast" { 5 } else { 10 };
    let _ = wait_for_commit_processor_completion(node, commit_index, timeout_secs).await;
    
    // Deterministic calc using commit_index from EndOfEpoch
    let last_global_exec_index_at_transition = crate::checkpoint::calculate_global_exec_index(
        node.current_epoch, commit_index, node.last_global_exec_index
    );
    
    info!("📊 Snapshot: Last block of epoch {}: {}", node.current_epoch, last_global_exec_index_at_transition);
    // No additional sleep needed - commit processor completion already waited above

    // Stop old authority
    if let Some(auth) = node.authority.take() {
        auth.stop().await;
    }

    // Update state
    node.current_epoch = new_epoch;
    node.current_commit_index.store(0, Ordering::SeqCst);
    
    // Sync with Go
    let executor_client = if config.executor_read_enabled {
        Arc::new(ExecutorClient::new(true, false, config.executor_send_socket_path.clone(), config.executor_receive_socket_path.clone()))
    } else { anyhow::bail!("Executor read disabled"); };
    
    let synced_index = if let Ok(go_last) = executor_client.get_last_block_number().await {
        if go_last >= last_global_exec_index_at_transition { go_last } else { last_global_exec_index_at_transition }
    } else {
        last_global_exec_index_at_transition
    };

    {
        let mut g = node.shared_last_global_exec_index.lock().await;
        *g = synced_index;
    }
    node.last_global_exec_index = synced_index;
    node.update_execution_lock_epoch(new_epoch).await;
    node.system_transaction_provider.update_epoch(new_epoch, new_epoch_timestamp_ms).await;

    // Prepare DB
    let db_path = node.storage_path.join("epochs").join(format!("epoch_{}", new_epoch)).join("consensus_db");
    if db_path.exists() { let _ = std::fs::remove_dir_all(&db_path); }
    std::fs::create_dir_all(&db_path)?;

    // Fetch committee
    let committee = crate::node::committee::build_committee_from_go_validators_at_block_with_epoch(&executor_client, synced_index, new_epoch).await?;
    node.check_and_update_node_mode(&committee, config).await?;

    let node_hostname = format!("node-{}", config.node_id);
    if let Some((idx, _)) = committee.authorities().find(|(_, a)| a.hostname == node_hostname) {
        node.own_index = idx;
    } else {
        node.own_index = consensus_config::AuthorityIndex::ZERO;
    }

    // Setup new processor
    let (commit_consumer, commit_receiver, mut block_receiver) = CommitConsumerArgs::new(0, 0);
    let epoch_cb = crate::commit_callbacks::create_epoch_transition_callback(node.epoch_transition_sender.clone());
    
    let exec_client_proc = if node.executor_commit_enabled {
        Some(Arc::new(ExecutorClient::new_with_initial_index(
            true, true, config.executor_send_socket_path.clone(), config.executor_receive_socket_path.clone(), synced_index + 1
        )))
    } else { None };

    let mut processor = crate::commit_processor::CommitProcessor::new(commit_receiver)
        .with_commit_index_callback(crate::commit_callbacks::create_commit_index_callback(node.current_commit_index.clone()))
        .with_global_exec_index_callback(crate::commit_callbacks::create_global_exec_index_callback(node.shared_last_global_exec_index.clone()))
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

    // Start Authority
    if matches!(node.node_mode, NodeMode::Validator) {
        let mut params = node.parameters.clone();
        params.db_path = db_path;
        node.boot_counter += 1;
        
        node.authority = Some(ConsensusAuthority::start(
            NetworkType::Tonic, new_epoch_timestamp_ms, node.own_index, committee, params,
            node.protocol_config.clone(), node.protocol_keypair.clone(), node.network_keypair.clone(),
            node.clock.clone(), node.transaction_verifier.clone(), commit_consumer, Registry::new(),
            node.boot_counter, Some(node.system_transaction_provider.clone() as Arc<dyn SystemTransactionProvider>)
        ).await);
    }

    // Update proxy
    if let Some(auth) = &node.authority {
        if let Some(proxy) = &node.transaction_client_proxy {
            proxy.set_client(auth.transaction_client()).await;
        } else {
             node.transaction_client_proxy = Some(Arc::new(crate::tx_submitter::TransactionClientProxy::new(auth.transaction_client())));
        }
    } else {
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
    let _ = executor_client.advance_epoch(new_epoch, new_epoch_timestamp_ms).await;

    // FORK-SAFETY: Sync timestamp from Go to ensure consistency
    // CRITICAL: Retry with delay to allow Go to update its state
    // Avoid using stale timestamp from old epoch
    let go_epoch_timestamp_ms = match sync_epoch_timestamp_from_go(&executor_client, new_epoch, new_epoch_timestamp_ms).await {
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
                info!("✅ [EPOCH TIMESTAMP SYNC] Timestamp consistent between local and Go: {}ms", new_epoch_timestamp_ms);
                timestamp
            }
        }
        Err(e) => {
            // Check if this is a "not implemented" error (endpoint missing)
            if e.to_string().contains("not found") || e.to_string().contains("Unexpected response") {
                info!("ℹ️ [EPOCH TIMESTAMP SYNC] Go endpoint not implemented yet, using local calculation: {}ms", new_epoch_timestamp_ms);
            } else {
                warn!("⚠️ [EPOCH TIMESTAMP SYNC] Failed to sync timestamp from Go: {}. Using local calculation.", e);
            }
            new_epoch_timestamp_ms
        }
    };

    // Update SystemTransactionProvider with verified timestamp
    node.system_transaction_provider.update_epoch(new_epoch, go_epoch_timestamp_ms).await;

    Ok(())
}

async fn wait_for_commit_processor_completion(node: &ConsensusNode, target: u32, max_wait: u64) -> Result<()> {
    let start = std::time::Instant::now();
    loop {
        let current = node.current_commit_index.load(Ordering::SeqCst);
        if current >= target { return Ok(()); }
        if start.elapsed().as_secs() >= max_wait { return Err(anyhow::anyhow!("Timeout")); }
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
            trace!("⏳ Consensus not ready yet (attempt {}/{}), waiting...", attempt, max_attempts);
            sleep(retry_delay).await;
        }
    }

    warn!("⚠️ Consensus failed to become ready after {} attempts", max_attempts);
    false
}

async fn test_consensus_readiness(node: &ConsensusNode) -> bool {
    if let Some(proxy) = &node.transaction_client_proxy {
        match proxy.submit(vec![vec![0u8; 64]]).await {
             Ok(_) => true,
             Err(_) => false,
        }
    } else { false }
}

/// Sync epoch timestamp from Go with retry logic to avoid stale timestamps
/// CRITICAL: Prevents using timestamp from old epoch after transition
/// Recover transactions that were submitted in the previous epoch but not committed
async fn recover_epoch_pending_transactions(
    node: &mut ConsensusNode,
) -> Result<usize> {
    let mut epoch_pending = node.epoch_pending_transactions.lock().await;
    if epoch_pending.is_empty() {
        return Ok(0);
    }

    info!("🔄 [EPOCH RECOVERY] Checking {} transactions from previous epoch for recovery", epoch_pending.len());

    let mut transactions_to_recover = Vec::new();

    // During epoch transition, we assume all pending transactions from the old epoch
    // that were submitted but not committed need to be recovered
    // This is a conservative approach - better to resubmit than to lose transactions
    info!("🔄 [EPOCH RECOVERY] Epoch transition detected - recovering all {} pending transactions from previous epoch",
          epoch_pending.len());

    transactions_to_recover.extend(epoch_pending.iter().cloned());

    // Clear the pending list - we'll resubmit what needs recovery
    epoch_pending.clear();

    if transactions_to_recover.is_empty() {
        info!("✅ [EPOCH RECOVERY] No transactions need recovery");
        return Ok(0);
    }

    // Resubmit transactions to new epoch
    info!("🚀 [EPOCH RECOVERY] Resubmitting {} transactions to new epoch", transactions_to_recover.len());

    let mut recovered_count = 0;
    let total_count = transactions_to_recover.len();

    for tx_data in transactions_to_recover {
        if let Some(proxy) = &node.transaction_client_proxy {
            match proxy.submit(vec![tx_data.clone()]).await {
                Ok(_) => {
                    recovered_count += 1;
                    info!("✅ [EPOCH RECOVERY] Successfully recovered transaction");
                }
                Err(e) => {
                    warn!("❌ [EPOCH RECOVERY] Failed to recover transaction: {}", e);
                    // Put back into pending queue for later retry
                    let mut pending = node.pending_transactions_queue.lock().await;
                    pending.push(tx_data);
                }
            }
        }
    }

    info!("📊 [EPOCH RECOVERY] Recovered {} out of {} transactions", recovered_count, total_count);
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
                            go_current_epoch, MAX_RETRIES, expected_epoch
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
                warn!("⚠️ [EPOCH SYNC] Failed to get current epoch from Go (attempt {}/{}): {}", attempt, MAX_RETRIES, e);
                if attempt == MAX_RETRIES {
                    return Err(anyhow::anyhow!("Failed to verify Go epoch after transition: {}", e));
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

                if timestamp_diff > 10000 { // 10 seconds tolerance
                    warn!(
                        "⚠️ [EPOCH SYNC] Go timestamp {}ms differs from expected {}ms by {}ms (attempt {}/{}). \
                         This may indicate stale timestamp from old epoch.",
                        go_timestamp, expected_timestamp, timestamp_diff, attempt, MAX_RETRIES
                    );

                    if attempt == MAX_RETRIES {
                        // At final attempt, accept the timestamp but log warning
                        warn!("⚠️ [EPOCH SYNC] Using Go timestamp despite large difference. \
                               This may cause epoch timing issues.");
                        return Ok(go_timestamp);
                    }

                    sleep(Duration::from_millis(RETRY_DELAY_MS)).await;
                    continue;
                }

                info!("✅ [EPOCH SYNC] Successfully synced timestamp from Go: {}ms (diff: {}ms)",
                      go_timestamp, timestamp_diff);
                return Ok(go_timestamp);
            }
            Err(e) => {
                warn!("⚠️ [EPOCH SYNC] Failed to get timestamp from Go (attempt {}/{}): {}", attempt, MAX_RETRIES, e);
                if attempt == MAX_RETRIES {
                    return Err(anyhow::anyhow!("Failed to get timestamp from Go after transition: {}", e));
                }
                sleep(Duration::from_millis(RETRY_DELAY_MS)).await;
                continue;
            }
        }
    }

    Err(anyhow::anyhow!("Failed to sync epoch timestamp from Go after {} attempts", MAX_RETRIES))
}