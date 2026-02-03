// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

//! Unified Epoch Monitor
//!
//! A single monitor that handles epoch transitions for BOTH SyncOnly and Validator nodes.
//! This replaces the previous fragmented approach of separate monitors.
//!
//! ## Design Principles
//! 1. **Single Source of Truth**: Go layer epoch is authoritative
//! 2. **Always Running**: Monitor never exits - runs continuously for all node modes
//! 3. **Fork-Safe**: Uses `boundary_block` from `get_epoch_boundary_data()`
//! 4. **Unified Logic**: Same code path for SyncOnly and Validator nodes

use crate::config::NodeConfig;
use crate::node::executor_client::proto;
use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

/// Start the unified epoch monitor for ALL node types (SyncOnly and Validator)
///
/// This monitor:
/// 1. Polls Go epoch every N seconds
/// 2. Detects when Rust epoch falls behind Go epoch
/// 3. Fetches epoch boundary data (fork-safe)
/// 4. Triggers appropriate transition (SyncOnly→Validator or epoch update)
///
/// IMPORTANT: This monitor NEVER exits - it runs continuously for the lifetime of the node.
/// This prevents the bug where Validators get stuck when they miss EndOfEpoch transactions.
pub fn start_unified_epoch_monitor(
    executor_client: &Option<Arc<crate::node::executor_client::ExecutorClient>>,
    config: &NodeConfig,
) -> Result<Option<JoinHandle<()>>> {
    let client_arc = match executor_client {
        Some(client) => client.clone(),
        None => {
            warn!("⚠️ [EPOCH MONITOR] Cannot start - no executor client");
            return Ok(None);
        }
    };

    let node_id = config.node_id;
    let config_clone = config.clone();
    // Default poll interval: configurable, default 10 seconds
    let poll_interval_secs = config.epoch_monitor_poll_interval_secs.unwrap_or(10);

    info!(
        "🔄 [EPOCH MONITOR] Starting unified epoch monitor for node-{} (poll_interval={}s)",
        node_id, poll_interval_secs
    );

    // Load own protocol key for committee membership check
    let own_protocol_key_base64 = match config.load_protocol_keypair() {
        Ok(keypair) => {
            use base64::{engine::general_purpose::STANDARD, Engine as _};
            STANDARD.encode(keypair.public().to_bytes())
        }
        Err(e) => {
            warn!(
                "⚠️ [EPOCH MONITOR] Failed to load protocol keypair: {}. Falling back to hostname matching.",
                e
            );
            String::new()
        }
    };
    let hostname = format!("node-{}", node_id);

    let handle = tokio::spawn(async move {
        loop {
            // Wait for poll interval
            tokio::time::sleep(Duration::from_secs(poll_interval_secs)).await;

            // 1. Get LOCAL Go epoch (may be stale for late-joiners!)
            let local_go_epoch = match client_arc.get_current_epoch().await {
                Ok(epoch) => epoch,
                Err(e) => {
                    debug!("⚠️ [EPOCH MONITOR] Failed to get local Go epoch: {}", e);
                    continue;
                }
            };

            // 2. Get NETWORK epoch from peers (critical for late-joiners!)
            // Use peer_rpc_addresses for WAN-based discovery
            let network_epoch = {
                let peer_rpc = config_clone.peer_rpc_addresses.clone();
                let _own_socket = config_clone.executor_receive_socket_path.clone();

                if !peer_rpc.is_empty() {
                    // WAN-based discovery (TCP) - recommended for cross-node sync
                    match crate::network::peer_rpc::query_peer_epochs_network(&peer_rpc).await {
                        Ok((epoch, _block, peer)) => {
                            if epoch > local_go_epoch {
                                info!(
                                    "🌐 [EPOCH MONITOR] Network epoch {} from peer {} is AHEAD of local Go epoch {}",
                                    epoch, peer, local_go_epoch
                                );
                            }
                            epoch
                        }
                        Err(_) => local_go_epoch, // Fallback to local
                    }
                } else {
                    // No WAN peers configured - use local Go epoch
                    // NOTE: LAN peer_executor_sockets was removed. For cross-node sync, configure peer_rpc_addresses.
                    local_go_epoch
                }
            };

            // 3. Get current Rust epoch from node
            let (rust_epoch, current_mode) =
                if let Some(node_arc) = crate::node::get_transition_handler_node().await {
                    let node_guard = node_arc.lock().await;
                    (node_guard.current_epoch, node_guard.node_mode.clone())
                } else {
                    debug!("⚠️ [EPOCH MONITOR] Node not registered yet, waiting...");
                    continue;
                };

            // 4. Check if transition needed (NETWORK epoch ahead of Rust)
            // Use network_epoch instead of local_go_epoch!
            if network_epoch <= rust_epoch {
                // No transition needed - epochs are in sync with network
                continue;
            }

            let epoch_gap = network_epoch - rust_epoch;
            info!(
                "🔄 [EPOCH MONITOR] Epoch gap detected: Rust={} Network={} (gap={})",
                rust_epoch, network_epoch, epoch_gap
            );

            // 4. Get epoch boundary data (FORK-SAFE!)
            // First try local Go, then fallback to peer TCP RPC
            // This is critical for late-joiners: local Go may not have boundary data
            // because it hasn't witnessed the epoch transition yet.

            // Try local Go first
            let local_executor_client = client_arc.clone();
            let boundary_data_result = local_executor_client
                .get_epoch_boundary_data(network_epoch)
                .await;

            let (new_epoch, epoch_timestamp_ms, boundary_block, validators) =
                match boundary_data_result {
                    Ok(data) => {
                        info!(
                        "📊 [EPOCH MONITOR] Got boundary data from LOCAL: epoch={}, timestamp={}ms, boundary_block={}, validators={}",
                        data.0, data.1, data.2, data.3.len()
                    );
                        data
                    }
                    Err(e) => {
                        // Local failed - try to fetch from peer via TCP RPC
                        warn!(
                        "⚠️ [EPOCH MONITOR] Local Go failed for epoch {}: {}. Trying peers via TCP RPC...",
                        network_epoch, e
                    );

                        let peer_rpc = &config_clone.peer_rpc_addresses;
                        if peer_rpc.is_empty() {
                            warn!("⚠️ [EPOCH MONITOR] No peers configured. Cannot fetch epoch boundary data. Will retry.");
                            continue;
                        }

                        // Try each peer until one succeeds
                        let mut peer_data = None;
                        for peer_addr in peer_rpc.iter() {
                            match crate::network::peer_rpc::query_peer_epoch_boundary_data(
                                peer_addr,
                                network_epoch,
                            )
                            .await
                            {
                                Ok(response) => {
                                    info!(
                                    "✅ [EPOCH MONITOR] Got boundary data from PEER {}: epoch={}, timestamp={}ms, boundary_block={}, validators={}",
                                    peer_addr, response.epoch, response.timestamp_ms, response.boundary_block, response.validators.len()
                                );

                                    // Convert ValidatorInfoSimple back to ValidatorInfo
                                    let validators: Vec<proto::ValidatorInfo> = response
                                        .validators
                                        .iter()
                                        .map(|v| {
                                            proto::ValidatorInfo {
                                                name: v.name.clone(),
                                                address: v.address.clone(),
                                                stake: v.stake.to_string(),
                                                protocol_key: v.protocol_key.clone(),
                                                network_key: v.network_key.clone(),
                                                authority_key: String::new(), // Not used in transition
                                                description: String::new(),
                                                website: String::new(),
                                                image: String::new(),
                                                commission_rate: 0,
                                                min_self_delegation: String::new(),
                                                accumulated_rewards_per_share: String::new(),
                                            }
                                        })
                                        .collect();

                                    peer_data = Some((
                                        response.epoch,
                                        response.timestamp_ms,
                                        response.boundary_block,
                                        validators,
                                    ));
                                    break;
                                }
                                Err(e) => {
                                    warn!("⚠️ [EPOCH MONITOR] Peer {} failed: {}. Trying next peer...", peer_addr, e);
                                }
                            }
                        }

                        match peer_data {
                            Some(data) => data,
                            None => {
                                warn!("⚠️ [EPOCH MONITOR] All peers failed to provide epoch {} boundary data. Will retry.", network_epoch);
                                continue;
                            }
                        }
                    }
                };

            // 5. Check committee membership for target epoch
            let is_in_committee = if !own_protocol_key_base64.is_empty() {
                validators
                    .iter()
                    .any(|v| v.protocol_key == own_protocol_key_base64)
            } else {
                validators.iter().any(|v| v.name == hostname)
            };

            // 6. Log transition intent
            let target_mode = if is_in_committee {
                crate::node::NodeMode::Validator
            } else {
                crate::node::NodeMode::SyncOnly
            };

            info!(
                "🔄 [EPOCH MONITOR] Transitioning: epoch {} → {} | mode {:?} → {:?} | boundary_block={}",
                rust_epoch, new_epoch, current_mode, target_mode, boundary_block
            );

            // 7. Execute transition
            if let Some(node_arc) = crate::node::get_transition_handler_node().await {
                let mut node_guard = node_arc.lock().await;

                // Use boundary_block as synced_global_exec_index (FORK-SAFE!)
                let synced_global_exec_index = boundary_block;

                match node_guard
                    .transition_to_epoch_from_system_tx(
                        new_epoch,
                        epoch_timestamp_ms,
                        synced_global_exec_index,
                        &config_clone,
                    )
                    .await
                {
                    Ok(()) => {
                        info!(
                            "✅ [EPOCH MONITOR] Successfully transitioned to epoch {} as {:?}",
                            new_epoch, target_mode
                        );
                    }
                    Err(e) => {
                        warn!(
                            "❌ [EPOCH MONITOR] Failed to transition to epoch {}: {}",
                            new_epoch, e
                        );
                    }
                }
            }

            // CRITICAL: Do NOT exit the loop! Monitor continues running
            // This is the key fix - monitor runs for the entire node lifetime
        }
    });

    Ok(Some(handle))
}

/// Stop the epoch monitor task
#[allow(dead_code)]
pub async fn stop_epoch_monitor(handle: Option<JoinHandle<()>>) {
    if let Some(h) = handle {
        h.abort();
        info!("🛑 [EPOCH MONITOR] Stopped unified epoch monitor");
    }
}

// ============================================================================
// LEGACY FUNCTIONS (kept for backwards compatibility, delegate to unified)
// ============================================================================

/// Legacy function - now delegates to unified monitor
/// Kept for backwards compatibility with existing code
#[allow(dead_code)]
#[deprecated(note = "Use start_unified_epoch_monitor instead")]
pub fn start_epoch_monitor(
    _node_mode: crate::node::NodeMode,
    executor_client: &Option<Arc<crate::node::executor_client::ExecutorClient>>,
    _current_epoch: u64,
    config: &NodeConfig,
) -> Result<Option<JoinHandle<()>>> {
    start_unified_epoch_monitor(executor_client, config)
}

/// Legacy function - now delegates to unified monitor
/// Kept for backwards compatibility
#[allow(dead_code)]
#[deprecated(note = "Use start_unified_epoch_monitor instead")]
pub fn start_validator_epoch_watchdog(
    executor_client: &Option<Arc<crate::node::executor_client::ExecutorClient>>,
    _current_epoch: u64,
    config: &NodeConfig,
) -> Result<Option<JoinHandle<()>>> {
    start_unified_epoch_monitor(executor_client, config)
}
