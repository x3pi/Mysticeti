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
    boundary_block_from_tx: u64, // CHANGED: This is now boundary_block from EndOfEpoch tx, not timestamp
    synced_global_exec_index: u64, // CHANGED: Use global_exec_index (u64) instead of commit_index (u32)
    config: &NodeConfig,
) -> Result<()> {
    // CRITICAL FIX: Prevent duplicate epoch transitions
    // Multiple EndOfEpoch transactions can trigger multiple transitions to the same epoch
    // This causes RocksDB lock conflicts when trying to open the same DB path twice
    let is_sync_only = matches!(node.node_mode, NodeMode::SyncOnly);
    let is_same_epoch = node.current_epoch == new_epoch;

    // CASE 1: Same epoch, but SyncOnly needs to become Validator
    // This is a MODE-ONLY transition - skip full epoch transition, just start authority
    if is_same_epoch && is_sync_only {
        // CRITICAL FIX 2026-02-05: MUST WAIT for Go to sync before promoting to Validator
        // Previously we deferred (returned early) but there was no guarantee of retry.
        // Now we ACTIVELY POLL and WAIT until Go catches up, then promote immediately.
        //
        // IMPORTANT: If a NEW epoch transition occurs while waiting, we must abort and
        // let the new epoch's transition handler take over.
        //
        // Create FRESH executor client for reliable communication
        let fresh_executor_client = ExecutorClient::new(
            true,
            false, // Don't need commit capability for just checking block number
            config.executor_send_socket_path.clone(),
            config.executor_receive_socket_path.clone(),
            None,
        );

        // ACTIVE WAIT: Poll Go until it reaches the required boundary
        // With epoch change detection and timeout safety
        let poll_interval = Duration::from_millis(500);
        let max_attempts = 600; // 5 minutes max (600 * 500ms)
        let mut attempt = 0u64;
        
        loop {
            attempt += 1;
            
            // SAFETY: Check if a new epoch has started - if so, abort and let new handler take over
            let go_current_epoch = fresh_executor_client.get_current_epoch().await.unwrap_or(0);
            if go_current_epoch > new_epoch {
                info!(
                    "🔄 [MODE TRANSITION] New epoch {} detected (was waiting for epoch {}). Aborting to let new epoch handler take over.",
                    go_current_epoch, new_epoch
                );
                return Ok(());
            }
            
            let go_current_block = match fresh_executor_client.get_last_block_number().await {
                Ok(block) => block,
                Err(e) => {
                    if attempt % 20 == 0 {
                        warn!(
                            "⚠️ [MODE TRANSITION] Cannot reach Go (attempt {}): {}. Will keep trying...",
                            attempt, e
                        );
                    }
                    0
                }
            };

            if go_current_block >= synced_global_exec_index {
                info!(
                    "✅ [MODE TRANSITION] Go synced! block {} >= boundary {}. Proceeding to Validator mode. (took {} attempts)",
                    go_current_block, synced_global_exec_index, attempt
                );
                break;
            }

            // Timeout safety - after 5 minutes, give up and let epoch_monitor retry
            if attempt >= max_attempts {
                warn!(
                    "⚠️ [MODE TRANSITION] Timeout after {} attempts (5 min). Go block {} still < boundary {}. Will retry via epoch_monitor.",
                    attempt, go_current_block, synced_global_exec_index
                );
                return Ok(());
            }

            // Log progress every 20 attempts (10 seconds)
            if attempt % 20 == 0 {
                info!(
                    "⏳ [MODE TRANSITION] Waiting for Go sync: block {} / {} ({}% complete, epoch={}, waiting {}s)",
                    go_current_block, 
                    synced_global_exec_index,
                    if synced_global_exec_index > 0 { go_current_block * 100 / synced_global_exec_index } else { 0 },
                    go_current_epoch,
                    attempt / 2
                );
            }

            sleep(poll_interval).await;
        }

        info!(
            "🔄 [MODE TRANSITION] SyncOnly → Validator in epoch {} (not a full epoch transition)",
            new_epoch
        );
        return transition_mode_only(
            node,
            new_epoch,
            boundary_block_from_tx, // Pass boundary_block instead of timestamp
            synced_global_exec_index,
            config,
        )
        .await;
    }

    // CASE 2: Already at this epoch and already Validator - skip
    if node.current_epoch >= new_epoch && !is_sync_only {
        info!(
            "ℹ️ [TRANSITION SKIP] Already at epoch {} (requested: {}) and already Validator. Skipping.",
            node.current_epoch, new_epoch
        );
        return Ok(());
    }

    // CASE 3: Current epoch ahead of requested - skip
    if node.current_epoch > new_epoch {
        info!(
            "ℹ️ [TRANSITION SKIP] Current epoch {} is AHEAD of requested {}. Skipping.",
            node.current_epoch, new_epoch
        );
        return Ok(());
    }

    // CASE 4: Full epoch transition (epoch actually changing)
    info!(
        "🔄 [FULL EPOCH TRANSITION] Processing: epoch {} -> {} (current_mode={:?})",
        node.current_epoch, new_epoch, node.node_mode
    );

    if node.is_transitioning.swap(true, Ordering::SeqCst) {
        warn!("⚠️ Full epoch transition already in progress, skipping.");
        node.is_transitioning.store(false, Ordering::SeqCst);
        return Ok(());
    }

    info!(
        "🔄 FULL TRANSITION: epoch {} -> {}",
        node.current_epoch, new_epoch
    );

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

    // [FIX 2026-01-29]: Calculate correct target_commit_index from synced_global_exec_index
    // FORMULA: global_exec_index = last_global_exec_index + commit_index
    // Therefore: target_commit_index = synced_global_exec_index - last_global_exec_index
    // This ensures we compare commit_index with commit_index (same metric)
    let target_commit_index = if synced_global_exec_index > node.last_global_exec_index {
        (synced_global_exec_index - node.last_global_exec_index) as u32
    } else {
        // Fallback: if somehow global_exec_index is less, use it directly (shouldn't happen)
        synced_global_exec_index as u32
    };
    info!(
        "⏳ [TRANSITION] Waiting for commit_processor: target_commit_index={}, current_commit_index={}, synced_global_exec_index={}, last_global_exec_index={}",
        target_commit_index,
        node.current_commit_index.load(Ordering::SeqCst),
        synced_global_exec_index,
        node.last_global_exec_index
    );

    // Wait for processor to reach the target commit index (ensure sequential block processing)
    // AUTO-DETECT: SyncOnly nodes can skip this wait since Go already has blocks from Rust P2P sync
    // Validator nodes MUST wait to ensure all blocks are committed before stopping authority
    let is_sync_only = matches!(node.node_mode, crate::node::NodeMode::SyncOnly);

    let timeout_secs = if is_sync_only {
        // SyncOnly: skip wait entirely - Go already has blocks from P2P sync
        0
    } else if config.epoch_transition_optimization == "fast" {
        // Validator fast mode: 5s wait
        5
    } else {
        // Validator balanced/default: 10s wait
        10
    };

    if timeout_secs > 0 {
        let _ = wait_for_commit_processor_completion(node, target_commit_index, timeout_secs).await;
    } else {
        info!(
            "⚡ [TRANSITION] SyncOnly mode detected: skipping commit_processor wait (Go already synced via P2P)"
        );
    }

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

    let executor_client =
        committee_source.create_executor_client(&config.executor_send_socket_path);

    // =============================================================================
    // GO-AUTHORITATIVE EPOCH BOUNDARY FIX (2026-02-01)
    // =============================================================================
    // PROBLEM: Old logic compared synced_global_exec_index (from EndOfEpoch tx) with
    //          Go's get_epoch_boundary_data(new_epoch) which returns boundary_block=0
    //          for epoch 1 (Go hasn't stored it yet). This caused verification failure.
    //
    // SOLUTION: Use Go Master's actual last_block_number as the authoritative boundary.
    //           Go knows what blocks it has committed, so this is the source of truth.
    //           All nodes query the same Go Master → consistent boundary across cluster.
    // =============================================================================

    let go_last_block = executor_client
        .get_last_block_number()
        .await
        .map_err(|e| anyhow::anyhow!("Cannot get Go Master's last_block_number: {}", e))?;

    info!(
        "📊 [GO-AUTHORITATIVE] Using Go Master's last_block={} as epoch boundary (EndOfEpoch tx had: {})",
        go_last_block, synced_global_exec_index
    );

    // Use Go's value as the authoritative synced_global_exec_index
    let _synced_global_exec_index = go_last_block;

    // =============================================================================
    // CRITICAL FIX: Stop old authority FIRST before fetching synced_index from Go
    // This prevents race condition where:
    // 1. We fetch synced_index=14400 from Go
    // 2. Old epoch sends more blocks (global_exec_index=14405, 14406, ..., 14409)
    // 3. New epoch starts with epoch_base_index=14400
    // 4. New epoch's commit_index=9 → global_exec_index=14409 (COLLISION!)
    //
    // By stopping old authority FIRST, we ensure all blocks from old epoch
    // have been sent to Go before we fetch epoch_base_index for new epoch.
    // =============================================================================

    info!("🛑 [TRANSITION] Stopping old authority BEFORE fetching synced_index...");

    // Capture the expected last global_exec_index BEFORE stopping authority
    // This is what we expect Go to have after all in-flight blocks are received
    let expected_last_block = {
        let shared_index = node.shared_last_global_exec_index.lock().await;
        *shared_index
    };
    info!(
        "📊 [TRANSITION] Expected last block after old epoch: {}",
        expected_last_block
    );

    // NOTE: We tried keeping old authority running for lagging node sync,
    // but this causes consensus conflicts (transaction blocking).
    // Now we extract the store and add it to LegacyEpochStoreManager for read-only sync.
    if let Some(auth) = node.authority.take() {
        // Extract store before stopping (the store Arc will survive the stop)
        let old_store = auth.take_store();
        let old_epoch = new_epoch.saturating_sub(1);
        node.legacy_store_manager.add_store(old_epoch, old_store);
        info!(
            "📦 [TRANSITION] Extracted store from epoch {} for legacy sync",
            old_epoch
        );

        auth.stop().await;
        info!("✅ [TRANSITION] Old authority stopped. Store preserved in LegacyEpochStoreManager.");
    }

    // =============================================================================
    // STRICT SEQUENTIAL GUARANTEE: Poll Go FOREVER until it confirms receiving expected_last_block
    // NO GAP, NO OVERLAP policy - we MUST NOT proceed until Go is in sync
    // This prevents the duplicate global_exec_index race condition
    // =============================================================================
    let poll_interval = Duration::from_millis(100);
    let mut go_last_block;
    let mut attempt = 0u64;

    loop {
        attempt += 1;
        match executor_client.get_last_block_number().await {
            Ok(last_block) => {
                go_last_block = last_block;
                if go_last_block >= expected_last_block {
                    info!(
                        "✅ [SYNC VERIFIED] Go confirmed receiving all blocks: go_last={} >= expected={} (took {} attempts)",
                        go_last_block, expected_last_block, attempt
                    );
                    break;
                } else {
                    // Log every 100 attempts (10 seconds) to show we're waiting
                    if attempt % 100 == 0 {
                        warn!(
                            "⏳ [SYNC WAIT] Waiting for Go to catch up: go_last={}, expected={} (waiting for {}s)",
                            go_last_block, expected_last_block, attempt / 10
                        );
                    }
                }
            }
            Err(e) => {
                // Log errors but keep trying
                if attempt % 100 == 0 {
                    error!(
                        "❌ [SYNC POLL] Cannot reach Go (attempt {}): {}. Will keep trying...",
                        attempt, e
                    );
                }
            }
        }

        tokio::time::sleep(poll_interval).await;
    }

    // At this point, go_last_block >= expected_last_block is GUARANTEED
    // NOW fetch final synced_index from Go - this should include all blocks from old epoch
    let synced_index = if let Ok(go_last) = executor_client.get_last_block_number().await {
        info!("📊 [SYNC] Go last committed block (verified): {}", go_last);
        go_last
    } else if committee_source.last_block > 0 {
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

    // Update state
    node.current_epoch = new_epoch;
    node.current_commit_index.store(0, Ordering::SeqCst);

    {
        let mut g = node.shared_last_global_exec_index.lock().await;
        *g = synced_index;
    }
    node.last_global_exec_index = synced_index;
    node.update_execution_lock_epoch(new_epoch).await;

    // EPOCH STATE LOG: Comprehensive state dump for debugging transitions
    info!(
        "📊 [EPOCH STATE UPDATED] epoch={}, last_global_exec_index={}, commit_index={}, mode={:?}",
        node.current_epoch,
        node.last_global_exec_index,
        node.current_commit_index.load(Ordering::SeqCst),
        node.node_mode
    );

    // =============================================================================
    // SIMPLIFIED: Timestamp is NOT in EndOfEpoch anymore!
    // We only have boundary_block_from_tx. Timestamp will be fetched from Go AFTER
    // Go advances epoch (Go derives it from boundary block header).
    // Use provisional value here; will be updated after get_epoch_boundary_data.
    // =============================================================================
    let epoch_timestamp_provisional: u64 = if boundary_block_from_tx > 0 {
        // Provisional: Use current time as placeholder until we get real timestamp from Go
        info!(
            "📝 [EPOCH TIMESTAMP] Will derive timestamp from boundary block {} after Go advance",
            boundary_block_from_tx
        );
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    } else {
        // Epoch 0 - use genesis timestamp (handled separately)
        info!("ℹ️ [EPOCH TIMESTAMP] Epoch 0 uses genesis timestamp from config");
        0
    };

    // NOTE: epoch_timestamp_provisional is temporary - will be replaced after Go advance
    // with definitive timestamp from get_epoch_boundary_data
    let epoch_timestamp_to_use = epoch_timestamp_provisional;

    node.system_transaction_provider
        .update_epoch(new_epoch, epoch_timestamp_to_use)
        .await;

    // CRITICAL FIX: Notify Go about epoch change BEFORE fetching committee
    // Go needs to advance its epoch state before it can return committee data for new epoch
    // Without this, fetch_committee will fail with "epoch X boundary block not stored"

    // =============================================================================
    // UNIVERSAL SYNC-AWARENESS FIX: Defer epoch advance if Go is behind
    // ALL nodes must ensure Go has ALL blocks up to boundary before advancing
    // This applies to BOTH SyncOnly AND SyncOnly→Validator transitions!
    // Otherwise, Go will record wrong boundary (current stale block instead of real boundary)
    //
    // CRITICAL: Use synced_global_exec_index (from EndOfEpoch tx / network) NOT synced_index
    //           synced_index was overwritten to go_last_block above, which defeats the purpose!
    // =============================================================================
    let required_boundary = synced_global_exec_index; // From network/EndOfEpoch tx

    // Check if Go has synced to the required boundary (applies to ALL nodes, not just SyncOnly)
    let go_current_block = executor_client.get_last_block_number().await.unwrap_or(0);

    if go_current_block < required_boundary {
        // Go hasn't synced to the boundary yet - QUEUE this transition for later
        info!(
            "📋 [DEFERRED EPOCH] Go block {} < required boundary {}. Queuing epoch {} transition.",
            go_current_block, required_boundary, new_epoch
        );

        // Queue the epoch transition for processing after sync catches up
        {
            let mut pending = node.pending_epoch_transitions.lock().await;
            pending.push(crate::node::PendingEpochTransition {
                epoch: new_epoch,
                timestamp_ms: epoch_timestamp_to_use,
                boundary_block: required_boundary,
            });
        }

        // =============================================================================
        // CRITICAL: Update SYNC-RELATED state so sync can fetch from new epoch
        // BUT do NOT update node.current_epoch!
        //
        // Why not update current_epoch?
        // - If we set current_epoch = new_epoch now, when the real transition runs later
        //   it will check "node.current_epoch >= new_epoch" and SKIP the transition!
        // - This causes consensus to NEVER start!
        //
        // We only update:
        // - last_global_exec_index (for sync to use correct epoch_base)
        // - shared_last_global_exec_index (same)
        //
        // The REAL transition (triggered by epoch_transition_sender after sync completes)
        // will update current_epoch and start consensus properly.
        // =============================================================================
        info!(
            "📋 [DEFERRED EPOCH] Updating sync state ONLY (NOT current_epoch) for epoch {} (base={})",
            new_epoch, required_boundary
        );

        // Update sync-related state ONLY (NOT current_epoch!)
        // node.current_epoch = new_epoch;  // DO NOT DO THIS!
        node.current_commit_index.store(0, Ordering::SeqCst);
        {
            let mut g = node.shared_last_global_exec_index.lock().await;
            *g = required_boundary;
        }
        node.last_global_exec_index = required_boundary;
        // node.update_execution_lock_epoch(new_epoch).await;  // Skip - don't lock for deferred epoch

        node.is_transitioning.store(false, Ordering::SeqCst);
        info!(
            "📋 [DEFERRED EPOCH] Sync state updated. Full transition queued for when Go reaches block {}",
            required_boundary
        );
        return Ok(());
    }

    info!(
        "✅ [EPOCH SYNC] Go is synced: block {} >= boundary {}. Proceeding with epoch {} advance.",
        go_current_block, required_boundary, new_epoch
    );

    info!(
        "📤 [EPOCH ADVANCE] Notifying Go about epoch {} transition (boundary: {})",
        new_epoch, required_boundary
    );
    if let Err(e) = executor_client
        .advance_epoch(new_epoch, epoch_timestamp_to_use, required_boundary)
        .await
    {
        warn!(
            "⚠️ [EPOCH ADVANCE] Failed to notify Go about epoch {}: {}. Continuing anyway...",
            new_epoch, e
        );
    }

    // =============================================================================
    // POST-TRANSITION: GET TIMESTAMP FROM GO (Block Header Derived)
    // UNIFIED TIMESTAMP: All nodes must use Go's timestamp from boundary block header
    // This ensures deterministic, identical timestamps across all nodes
    // =============================================================================
    let mut epoch_timestamp_to_use = epoch_timestamp_to_use; // Make mutable to allow update
    match executor_client.get_epoch_boundary_data(new_epoch).await {
        Ok((stored_epoch, stored_timestamp, stored_boundary, _validators)) => {
            // Validate boundary block matches what we sent
            if stored_boundary != required_boundary {
                error!(
                    "🚨 [BOUNDARY MISMATCH] Go stored boundary={} but we sent {}! Potential block skip!",
                    stored_boundary, required_boundary
                );
            } else {
                info!(
                    "✅ [CONTINUITY VERIFIED] Go confirmed epoch {} boundary: block={}, timestamp={}",
                    stored_epoch, stored_boundary, stored_timestamp
                );
            }

            // UNIFIED TIMESTAMP: Use Go's timestamp (derived from block header)
            // This replaces SystemTx timestamp to ensure all nodes use the SAME value
            if stored_timestamp > 0 {
                if stored_timestamp != epoch_timestamp_to_use {
                    info!(
                        "🔄 [UNIFIED TIMESTAMP] Updating timestamp: SystemTx={} → Go(block header)={}",
                        epoch_timestamp_to_use, stored_timestamp
                    );
                    epoch_timestamp_to_use = stored_timestamp;

                    // Also update system_transaction_provider with unified timestamp
                    node.system_transaction_provider
                        .update_epoch(new_epoch, epoch_timestamp_to_use)
                        .await;
                }
                info!(
                    "✅ [UNIFIED TIMESTAMP] Using Go's block-header-derived timestamp for epoch {}: {} ms",
                    new_epoch, epoch_timestamp_to_use
                );
            }
        }
        Err(e) => {
            // Go might not have stored it yet - this is expected for epoch 0→1
            warn!(
                "⚠️ [VALIDATION SKIP] Cannot verify boundary storage: {}. Using SystemTx timestamp as fallback.",
                e
            );
        }
    }

    // NOTE: Peer timestamp consensus is NO LONGER needed
    // All nodes process the same EndOfEpoch SystemTx → all get the same timestamp

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

    // Update epoch_eth_addresses cache with new epoch's committee
    if let Err(e) = committee_source
        .fetch_and_update_epoch_eth_addresses(
            &config.executor_send_socket_path,
            new_epoch,
            &node.epoch_eth_addresses,
        )
        .await
    {
        warn!(
            "⚠️ [TRANSITION] Failed to update epoch_eth_addresses: {}",
            e
        );
    }

    node.check_and_update_node_mode(&committee, config, true).await?;

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

        // Share epoch_eth_addresses HashMap reference for leader address lookup
        processor = processor.with_epoch_eth_addresses(node.epoch_eth_addresses.clone());

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
                    synced_index, // epoch_base_index is the synced_index (last global_exec_index of prev epoch)
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
                    Some(node.legacy_store_manager.clone()), // Pass legacy store manager to avoid RocksDB lock conflicts
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
        // SyncOnly mode: Setup CommitProcessor (for EndOfEpoch detection) but NOT Authority
        // This enables SyncOnly to detect epoch transitions from synced blocks
        info!(
            "🔄 [EPOCH TRANSITION] SyncOnly mode - setting up CommitProcessor for epoch detection"
        );

        // Setup CommitProcessor - same as Validator mode
        let (_commit_consumer, commit_receiver, mut block_receiver) = CommitConsumerArgs::new(0, 0);
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
                Some(node.storage_path.clone()),
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

        // Share epoch_eth_addresses HashMap reference for leader address lookup
        processor = processor.with_epoch_eth_addresses(node.epoch_eth_addresses.clone());

        if let Some(c) = exec_client_proc {
            processor = processor.with_executor_client(c);
        }

        // Note: commit_consumer receiver is passed to CommitProcessor, keeping channel open

        tokio::spawn(async move {
            let _ = processor.run().await;
        });
        tokio::spawn(async move { while block_receiver.recv().await.is_some() {} });

        // Clear authority and proxy (SyncOnly doesn't run consensus)
        node.authority = None;
        node.transaction_client_proxy = None;

        // CRITICAL FIX: Stop old sync task FIRST to prevent stale committee from blocking fetch
        // Old RustSyncNode keeps running with old epoch's committee, causing silent fetch failures
        // after epoch transition. We must stop it before starting new one with fresh committee.
        if node.sync_task_handle.is_some() {
            info!("🛑 [SYNC ONLY] Stopping old sync task before starting new one with fresh committee");
            if let Err(e) = crate::node::sync::stop_sync_task(node).await {
                warn!(
                    "⚠️ [SYNC ONLY] Failed to stop old sync task: {}. Continuing anyway.",
                    e
                );
            }
        }

        // Start sync task to receive blocks from peers via Rust network
        // The sync task will feed blocks to CommitProcessor for EndOfEpoch detection
        info!(
            "🔄 [SYNC ONLY] Starting Rust P2P sync task with fresh committee for epoch {}",
            new_epoch
        );

        // Use RustSyncNode for full Rust P2P sync
        // CRITICAL FIX: SyncOnly nodes need can_commit=true to send synced blocks to their local Go
        // Without this, Go stays stuck at old block number and never syncs up
        let rust_sync_executor = Arc::new(ExecutorClient::new(
            true,
            true, // SyncOnly nodes must commit synced blocks to local Go
            config.executor_send_socket_path.clone(),
            config.executor_receive_socket_path.clone(),
            None,
        ));

        // CRITICAL: Initialize ExecutorClient from Go's current state SYNCHRONOUSLY
        // This syncs next_expected_index with Go's last_block_number + 1
        // MUST await here, NOT spawn! Otherwise races with start_rust_sync_task_with_network
        rust_sync_executor.initialize_from_go().await;

        // Create Context for P2P networking
        // For SyncOnly nodes, we use a dummy own_index (0) since we don't participate in consensus
        let sync_metrics = consensus_core::initialise_metrics(Registry::new());
        let sync_context = std::sync::Arc::new(consensus_core::Context::new(
            epoch_timestamp_to_use,
            consensus_config::AuthorityIndex::new_for_test(0), // SyncOnly uses dummy index
            committee.clone(),
            node.parameters.clone(),
            node.protocol_config.clone(),
            sync_metrics,
            node.clock.clone(),
        ));

        match crate::node::rust_sync_node::start_rust_sync_task_with_network(
            rust_sync_executor,
            node.epoch_transition_sender.clone(),
            new_epoch,
            0, // initial_commit_index
            sync_context,
            node.network_keypair.clone(),
            committee.clone(),
            config.peer_rpc_addresses.clone(), // For epoch boundary data fallback
        )
        .await
        {
            Ok(handle) => {
                node.sync_task_handle = Some(handle);
                info!("✅ [SYNC ONLY] Rust P2P sync started with full networking");
            }
            Err(e) => {
                warn!("⚠️ [SYNC ONLY] Failed to start Rust P2P sync: {}", e);
            }
        }
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

    // NOTE: advance_epoch was already called at line 340 with correct boundary.
    // No need to call again here (was causing unnecessary duplicate RPC call).

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
        epoch_timestamp_to_use, // Use provisional timestamp
    )
    .await
    {
        Ok(timestamp) => {
            if timestamp != epoch_timestamp_to_use {
                warn!(
                    "⚠️ [EPOCH TIMESTAMP SYNC] Timestamp mismatch after transition: \
                     Local provisional: {}ms, Go reported: {}ms, diff: {}ms. \
                     Using Go's timestamp to prevent fork.",
                    epoch_timestamp_to_use,
                    timestamp,
                    (timestamp as i64 - epoch_timestamp_to_use as i64).abs()
                );
                timestamp
            } else {
                info!(
                    "✅ [EPOCH TIMESTAMP SYNC] Timestamp consistent between local and Go: {}ms",
                    epoch_timestamp_to_use
                );
                timestamp
            }
        }
        Err(e) => {
            // Check if this is a "not implemented" error (endpoint missing)
            if e.to_string().contains("not found") || e.to_string().contains("Unexpected response")
            {
                info!("ℹ️ [EPOCH TIMESTAMP SYNC] Go endpoint not implemented yet, using local calculation: {}ms", epoch_timestamp_to_use);
            } else {
                warn!("⚠️ [EPOCH TIMESTAMP SYNC] Failed to sync timestamp from Go: {}. Using local calculation.", e);
            }
            epoch_timestamp_to_use
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

    // =========================================================================
    // CROSS-EPOCH TRANSITION SUMMARY
    // Log final state for debugging mode transitions
    // =========================================================================
    info!(
        "✅ [EPOCH TRANSITION COMPLETE] epoch={}, mode={:?}, last_global_exec_index={}, go_sync_complete={}",
        node.current_epoch,
        node.node_mode,
        node.last_global_exec_index,
        executor_client.get_last_block_number().await.unwrap_or(0) >= node.last_global_exec_index
    );

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

/// MODE-ONLY TRANSITION: SyncOnly → Validator within the SAME epoch
/// This happens when a node joins the committee mid-epoch (e.g., added to committee after epoch started)
/// Unlike full epoch transition, this:
/// - Does NOT recreate DB (uses existing epoch DB)
/// - Does NOT wait for commit_processor sync
/// - Just starts the authority components
pub async fn transition_mode_only(
    node: &mut ConsensusNode,
    epoch: u64,
    _boundary_block_unused: u64, // INTENTIONALLY UNUSED: Timestamp is fetched from Go
    synced_global_exec_index: u64,
    config: &NodeConfig,
) -> Result<()> {
    // Guard against concurrent transitions
    if node.is_transitioning.swap(true, Ordering::SeqCst) {
        warn!("⚠️ Mode transition already in progress, skipping.");
        node.is_transitioning.store(false, Ordering::SeqCst);
        return Ok(());
    }

    struct Guard(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Guard {
        fn drop(&mut self) {
            if self.0.load(Ordering::SeqCst) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
    }
    let _guard = Guard(node.is_transitioning.clone());

    info!(
        "🔄 [MODE TRANSITION] Starting SyncOnly → Validator for epoch {} (no DB recreation)",
        epoch
    );

    // Use existing epoch DB path
    let db_path = node
        .storage_path
        .join("epochs")
        .join(format!("epoch_{}", epoch))
        .join("consensus_db");

    // Create if doesn't exist (shouldn't happen but be safe)
    if !db_path.exists() {
        std::fs::create_dir_all(&db_path)?;
        warn!(
            "⚠️ [MODE TRANSITION] DB path didn't exist, created: {:?}",
            db_path
        );
    }

    // Fetch committee using same pattern as epoch_monitor
    let committee_source = crate::node::committee_source::CommitteeSource::discover(config).await?;

    // =============================================================================
    // UNIFIED TIMESTAMP APPROACH (FORK-SAFE)
    // =============================================================================
    // Use fetch_committee_with_timestamp to get BOTH committee AND timestamp from Go.
    // Go derives timestamp deterministically:
    // - Epoch 0: Genesis timestamp from genesis.json
    // - Epoch N: boundaryBlock.Header().TimeStamp() * 1000
    //
    // This REPLACES the epoch_timestamp_ms parameter - we IGNORE what was passed in
    // and use Go's authoritative value instead. This ensures ALL nodes use the same
    // timestamp even if EndOfEpoch SystemTx had different precision.
    // =============================================================================
    let (committee, go_authoritative_timestamp) = committee_source
        .fetch_committee_with_timestamp(&config.executor_send_socket_path, epoch)
        .await?;

    // Update epoch_eth_addresses cache with new epoch's committee
    // CRITICAL: This is needed for leader address resolution in CommitProcessor
    if let Err(e) = committee_source
        .fetch_and_update_epoch_eth_addresses(
            &config.executor_send_socket_path,
            epoch,
            &node.epoch_eth_addresses,
        )
        .await
    {
        warn!(
            "⚠️ [MODE TRANSITION] Failed to update epoch_eth_addresses: {}",
            e
        );
    }

    info!(
        "✅ [MODE TRANSITION] Got UNIFIED committee+timestamp from Go: epoch={}, timestamp={} ms",
        epoch, go_authoritative_timestamp
    );

    // Update node mode (this also handles Go handoff)
    node.check_and_update_node_mode(&committee, config, true).await?;

    // Find our index in committee
    let own_protocol_pubkey = node.protocol_keypair.public();
    if let Some((idx, _)) = committee
        .authorities()
        .find(|(_, a)| a.protocol_key == own_protocol_pubkey)
    {
        node.own_index = idx;
        info!(
            "✅ [MODE TRANSITION] Found self in committee at index {}",
            idx
        );
    } else {
        // NOT in committee - stay in SyncOnly mode but update epoch to continue syncing
        warn!(
            "⚠️ [MODE TRANSITION] Not found in committee - staying in SyncOnly mode for epoch {}",
            epoch
        );

        // CRITICAL FIX: Update epoch state even when not in committee
        // Otherwise sync task will keep trying to transition to the same epoch
        node.current_epoch = epoch;
        node.last_global_exec_index = synced_global_exec_index;

        // IMPORTANT: Stop old sync task first, otherwise new task won't start
        // (start_sync_task returns early if sync_task_handle.is_some())
        // This ensures new sync task gets updated epoch from node.current_epoch
        info!("🔄 [MODE TRANSITION] Stopping old sync task before restart...");
        crate::node::sync::stop_sync_task(node).await?;

        info!(
            "🔄 [MODE TRANSITION] Starting new sync task for SyncOnly mode in epoch {}",
            epoch
        );
        crate::node::sync::start_sync_task(node, config).await?;

        return Ok(());
    }

    // Update epoch state
    node.current_epoch = epoch;
    node.last_global_exec_index = synced_global_exec_index;
    // Note: shared_last_global_exec_index is Arc<Mutex<u64>>, updated via commit_processor
    node.current_commit_index.store(0, Ordering::SeqCst);

    // =============================================================================
    // UNIFIED TIMESTAMP (FORK-SAFE) - 2026-02-04
    // =============================================================================
    // We now use go_authoritative_timestamp from fetch_committee_with_timestamp().
    // This timestamp comes from Go's get_epoch_boundary_data():
    // - Epoch 0: Genesis timestamp from genesis.json
    // - Epoch N: boundaryBlock.Header().TimeStamp() * 1000
    //
    // This IGNORES the boundary_block parameter (from EndOfEpoch SystemTx).
    // Timestamp is fetched directly from Go's get_epoch_boundary_data.
    // By using Go's derivation, ALL nodes get IDENTICAL timestamp = NO FORK!
    //
    // Note: _boundary_block_unused parameter is prefixed with _ to suppress unused warning
    // =============================================================================
    let epoch_timestamp_to_use = go_authoritative_timestamp;

    info!(
        "✅ [MODE TRANSITION] Using UNIFIED timestamp={} ms from Go boundary block (ignoring EndOfEpoch tx timestamp)",
        epoch_timestamp_to_use
    );

    // Now setup authority components (same as in full transition)
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
            synced_global_exec_index + 1,
            Some(node.storage_path.clone()),
        )))
    } else {
        None
    };

    let mut processor = crate::consensus::commit_processor::CommitProcessor::new(commit_receiver)
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
        .with_epoch_info(epoch, synced_global_exec_index)
        .with_is_transitioning(node.is_transitioning.clone())
        .with_pending_transactions_queue(node.pending_transactions_queue.clone())
        .with_epoch_transition_callback(epoch_cb);

    // Share epoch_eth_addresses HashMap reference for leader address lookup
    processor = processor.with_epoch_eth_addresses(node.epoch_eth_addresses.clone());

    if let Some(c) = exec_client_proc {
        processor = processor.with_executor_client(c);
    }

    tokio::spawn(async move {
        let _ = processor.run().await;
    });
    tokio::spawn(async move { while block_receiver.recv().await.is_some() {} });

    // =======================================================================
    // CENTRALIZED CLEANUP: Ensure sync task is fully stopped before Authority
    // check_and_update_node_mode should have stopped it, but verify just in case
    // =======================================================================
    if node.sync_task_handle.is_some() {
        warn!("⚠️ [MODE TRANSITION] Sync task still running - stopping explicitly before Authority start");
        crate::node::sync::stop_sync_task(node).await?;
    }

    // Start Authority
    let mut params = node.parameters.clone();
    params.db_path = db_path;
    node.boot_counter += 1;

    info!("🚀 [MODE TRANSITION] Starting ConsensusAuthority for Validator mode");
    node.authority = Some(
        ConsensusAuthority::start(
            NetworkType::Tonic,
            epoch_timestamp_to_use,
            synced_global_exec_index,
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
            Some(node.system_transaction_provider.clone() as Arc<dyn SystemTransactionProvider>),
            Some(node.legacy_store_manager.clone()), // Pass legacy store manager to avoid RocksDB lock conflicts
        )
        .await,
    );

    // Note: proxy update is handled by check_and_update_node_mode

    info!(
        "✅ [MODE TRANSITION] Successfully transitioned to Validator mode for epoch {}",
        epoch
    );

    Ok(())
}

/// CROSS-EPOCH DEMOTION: Validator → SyncOnly with epoch catch-up
///
/// This handles the case where a Validator node:
/// 1. Gets removed from committee at epoch N
/// 2. Needs to demote to SyncOnly AND catch up to current network epoch
///
/// The function:
/// 1. Gracefully stops the authority
/// 2. Waits for Go to sync up
/// 3. Switches mode to SyncOnly
/// 4. Starts sync task to catch up to network
///
/// This is called by check_and_update_node_mode when transitioning Validator→SyncOnly
/// but the existing logic already handles this well. This function provides additional
/// handling for cases where we need to catch up multiple epochs after demotion.
#[allow(dead_code)]
pub async fn demote_to_synconly_and_catchup(
    node: &mut ConsensusNode,
    target_epoch: u64,
    target_block: u64,
    config: &NodeConfig,
) -> Result<()> {
    // Guard against concurrent transitions
    if node.is_transitioning.swap(true, Ordering::SeqCst) {
        warn!("⚠️ Demotion already in progress, skipping.");
        node.is_transitioning.store(false, Ordering::SeqCst);
        return Ok(());
    }

    info!(
        "🔄 [CROSS-EPOCH DEMOTION] Starting Validator → SyncOnly: current_epoch={}, target_epoch={}, target_block={}",
        node.current_epoch, target_epoch, target_block
    );

    // STEP 1: Stop authority gracefully
    if let Some(auth) = node.authority.take() {
        info!("🛑 [DEMOTION] Stopping consensus authority...");
        auth.stop().await;
        info!("✅ [DEMOTION] Authority stopped successfully");
    }

    // STEP 2: Wait for Go to catch up to our last block
    let expected_last_block = node.last_global_exec_index;
    if let Some(ref executor_client) = node.executor_client {
        info!(
            "⏳ [DEMOTION] Waiting for Go to reach block {}...",
            expected_last_block
        );

        let poll_interval = Duration::from_millis(100);
        let mut attempt = 0u64;
        let max_attempts = 6000; // 10 minutes max

        loop {
            attempt += 1;
            match executor_client.get_last_block_number().await {
                Ok(go_last_block) => {
                    if go_last_block >= expected_last_block {
                        info!(
                            "✅ [DEMOTION] Go reached block {} (expected: {}) after {} polls",
                            go_last_block, expected_last_block, attempt
                        );
                        break;
                    }
                    if attempt % 100 == 0 {
                        info!(
                            "⏳ [DEMOTION] Go: {}/{} blocks (waiting {}s)",
                            go_last_block,
                            expected_last_block,
                            attempt / 10
                        );
                    }
                }
                Err(e) => {
                    if attempt % 100 == 0 {
                        warn!("⚠️ [DEMOTION] Cannot reach Go: {}. Retrying...", e);
                    }
                }
            }

            if attempt >= max_attempts {
                warn!(
                    "⚠️ [DEMOTION] Timeout waiting for Go sync after {} attempts. Proceeding anyway.",
                    attempt
                );
                break;
            }

            sleep(poll_interval).await;
        }
    }

    // STEP 3: Update mode to SyncOnly
    node.node_mode = NodeMode::SyncOnly;
    node.current_epoch = target_epoch;
    node.last_global_exec_index = target_block;

    info!(
        "📊 [DEMOTION] Mode updated: SyncOnly, epoch={}, last_global_exec_index={}",
        node.current_epoch, node.last_global_exec_index
    );

    // STEP 4: Notify Go of sync mode
    if let Some(ref executor_client) = node.executor_client {
        let _ = executor_client.set_sync_start_block(target_block).await;
    }

    // STEP 5: Start sync task
    info!("🔄 [DEMOTION] Starting sync task for catch-up...");
    if let Err(e) = crate::node::sync::start_sync_task(node, config).await {
        warn!("⚠️ [DEMOTION] Failed to start sync task: {}", e);
    }

    // STEP 6: Start epoch monitor for future transitions
    if let Ok(Some(handle)) =
        crate::node::epoch_monitor::start_unified_epoch_monitor(&node.executor_client, config)
    {
        info!("🔄 [DEMOTION] Started epoch monitor for future transitions");
        node.epoch_monitor_handle = Some(handle);
    }

    node.is_transitioning.store(false, Ordering::SeqCst);

    info!(
        "✅ [CROSS-EPOCH DEMOTION] Successfully demoted to SyncOnly at epoch {}",
        target_epoch
    );

    Ok(())
}

/// Check if node should be promoted from SyncOnly to Validator
///
/// This is a helper function that can be called after sync catches up
/// to verify if promotion is appropriate.
#[allow(dead_code)]
pub async fn check_promotion_eligibility(
    node: &ConsensusNode,
    config: &NodeConfig,
) -> Result<Option<u64>> {
    // Only SyncOnly nodes can be promoted
    if !matches!(node.node_mode, NodeMode::SyncOnly) {
        return Ok(None);
    }

    // Fetch current network epoch and committee
    let committee_source = crate::node::committee_source::CommitteeSource::discover(config).await?;
    let network_epoch = committee_source.epoch;

    // Check if we're at the network epoch
    if node.current_epoch < network_epoch {
        info!(
            "📊 [PROMOTION CHECK] Node epoch {} < network epoch {}. Need to catch up first.",
            node.current_epoch, network_epoch
        );
        return Ok(None);
    }

    // Fetch committee for current epoch
    let committee = committee_source
        .fetch_committee(&config.executor_send_socket_path, network_epoch)
        .await?;

    // Check if we're in the committee
    let own_protocol_pubkey = node.protocol_keypair.public();
    let in_committee = committee
        .authorities()
        .any(|(_, authority)| authority.protocol_key == own_protocol_pubkey);

    if in_committee {
        info!(
            "✅ [PROMOTION CHECK] Node IS in committee for epoch {}. Eligible for promotion!",
            network_epoch
        );
        Ok(Some(network_epoch))
    } else {
        info!(
            "ℹ️ [PROMOTION CHECK] Node NOT in committee for epoch {}. Staying SyncOnly.",
            network_epoch
        );
        Ok(None)
    }
}

