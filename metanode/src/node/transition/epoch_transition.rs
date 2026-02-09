// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

//! Full epoch transitions (epoch N → N+1).

use crate::config::NodeConfig;
use crate::node::executor_client::ExecutorClient;
use crate::node::{ConsensusNode, NodeMode};
use anyhow::Result;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

use super::consensus_setup::{setup_synconly_sync, setup_validator_consensus};
use super::demotion::determine_role_and_check_transition;
use super::mode_transition::transition_mode_only;
use super::tx_recovery::recover_epoch_pending_transactions;
use super::verification::{
    verify_epoch_consistency, wait_for_commit_processor_completion, wait_for_consensus_ready,
};

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
        return super::mode_transition::handle_synconly_upgrade_wait(
            node,
            new_epoch,
            boundary_block_from_tx,
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

    // =============================================================================
    // FIX 2026-02-06: Call advance_epoch on Go FIRST, before fetching committee!
    //
    // PROBLEM: determine_role_and_check_transition() calls fetch_committee() which
    // waits for Go to have epoch N data. But Go doesn't have it because advance_epoch
    // was only called AFTER this check (line 558). Circular dependency = deadlock!
    //
    // SOLUTION: Call advance_epoch() FIRST, so Go stores the epoch boundary data.
    // Then fetch_committee can succeed.
    // =============================================================================

    // Initialize checkpoint manager for crash recovery
    let checkpoint_manager = crate::node::epoch_checkpoint::CheckpointManager::new(
        &config.storage_path,
        &format!("node-{}", config.node_id),
    );

    // Check for incomplete transition from previous crash
    if let Ok(Some(incomplete)) = checkpoint_manager.get_incomplete_transition().await {
        info!(
            "🔄 [CHECKPOINT] Found incomplete transition: state={}, epoch={:?}",
            incomplete.state.name(),
            incomplete.state.epoch()
        );
        // For now, just log and continue - future: implement resume logic
    }

    {
        info!(
            "📤 [ADVANCE EPOCH FIRST] Notifying Go about epoch {} BEFORE fetching committee (boundary: {}, synced: {})",
            new_epoch, boundary_block_from_tx, synced_global_exec_index
        );

        let early_executor_client = crate::node::executor_client::ExecutorClient::new(
            true,
            false,
            config.executor_send_socket_path.clone(),
            config.executor_receive_socket_path.clone(),
            None,
        );

        // Use synced_global_exec_index as the boundary (this is when the EndOfEpoch tx was committed)
        // Timestamp is provisional (current time) - Go will derive real timestamp from block header
        let provisional_timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        match early_executor_client
            .advance_epoch(new_epoch, provisional_timestamp, synced_global_exec_index)
            .await
        {
            Ok(_) => {
                info!(
                    "✅ [ADVANCE EPOCH FIRST] Go notified about epoch {} (boundary={}). Committee fetch should now work.",
                    new_epoch, synced_global_exec_index
                );

                // Save checkpoint: Go has been notified
                if let Err(e) = checkpoint_manager
                    .checkpoint_advance_epoch(
                        new_epoch,
                        synced_global_exec_index,
                        provisional_timestamp,
                    )
                    .await
                {
                    warn!("⚠️ Failed to save checkpoint: {}", e);
                }
            }
            Err(e) => {
                // Go now accepts advance_epoch even when sync is incomplete
                // So this error path is for other unexpected errors
                warn!(
                    "⚠️ [ADVANCE EPOCH FIRST] Failed to notify Go about epoch {}: {}. Continuing anyway.",
                    new_epoch, e
                );
                // Continue to fetch_committee - Go may still work
            }
        }
    }

    // =============================================================================
    // STEP 0: ROLE-FIRST CHECK (MUST BE FIRST!)
    // =============================================================================
    // Determine node's role for the new epoch BEFORE any other operations.
    // This ensures we know whether to setup Validator or SyncOnly infrastructure.
    // NOTE: Go now has epoch boundary data from advance_epoch call above.
    // =============================================================================
    let own_protocol_pubkey = node.protocol_keypair.public();
    let (target_role, needs_mode_change) = determine_role_and_check_transition(
        new_epoch,
        &node.node_mode,
        &own_protocol_pubkey,
        config,
    )
    .await?;

    info!(
        "📋 [ROLE-FIRST] Epoch {} transition: target_role={:?}, needs_mode_change={}, current_mode={:?}",
        new_epoch, target_role, needs_mode_change, node.node_mode
    );

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

    let synced_index = stop_authority_and_poll_go(
        node,
        new_epoch,
        &executor_client,
        &committee_source,
        calculated_last_block,
    )
    .await?;

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
            .unwrap_or_default()
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
        return handle_deferred_epoch_transition(
            node,
            new_epoch,
            epoch_timestamp_to_use,
            required_boundary,
            go_current_block,
        )
        .await;
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
                                                             // Store epoch boundary block for later use in Validator Priority check
    let mut epoch_boundary_block: u64 = synced_index; // Default to synced_index if get_epoch_boundary_data fails
    match executor_client.get_epoch_boundary_data(new_epoch).await {
        Ok((stored_epoch, stored_timestamp, stored_boundary, _validators)) => {
            // Save the authoritative epoch boundary block
            epoch_boundary_block = stored_boundary;
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

    // Clone committee for later use in Validator Priority check
    // (original committee will be moved into ConsensusAuthority::start())
    let committee_for_priority_check = committee.clone();

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

    node.check_and_update_node_mode(&committee, config, true)
        .await?;

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

    // Setup consensus components based on mode
    if matches!(node.node_mode, NodeMode::Validator) {
        setup_validator_consensus(
            node,
            new_epoch,
            epoch_boundary_block,
            epoch_timestamp_to_use,
            db_path,
            committee,
            config,
        )
        .await?;
    } else {
        setup_synconly_sync(
            node,
            new_epoch,
            epoch_boundary_block,
            epoch_timestamp_to_use,
            committee,
            config,
        )
        .await?;
    }

    // Wait for consensus to stabilize with proper synchronization instead of fixed sleep
    if wait_for_consensus_ready(node).await {
        info!("✅ Consensus ready.");
    }

    // Recover transactions from previous epoch that were not committed
    let _ = recover_epoch_pending_transactions(node).await;

    node.is_transitioning.store(false, Ordering::SeqCst);
    let _ = node.submit_queued_transactions().await;

    // =========================================================================
    // VALIDATOR PRIORITY FIX: After SyncOnly setup, re-check if we should be Validator
    //
    // Problem: When a SyncOnly node transitions epochs, check_and_update_node_mode()
    // is called BEFORE committee is fetched for the new epoch. If the new epoch's
    // committee includes this node, we must upgrade to Validator.
    //
    // Solution: After SyncOnly mode is established and sync starts, re-check
    // committee membership. If we're now in the committee, trigger mode upgrade.
    // =========================================================================
    if matches!(node.node_mode, NodeMode::SyncOnly) {
        let own_protocol_pubkey = node.protocol_keypair.public();
        let is_now_in_committee = committee_for_priority_check
            .authorities()
            .any(|(_, authority)| authority.protocol_key == own_protocol_pubkey);

        if is_now_in_committee {
            info!(
                "🚀 [VALIDATOR PRIORITY] SyncOnly node IS in committee for epoch {}! Triggering upgrade.",
                new_epoch
            );

            // Trigger upgrade: SyncOnly → Validator
            // Use synced_index as boundary for the mode-only transition
            // Use epoch_boundary_block (from get_epoch_boundary_data) instead of synced_index
            // This ensures we use the correct epoch boundary, not just the last committed block
            let _upgrade_result = transition_mode_only(
                node,
                new_epoch,
                epoch_boundary_block, // boundary_block from get_epoch_boundary_data
                epoch_boundary_block, // synced_global_exec_index
                config,
            )
            .await;

            info!(
                "✅ [VALIDATOR PRIORITY] Mode upgrade complete: now {:?}",
                node.node_mode
            );
        } else {
            info!(
                "ℹ️ [VALIDATOR PRIORITY] Node NOT in committee for epoch {}. Staying SyncOnly.",
                new_epoch
            );
        }
    }

    node.reset_reconfig_state().await;

    // Post-transition verification: epoch consistency + timestamp sync
    verify_epoch_consistency(node, new_epoch, epoch_timestamp_to_use, &executor_client).await?;

    Ok(())
}

/// Stop old authority, preserve store, and poll Go until it has all old-epoch blocks.
/// Returns the verified synced_index from Go.
pub(super) async fn stop_authority_and_poll_go(
    node: &mut ConsensusNode,
    new_epoch: u64,
    executor_client: &ExecutorClient,
    committee_source: &crate::node::committee_source::CommitteeSource,
    calculated_last_block: u64,
) -> Result<u64> {
    info!("🛑 [TRANSITION] Stopping old authority BEFORE fetching synced_index...");

    let expected_last_block = {
        let shared_index = node.shared_last_global_exec_index.lock().await;
        *shared_index
    };
    info!(
        "📊 [TRANSITION] Expected last block after old epoch: {}",
        expected_last_block
    );

    // Extract store and stop old authority
    if let Some(auth) = node.authority.take() {
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

    // STRICT SEQUENTIAL GUARANTEE: Poll Go until it confirms receiving expected_last_block
    let poll_interval = Duration::from_millis(100);
    let mut attempt = 0u64;

    loop {
        attempt += 1;
        match executor_client.get_last_block_number().await {
            Ok(last_block) => {
                if last_block >= expected_last_block {
                    info!(
                        "✅ [SYNC VERIFIED] Go confirmed receiving all blocks: go_last={} >= expected={} (took {} attempts)",
                        last_block, expected_last_block, attempt
                    );
                    break;
                } else if attempt % 100 == 0 {
                    warn!(
                        "⏳ [SYNC WAIT] Waiting for Go to catch up: go_last={}, expected={} (waiting for {}s)",
                        last_block, expected_last_block, attempt / 10
                    );
                }
            }
            Err(e) => {
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

    // Fetch final synced_index from Go
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
    Ok(synced_index)
}

/// Handle deferred epoch transition when Go hasn't synced to the required boundary yet.
/// Updates sync-related state (NOT current_epoch!) and queues the transition.
pub(super) async fn handle_deferred_epoch_transition(
    node: &mut ConsensusNode,
    new_epoch: u64,
    epoch_timestamp: u64,
    required_boundary: u64,
    go_current_block: u64,
) -> Result<()> {
    info!(
        "📋 [DEFERRED EPOCH] Go block {} < required boundary {}. Queuing epoch {} transition.",
        go_current_block, required_boundary, new_epoch
    );

    {
        let mut pending = node.pending_epoch_transitions.lock().await;
        pending.push(crate::node::PendingEpochTransition {
            epoch: new_epoch,
            timestamp_ms: epoch_timestamp,
            boundary_block: required_boundary,
        });
    }

    // CRITICAL: Update SYNC-RELATED state so sync can fetch from new epoch
    // BUT do NOT update node.current_epoch!
    // If we set current_epoch = new_epoch now, the real transition will SKIP
    // because it checks "current_epoch >= new_epoch"
    info!(
        "📋 [DEFERRED EPOCH] Updating sync state ONLY (NOT current_epoch) for epoch {} (base={})",
        new_epoch, required_boundary
    );

    node.current_commit_index.store(0, Ordering::SeqCst);
    {
        let mut g = node.shared_last_global_exec_index.lock().await;
        *g = required_boundary;
    }
    node.last_global_exec_index = required_boundary;

    node.is_transitioning.store(false, Ordering::SeqCst);
    info!(
        "📋 [DEFERRED EPOCH] Sync state updated. Full transition queued for when Go reaches block {}",
        required_boundary
    );
    Ok(())
}
