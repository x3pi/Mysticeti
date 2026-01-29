// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

//! Epoch Monitor for SyncOnly nodes
//!
//! Monitors epoch changes and triggers transition to Validator mode
//! when the node is detected in the new committee.

use crate::config::NodeConfig;
use anyhow::Result;
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use std::sync::Arc;

/// Start the epoch monitor task for SyncOnly nodes
///
/// This task periodically polls Go's epoch and committee state.
/// When a new epoch is detected and this node is in the committee,
/// it triggers a transition to Validator mode.
///
/// FORK-SAFETY: This monitor does NOT advance epochs unilaterally.
/// It only transitions when the network (via Go's epoch number) has
/// already advanced the epoch through consensus.
pub fn start_epoch_monitor(
    node_mode: crate::node::NodeMode,
    executor_client: &Option<Arc<crate::node::executor_client::ExecutorClient>>,
    current_epoch: u64,
    config: &NodeConfig,
) -> Result<Option<JoinHandle<()>>> {
    // Debug assertions for Send/Sync
    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}
    assert_send::<NodeConfig>();
    assert_sync::<NodeConfig>();
    assert_send::<Arc<crate::node::executor_client::ExecutorClient>>();
    assert_sync::<crate::node::executor_client::ExecutorClient>();
    // Only start for SyncOnly nodes
    if !matches!(node_mode, crate::node::NodeMode::SyncOnly) {
        debug!("📊 [EPOCH MONITOR] Skipping - node is already validator");
        return Ok(None);
    }

    let client_arc = match executor_client {
        Some(client) => client.clone(),
        None => {
            warn!("⚠️ [EPOCH MONITOR] Cannot start - no executor client");
            return Ok(None);
        }
    };

    let node_id = config.node_id;
    let config_clone = config.clone();
    let poll_interval_secs = config.epoch_monitor_poll_interval_secs.unwrap_or(5);

    info!(
        "🔍 [EPOCH MONITOR] Starting epoch monitor for SyncOnly node-{} (current_epoch={}, poll_interval={}s)",
        node_id, current_epoch, poll_interval_secs
    );

    // Start notification listener for real-time push from Go
    // Use fixed socket path - Go notifier connects here when validators register/deregister
    let socket_path = "/tmp/committee-notify.sock";
    let (mut notification_rx, _listener_handle) =
        super::notification_listener::start_notification_driven_monitor(socket_path);

    let handle = tokio::spawn(async move {
        let mut last_known_epoch = current_epoch;
        // FIX: Use protocol_key matching instead of hostname
        // Load protocol_key from config for identity verification
        let own_protocol_key_base64 = match config_clone.load_protocol_keypair() {
            Ok(keypair) => {
                use base64::{engine::general_purpose::STANDARD, Engine as _};
                STANDARD.encode(keypair.public().to_bytes())
            }
            Err(e) => {
                warn!("⚠️ [EPOCH MONITOR] Failed to load protocol keypair: {}. Falling back to hostname matching.", e);
                String::new()
            }
        };
        let hostname = format!("node-{}", node_id);
        let mut last_checked_block: u64 = 0;

        loop {
            // Wait for either notification from Go OR timeout (fallback polling)
            let should_check = tokio::select! {
                // Immediate check when Go pushes notification
                notification = notification_rx.recv() => {
                    match notification {
                        Some(n) => {
                            info!("📢 [EPOCH MONITOR] Received committee change notification: {:?}", n.change_type);
                            true
                        }
                        None => {
                            warn!("⚠️ [EPOCH MONITOR] Notification channel closed");
                            false
                        }
                    }
                }
                // Fallback: poll every N seconds
                _ = tokio::time::sleep(Duration::from_secs(poll_interval_secs)) => {
                    true
                }
            };

            if !should_check {
                continue;
            }

            // Check current epoch from Go
            let go_epoch = match client_arc.get_current_epoch().await {
                Ok(e) => e,
                Err(e) => {
                    debug!("🔍 [EPOCH MONITOR] Failed to get epoch from Go: {}", e);
                    continue;
                }
            };

            // Log epoch change if detected
            if go_epoch > last_known_epoch {
                info!(
                    "🔄 [EPOCH MONITOR] Detected epoch change: {} -> {}",
                    last_known_epoch, go_epoch
                );

                // EPOCH CATCHUP LOGIC FOR SYNCONLY NODES
                // When Go epoch > startup epoch, it means:
                // 1. We started with stale epoch from local Go Master
                // 2. Go Master has now synced correct epoch from network
                //
                // SyncOnly nodes should use Go's updated epoch for transition
                // rather than requiring restart.
                if go_epoch > current_epoch {
                    warn!(
                        "🔄 [EPOCH MONITOR] Go epoch ({}) advanced beyond startup epoch ({}). Using Go's epoch for transition.",
                        go_epoch, current_epoch
                    );
                    info!(
                        "🔄 [EPOCH MONITOR] Will use epoch {} from Go Master for committee check.",
                        go_epoch
                    );
                    // Continue to check committee with the new epoch
                    // Don't exit - we can still transition if we're in the updated committee
                }

                last_known_epoch = go_epoch;
            }

            // ALWAYS check committee, not just on epoch change!
            // This allows nodes registered mid-epoch to transition immediately.
            //
            // FIX: Use get_epoch_boundary_data() for CONSISTENT validator snapshot
            // This fetches validators at the EPOCH BOUNDARY BLOCK (last block of prev epoch)
            // instead of latest block, preventing fork from inconsistent validator sets
            let (boundary_epoch, target_epoch_timestamp_ms, boundary_block, validators) =
                match client_arc.get_epoch_boundary_data(go_epoch).await {
                    Ok(data) => {
                        info!(
                            "📊 [EPOCH MONITOR] Got epoch boundary data: epoch={}, timestamp_ms={}, boundary_block={}, validators={}",
                            data.0, data.1, data.2, data.3.len()
                        );
                        data
                    }
                    Err(e) => {
                        // Fallback to old method if new API not available
                        warn!("⚠️ [EPOCH MONITOR] get_epoch_boundary_data failed: {}. Falling back to legacy API.", e);

                        let last_block = match client_arc.get_last_block_number().await {
                            Ok(b) => b,
                            Err(e) => {
                                warn!("⚠️ [EPOCH MONITOR] Failed to get last block number: {}", e);
                                continue;
                            }
                        };

                        let validators = match client_arc.get_validators_at_block(last_block).await
                        {
                            Ok((v, _)) => v,
                            Err(e) => {
                                warn!("⚠️ [EPOCH MONITOR] Failed to get validators: {}", e);
                                continue;
                            }
                        };

                        let timestamp = match client_arc.get_epoch_start_timestamp().await {
                            Ok(ts) => ts,
                            Err(e) => {
                                warn!(
                                    "⚠️ [EPOCH MONITOR] Failed to get epoch_start_timestamp: {}",
                                    e
                                );
                                continue;
                            }
                        };

                        (go_epoch, timestamp, last_block, validators)
                    }
                };

            // Skip if we already checked this block (avoid spamming)
            if boundary_block == last_checked_block {
                continue;
            }
            last_checked_block = boundary_block;

            // FIX: Use protocol_key matching instead of hostname
            // Fallback to hostname if protocol_key loading failed
            let is_in_committee = if !own_protocol_key_base64.is_empty() {
                validators
                    .iter()
                    .any(|v| v.protocol_key == own_protocol_key_base64)
            } else {
                validators.iter().any(|v| v.name == hostname)
            };

            // FORK-SAFETY: Do NOT advance epoch unilaterally!
            // SyncOnly nodes must wait for network consensus on epoch change.
            // The epoch will advance when:
            // 1. Existing validators produce an AdvanceEpoch system transaction
            // 2. Go Master receives this via network sync from peers
            // 3. Go's epoch number updates
            // 4. We detect the change here
            //
            // We use Go's reported epoch directly - no speculation!
            let target_epoch = boundary_epoch;

            // target_epoch_timestamp_ms already retrieved from get_epoch_boundary_data()
            info!(
                "📊 [EPOCH MONITOR] Using epoch_start_timestamp_ms={} from epoch boundary data",
                target_epoch_timestamp_ms
            );

            // CRITICAL FIX: Only transition when BOTH conditions are met:
            // 1. Node is in the committee
            // 2. Epoch has ACTUALLY CHANGED from when we started
            //
            // Without epoch change check, new validators would incorrectly transition
            // mid-epoch when they're registered. The new committee only takes effect
            // at the NEXT epoch boundary, not immediately after registration.
            //
            // Example: Epoch 0 has 4 validators. Node 5 registers. Node 5 sees itself
            // in the "current" committee but should NOT transition until epoch 1
            // because other validators are still running with the 4-validator committee.
            let epoch_has_changed = go_epoch > current_epoch;

            if is_in_committee && epoch_has_changed {
                // Node is in committee AND epoch has changed - safe to transition to Validator
                info!(
                    "🎉 [EPOCH MONITOR] Node {} is in committee for epoch {} (changed from {})! Preparing transition to Validator...",
                    hostname, target_epoch, current_epoch
                );

                // FORK-SAFETY PEER EPOCH CHECK: Verify that at least one peer is also at the target epoch
                // This prevents premature transition when local Go Master has advanced but peers haven't
                let peer_epoch_verified =
                    match crate::node::committee_source::CommitteeSource::discover(&config_clone)
                        .await
                    {
                        Ok(source) => {
                            if source.is_peer && source.epoch >= target_epoch {
                                info!(
                                "✅ [PEER EPOCH CHECK] Peer {} is at epoch {} (target={}). Safe to proceed!",
                                source.socket_path, source.epoch, target_epoch
                            );
                                true
                            } else if source.is_peer && source.epoch < target_epoch {
                                warn!(
                                "⏳ [PEER EPOCH CHECK] Peer {} still at epoch {} (target={}). Waiting for peers to advance...",
                                source.socket_path, source.epoch, target_epoch
                            );
                                false
                            } else {
                                // No peer available - might be first validator or network partition
                                // LOCAL-ONLY FALLBACK (Transition Autonomy): If no peers are configured
                                // or reachable (is_peer=false), proceed using only local Go Master as
                                // the source of truth. This is critical for:
                                // - First validator in a new committee
                                // - Isolated nodes like Node 4 in early join phases
                                // - Single-node testnets
                                info!(
                                    "ℹ️ [PEER EPOCH CHECK] No peer available (is_peer=false). Using LOCAL-ONLY FALLBACK. Local epoch={}. Proceeding with transition!",
                                    source.epoch
                                );
                                true // FIXED: Allow transition with local Go Master only
                            }
                        }
                        Err(e) => {
                            warn!("⚠️ [PEER EPOCH CHECK] Failed to discover peer source: {}. Will retry...", e);
                            false
                        }
                    };

                // If peer epoch not verified, skip this transition attempt and wait for next poll
                if !peer_epoch_verified {
                    info!(
                        "⏳ [EPOCH MONITOR] Waiting for peer epoch verification before transition. Will check again in {}s...",
                        poll_interval_secs
                    );
                    continue;
                }

                // FORK-SAFETY WAIT BARRIER: Ensure Go Master has synced all blocks from network peers
                // Before joining consensus, we must ensure our Go Master has all blocks that
                // network peers have already produced. Otherwise we'll create new blocks
                // that conflict with existing blocks = FORK!

                // Step 1: Query peer's last_global_exec_index via CommitteeSource
                let peer_last_block =
                    match crate::node::committee_source::CommitteeSource::discover(&config_clone)
                        .await
                    {
                        Ok(source) => {
                            info!(
                            "📊 [SYNC BARRIER] Peer source: last_block={}, epoch={}, is_peer={}",
                            source.last_block, source.epoch, source.is_peer
                        );
                            source.last_block
                        }
                        Err(e) => {
                            warn!("⚠️ [SYNC BARRIER] Failed to discover peer source: {}. Using boundary_block as fallback.", e);
                            boundary_block
                        }
                    };

                // Step 2: Wait for Go Master to sync up to peer's last block
                let go_last_block = match client_arc.get_last_block_number().await {
                    Ok(b) => b,
                    Err(e) => {
                        warn!("⚠️ [SYNC BARRIER] Failed to get Go last block: {}", e);
                        0
                    }
                };

                if go_last_block < peer_last_block {
                    info!(
                        "⏳ [SYNC BARRIER] Go Master ({}) behind network ({}). Waiting for sync...",
                        go_last_block, peer_last_block
                    );

                    // Poll until Go catches up (max 60 seconds)
                    let mut sync_wait_attempts = 0;
                    const MAX_SYNC_WAIT_ATTEMPTS: u32 = 120; // 60 seconds at 500ms intervals

                    loop {
                        sync_wait_attempts += 1;
                        if sync_wait_attempts > MAX_SYNC_WAIT_ATTEMPTS {
                            warn!(
                                "⚠️ [SYNC BARRIER] Timeout waiting for Go sync after 60s. Go={}, Peer={}. Proceeding anyway...",
                                go_last_block, peer_last_block
                            );
                            break;
                        }

                        tokio::time::sleep(Duration::from_millis(500)).await;

                        match client_arc.get_last_block_number().await {
                            Ok(current_go_block) => {
                                if current_go_block >= peer_last_block {
                                    info!(
                                        "✅ [SYNC BARRIER] Go Master synced to block {} (peer={}). Safe to join consensus!",
                                        current_go_block, peer_last_block
                                    );
                                    break;
                                }
                                if sync_wait_attempts % 10 == 0 {
                                    info!(
                                        "⏳ [SYNC BARRIER] Still waiting... Go={}, Peer={}, attempt={}/{}",
                                        current_go_block, peer_last_block, sync_wait_attempts, MAX_SYNC_WAIT_ATTEMPTS
                                    );
                                }
                            }
                            Err(e) => {
                                warn!("⚠️ [SYNC BARRIER] Error getting Go block: {}", e);
                            }
                        }
                    }
                } else {
                    info!(
                        "✅ [SYNC BARRIER] Go Master already synced: Go={} >= Peer={}. Safe to join!",
                        go_last_block, peer_last_block
                    );
                }

                // Get the node from global registry and trigger transition
                if let Some(node_arc) = crate::node::get_transition_handler_node().await {
                    let mut node_guard = node_arc.lock().await;

                    // Check if still SyncOnly (might have changed)
                    if !matches!(node_guard.node_mode, crate::node::NodeMode::SyncOnly) {
                        info!("ℹ️ [EPOCH MONITOR] Node already transitioned, stopping monitor");
                        return;
                    }

                    // FORK-SAFETY: Use the ACTUAL synced global_exec_index from Go, NOT commit_index=0
                    // This ensures we start from where Go has synced, not from epoch start
                    let synced_global_exec_index = match client_arc.get_last_block_number().await {
                        Ok(b) => b,
                        Err(_) => boundary_block, // Fallback to boundary
                    };

                    info!(
                        "📊 [EPOCH MONITOR] Using synced_global_exec_index={} for transition",
                        synced_global_exec_index
                    );

                    // FORK-SAFETY: Do NOT call advance_epoch here!
                    // The epoch was already advanced by network consensus (we checked go_epoch > 0 above).
                    // Go's state is already up-to-date.

                    // Trigger the epoch transition with synced index
                    // Pass synced_global_exec_index so transition knows where to start
                    match node_guard
                        .transition_to_epoch_from_system_tx(
                            target_epoch,
                            target_epoch_timestamp_ms,
                            synced_global_exec_index, // FIXED: Use actual synced index, not 0
                            &config_clone,
                        )
                        .await
                    {
                        Ok(()) => {
                            info!(
                                "✅ [EPOCH MONITOR] Successfully transitioned to Validator mode for epoch {} (starting from global_exec_index={})",
                                target_epoch, synced_global_exec_index
                            );
                            // Exit the monitor loop - node is now a validator
                            return;
                        }
                        Err(e) => {
                            warn!(
                                "❌ [EPOCH MONITOR] Failed to transition to Validator mode: {}",
                                e
                            );
                            // Continue monitoring - might succeed on next epoch
                        }
                    }
                } else {
                    warn!("⚠️ [EPOCH MONITOR] Node not found in global registry");
                }
            } else if is_in_committee && !epoch_has_changed {
                // Node is in committee but epoch hasn't changed yet
                // This is the case when a new validator registered mid-epoch
                // They must wait until next epoch boundary to join consensus
                debug!(
                    "📊 [EPOCH MONITOR] Node {} is in committee but epoch unchanged (current={}, go={}). Waiting for epoch boundary...",
                    hostname, current_epoch, go_epoch
                );
            } else {
                debug!(
                    "📊 [EPOCH MONITOR] Node {} not in committee for epoch {} (found {} validators: {:?})",
                    hostname, target_epoch, validators.len(), validators.iter().map(|v| v.name.clone()).collect::<Vec<_>>()
                );
            }

            last_known_epoch = go_epoch;
        }
    });

    Ok(Some(handle))
}

/// Stop the epoch monitor task
#[allow(dead_code)] // May be called externally or for non-self-triggered transitions
pub async fn stop_epoch_monitor(handle: Option<JoinHandle<()>>) {
    if let Some(h) = handle {
        h.abort();
        info!("🛑 [EPOCH MONITOR] Stopped epoch monitor task");
    }
}
