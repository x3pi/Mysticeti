// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

//! Mode-only transitions (SyncOnly → Validator within the same epoch).

use crate::config::NodeConfig;
use crate::node::executor_client::ExecutorClient;
use crate::node::{ConsensusNode, NodeMode};
use anyhow::Result;
use consensus_core::{
    CommitConsumerArgs, ConsensusAuthority, NetworkType, SystemTransactionProvider,
};
use prometheus::Registry;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{info, warn};

use super::demotion::determine_role_for_epoch;

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

    // CRITICAL FIX: Stop sync task FIRST when upgrading to Validator
    // Sync is redundant once we're a Validator - blocks come from DAG consensus
    // Use SyncController for centralized state management
    if node.sync_controller.is_enabled() {
        info!("🛑 [MODE TRANSITION] Stopping sync task via SyncController (Validator gets blocks from DAG)");
        if let Err(e) = node.sync_controller.disable_sync().await {
            warn!(
                "⚠️ [MODE TRANSITION] SyncController disable failed: {}. Continuing anyway.",
                e
            );
        }
    }

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
    node.check_and_update_node_mode(&committee, config, true)
        .await?;

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

    // Initialize BlockCoordinator for dual-stream block production
    let coordinator = Arc::new(crate::node::block_coordinator::BlockCoordinator::new(
        synced_global_exec_index + 1,
        crate::node::block_coordinator::CoordinatorConfig::default(),
    ));
    node.block_coordinator = Some(coordinator.clone());
    info!("📦 [COORDINATOR] BlockCoordinator initialized for mode transition epoch {} (next_expected={})", 
        epoch, synced_global_exec_index + 1);

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
        .with_epoch_transition_callback(epoch_cb)
        .with_block_coordinator(coordinator.clone()); // Connect to BlockCoordinator

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

/// CASE 1 handler: SyncOnly node needs to become Validator within the SAME epoch.
/// Actively polls Go until it syncs to the boundary, then triggers mode-only transition.
/// Aborts if a new epoch is detected or if timeout (5 min) is reached.
pub(super) async fn handle_synconly_upgrade_wait(
    node: &mut ConsensusNode,
    new_epoch: u64,
    boundary_block_from_tx: u64,
    synced_global_exec_index: u64,
    config: &NodeConfig,
) -> Result<()> {
    // SAFETY CHECK: Verify role explicitly before upgrading
    let own_protocol_pubkey = node.protocol_keypair.public();
    let role_check = determine_role_for_epoch(new_epoch, &own_protocol_pubkey, config).await?;

    if matches!(role_check, NodeMode::SyncOnly) {
        info!("ℹ️ [MODE TRANSITION] Re-checked role for epoch {}: Still SyncOnly (not in committee). Aborting upgrade.", new_epoch);
        return Ok(());
    }
    info!(
        "✅ [MODE TRANSITION] Verified role for epoch {}: Validator. Proceeding with sync wait...",
        new_epoch
    );

    // Create FRESH executor client for reliable communication
    let fresh_executor_client = ExecutorClient::new(
        true,
        false, // Don't need commit capability for just checking block number
        config.executor_send_socket_path.clone(),
        config.executor_receive_socket_path.clone(),
        None,
    );

    // ACTIVE WAIT: Poll Go until it reaches the required boundary
    let poll_interval = Duration::from_millis(500);
    let max_attempts = 600; // 5 minutes max (600 * 500ms)
    let mut attempt = 0u64;

    loop {
        attempt += 1;

        // SAFETY: Check if a new epoch has started - if so, abort
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

        if attempt >= max_attempts {
            warn!(
                "⚠️ [MODE TRANSITION] Timeout after {} attempts (5 min). Go block {} still < boundary {}. Will retry via epoch_monitor.",
                attempt, go_current_block, synced_global_exec_index
            );
            return Ok(());
        }

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
    transition_mode_only(
        node,
        new_epoch,
        boundary_block_from_tx,
        synced_global_exec_index,
        config,
    )
    .await
}
