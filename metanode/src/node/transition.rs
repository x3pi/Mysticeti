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
    let is_sync_only = matches!(node.node_mode, NodeMode::SyncOnly);
    let is_same_epoch = node.current_epoch == new_epoch;

    // CASE 1: Same epoch, but SyncOnly needs to become Validator
    // This is a MODE-ONLY transition - skip full epoch transition, just start authority
    if is_same_epoch && is_sync_only {
        info!(
            "🔄 [MODE TRANSITION] SyncOnly → Validator in epoch {} (not a full epoch transition)",
            new_epoch
        );
        return transition_mode_only(
            node,
            new_epoch,
            new_epoch_timestamp_ms,
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
    let timeout_secs = if config.epoch_transition_optimization == "fast" {
        5
    } else {
        10
    };
    let _ = wait_for_commit_processor_completion(node, target_commit_index, timeout_secs).await;

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
    // CRITICAL: Use timestamp from EndOfEpoch SYSTEM TRANSACTION as single source of truth
    // This ensures ALL nodes use the SAME timestamp since they all process the same EndOfEpoch tx
    // CommitteeSource/Go timestamps are NO LONGER authoritative - they caused fork issues!
    // =============================================================================
    let epoch_timestamp_to_use = new_epoch_timestamp_ms; // FROM SYSTEM TX - AUTHORITATIVE

    // Log CommitteeSource timestamp for debugging only (NOT used as source of truth)
    let source_timestamp = committee_source.get_epoch_timestamp();
    if source_timestamp != new_epoch_timestamp_ms && source_timestamp > 0 {
        warn!(
            "🔍 [TIMESTAMP INFO] CommitteeSource has different timestamp: source={}, system_tx={}. Using SYSTEM TX value (authoritative).",
            source_timestamp, new_epoch_timestamp_ms
        );
    }

    info!(
        "✅ [EPOCH TIMESTAMP] Using timestamp from EndOfEpoch SystemTx for epoch {}: {} ms",
        new_epoch, epoch_timestamp_to_use
    );

    node.system_transaction_provider
        .update_epoch(new_epoch, epoch_timestamp_to_use)
        .await;

    // CRITICAL FIX: Notify Go about epoch change BEFORE fetching committee
    // Go needs to advance its epoch state before it can return committee data for new epoch
    // Without this, fetch_committee will fail with "epoch X boundary block not stored"

    // =============================================================================
    // SYNCONLY SYNC-AWARENESS FIX: Defer epoch advance if Go is behind
    // For SyncOnly nodes, we must ensure Go has ALL blocks up to boundary before advancing
    // Otherwise, Go will record wrong boundary (current stale block instead of real boundary)
    //
    // CRITICAL: Use synced_global_exec_index (from EndOfEpoch tx / network) NOT synced_index
    //           synced_index was overwritten to go_last_block above, which defeats the purpose!
    // =============================================================================
    let required_boundary = synced_global_exec_index; // From network/EndOfEpoch tx

    if is_sync_only {
        let go_current_block = executor_client.get_last_block_number().await.unwrap_or(0);

        if go_current_block < required_boundary {
            // Go hasn't synced to the boundary yet - QUEUE this transition for later
            info!(
                "📋 [DEFERRED EPOCH] SyncOnly: Go block {} < required boundary {}. Queuing epoch {} transition.",
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
            // CRITICAL FIX: Still update RUST state so sync can fetch from new epoch!
            // Peers have already advanced to new epoch and purged old epoch data.
            // Rust needs to know to fetch from new epoch, but Go advance is deferred.
            // =============================================================================
            info!(
                "📋 [DEFERRED EPOCH] Updating Rust state to epoch {} (base={}) while deferring Go advance",
                new_epoch, required_boundary
            );

            // Update Rust epoch state (but NOT calling Go advance_epoch)
            node.current_epoch = new_epoch;
            node.current_commit_index.store(0, Ordering::SeqCst);
            {
                let mut g = node.shared_last_global_exec_index.lock().await;
                *g = required_boundary;
            }
            node.last_global_exec_index = required_boundary;
            node.update_execution_lock_epoch(new_epoch).await;

            // Note: Not starting new consensus here - sync task will fetch using existing network
            // The epoch/epoch_base update above is sufficient for sync to work

            node.is_transitioning.store(false, Ordering::SeqCst);
            info!(
                "📋 [DEFERRED EPOCH] Rust state updated. Go advance queued for when sync reaches block {}",
                required_boundary
            );
            return Ok(());
        } else {
            info!(
                "✅ [SYNCONLY EPOCH] Go is synced: block {} >= boundary {}. Proceeding with epoch {} advance.",
                go_current_block, required_boundary, new_epoch
            );
        }
    }

    info!(
        "📤 [EPOCH ADVANCE] Notifying Go about epoch {} transition (boundary: {})",
        new_epoch, synced_index
    );
    if let Err(e) = executor_client
        .advance_epoch(new_epoch, epoch_timestamp_to_use, synced_index)
        .await
    {
        warn!(
            "⚠️ [EPOCH ADVANCE] Failed to notify Go about epoch {}: {}. Continuing anyway...",
            new_epoch, e
        );
    }

    // =============================================================================
    // POST-TRANSITION VALIDATION: Verify Go stored the boundary correctly
    // NOTE: Go timestamps are NO LONGER authoritative - only log differences
    // =============================================================================
    match executor_client.get_epoch_boundary_data(new_epoch).await {
        Ok((stored_epoch, stored_timestamp, stored_boundary, _validators)) => {
            // Validate boundary block matches what we sent
            if stored_boundary != synced_index {
                error!(
                    "🚨 [BOUNDARY MISMATCH] Go stored boundary={} but we sent {}! Potential block skip!",
                    stored_boundary, synced_index
                );
            } else {
                info!(
                    "✅ [CONTINUITY VERIFIED] Go confirmed epoch {} boundary: block={}, timestamp={}",
                    stored_epoch, stored_boundary, stored_timestamp
                );
            }

            // Log timestamp difference (but DO NOT update epoch_timestamp_to_use - SystemTx is authoritative)
            if stored_timestamp != epoch_timestamp_to_use && stored_timestamp > 0 {
                warn!(
                    "🔍 [TIMESTAMP INFO] Go has timestamp={}, SystemTx has {}. Using SystemTx (authoritative).",
                    stored_timestamp, epoch_timestamp_to_use
                );
            }
        }
        Err(e) => {
            // Go might not have stored it yet - this is expected for epoch 0→1
            warn!(
                "⚠️ [VALIDATION SKIP] Cannot verify boundary storage: {}. Continuing with SystemTx timestamp.",
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

/// MODE-ONLY TRANSITION: SyncOnly → Validator within the SAME epoch
/// This happens when a node joins the committee mid-epoch (e.g., added to committee after epoch started)
/// Unlike full epoch transition, this:
/// - Does NOT recreate DB (uses existing epoch DB)
/// - Does NOT wait for commit_processor sync
/// - Just starts the authority components
pub async fn transition_mode_only(
    node: &mut ConsensusNode,
    epoch: u64,
    epoch_timestamp_ms: u64,
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

    let committee = committee_source
        .fetch_committee(&config.executor_send_socket_path, epoch)
        .await?;

    // Update node mode (this also handles Go handoff)
    node.check_and_update_node_mode(&committee, config).await?;

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

    // Use verified timestamp from CommitteeSource or fallback to passed value
    let verified_epoch_timestamp_ms = committee_source.epoch_timestamp_ms;
    let epoch_timestamp_to_use = if verified_epoch_timestamp_ms > 0 {
        verified_epoch_timestamp_ms
    } else {
        epoch_timestamp_ms
    };

    info!(
        "✅ [MODE TRANSITION] Using epoch_timestamp={} ms (verified={})",
        epoch_timestamp_to_use,
        verified_epoch_timestamp_ms > 0
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

    if let Some(c) = exec_client_proc {
        processor = processor.with_executor_client(c);
    }

    tokio::spawn(async move {
        let _ = processor.run().await;
    });
    tokio::spawn(async move { while block_receiver.recv().await.is_some() {} });

    // Start Authority
    let mut params = node.parameters.clone();
    params.db_path = db_path;
    node.boot_counter += 1;

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
