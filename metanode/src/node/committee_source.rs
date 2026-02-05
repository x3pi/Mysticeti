// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

//! Unified Committee Source Module
//!
//! This module provides a fork-safe way to fetch committee information
//! that works consistently across both SyncOnly and Validator modes.
//!
//! ## Fork Prevention Principles
//!
//! 1. Always use Go Master with highest epoch (network consensus)
//! 2. Always use `get_epoch_start_timestamp()` for consistent genesis hash
//! 3. Committee and timestamp must come from the SAME source

use crate::config::NodeConfig;
use crate::node::executor_client::ExecutorClient;
use anyhow::Result;
use consensus_config::Committee;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Unified committee source for both SyncOnly and Validator modes
/// Ensures fork-safe committee fetching by always using the best available source
#[derive(Debug, Clone)]
pub struct CommitteeSource {
    /// Best Go Master socket (either local or peer)
    pub socket_path: String,
    /// Epoch from the best source
    pub epoch: u64,
    /// Last committed block from best source
    pub last_block: u64,
    /// Whether this source is from a peer (not local)
    pub is_peer: bool,
    /// Peer RPC addresses for fallback when local is behind
    #[allow(dead_code)]
    pub peer_rpc_addresses: Vec<String>,
}

impl CommitteeSource {
    /// Discover the best committee source
    /// Priority: Peer with highest epoch > Local Go Master
    ///
    /// This ensures all nodes use the same committee source, preventing fork.
    pub async fn discover(config: &NodeConfig) -> Result<Self> {
        info!("🔍 [COMMITTEE SOURCE] Discovering best committee source...");

        // First, check local Go Master
        let local_client = ExecutorClient::new(
            true,
            false,
            config.executor_send_socket_path.clone(),
            config.executor_receive_socket_path.clone(),
            None,
        );

        let local_epoch = local_client.get_current_epoch().await.unwrap_or(0);
        let local_block = local_client.get_last_block_number().await.unwrap_or(0);

        info!(
            "📊 [COMMITTEE SOURCE] Local Go Master: epoch={}, block={}",
            local_epoch, local_block
        );

        // If no TCP peer addresses configured, use local
        if config.peer_rpc_addresses.is_empty() {
            info!("ℹ️ [COMMITTEE SOURCE] No peer RPC addresses configured, using local Go Master");
            return Ok(Self {
                socket_path: config.executor_receive_socket_path.clone(),
                epoch: local_epoch,
                last_block: local_block,
                is_peer: false,
                peer_rpc_addresses: Vec::new(),
            });
        }

        // Query TCP peers to find the best source
        let mut best_epoch = local_epoch;
        let mut best_block = local_block;
        let best_socket = config.executor_receive_socket_path.clone();
        let mut is_peer = false;

        // Use TCP RPC to query peer nodes over network
        for peer_address in &config.peer_rpc_addresses {
            match crate::network::peer_rpc::query_peer_info(peer_address).await {
                Ok(peer_info) => {
                    debug!(
                        "📊 [COMMITTEE SOURCE] TCP Peer {}: epoch={}, block={}, timestamp={}",
                        peer_address, peer_info.epoch, peer_info.last_block, peer_info.timestamp_ms
                    );

                    // Use peer if:
                    // 1. Higher epoch (network has advanced)
                    // 2. Same epoch but higher block (more up-to-date)
                    if peer_info.epoch > best_epoch
                        || (peer_info.epoch == best_epoch && peer_info.last_block > best_block)
                    {
                        best_epoch = peer_info.epoch;
                        best_block = peer_info.last_block;
                        // For TCP peers, we still use local socket for actual data read
                        // The peer info just tells us who is ahead
                        // best_socket stays as local since we can't RPC read blocks over TCP (yet)
                        is_peer = true;

                        info!(
                            "✅ [COMMITTEE SOURCE] Found ahead peer: {} (epoch={}, block={}). Using local Go Master for data.",
                            peer_address, peer_info.epoch, peer_info.last_block
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        "⚠️ [COMMITTEE SOURCE] Failed to query TCP peer {}: {}",
                        peer_address, e
                    );
                }
            }
        }

        info!(
            "✅ [COMMITTEE SOURCE] Selected source: {} (epoch={}, block={}, is_peer={})",
            best_socket, best_epoch, best_block, is_peer
        );

        Ok(Self {
            socket_path: best_socket.clone(),
            epoch: best_epoch,
            last_block: best_block,
            is_peer,
            peer_rpc_addresses: config.peer_rpc_addresses.clone(),
        })
    }

    /// Create an executor client connected to this source
    pub fn create_executor_client(&self, send_socket: &str) -> Arc<ExecutorClient> {
        Arc::new(ExecutorClient::new(
            true,
            false,
            send_socket.to_string(),
            self.socket_path.clone(),
            None,
        ))
    }

    /// Fetch committee from this source using EPOCH BOUNDARY DATA
    /// This ensures validators are fetched from the boundary block (last block of prev epoch)
    /// for consistent committee across all nodes
    ///
    /// NOTE: target_epoch is the epoch we're transitioning TO, not the current epoch.
    /// This is critical because during epoch transition, the Go Master may still report
    /// the old epoch while we need the new epoch's committee.
    ///
    /// CRITICAL FIX: Single Source of Truth for Committee
    ///
    /// To prevent forks, this function ONLY uses LOCAL Go as the data source.
    /// It will wait INDEFINITELY until local Go has synced to the boundary block
    /// and can provide validators for the target epoch.
    ///
    /// The sync process (rust_sync_node) must complete BEFORE this returns.
    /// This ensures all nodes derive committee from the same verified blockchain state.
    pub async fn fetch_committee(&self, send_socket: &str, target_epoch: u64) -> Result<Committee> {
        let client = self.create_executor_client(send_socket);

        info!(
            "📋 [COMMITTEE SOURCE] Fetching committee for target_epoch {} from LOCAL Go ONLY (single source)",
            target_epoch
        );

        // Retry configuration - wait indefinitely for sync to complete
        const INITIAL_DELAY_MS: u64 = 500;
        const MAX_DELAY_MS: u64 = 5000;
        const LOG_INTERVAL: u32 = 10;

        let mut attempt = 0u32;
        let mut delay_ms = INITIAL_DELAY_MS;

        loop {
            attempt += 1;
            let should_log = attempt == 1 || attempt % LOG_INTERVAL == 0;

            // SINGLE SOURCE: Only use local Go Master
            match client.get_epoch_boundary_data(target_epoch).await {
                Ok((epoch, timestamp_ms, boundary_block, validators)) => {
                    if epoch == target_epoch {
                        info!(
                            "✅ [COMMITTEE SOURCE] Got epoch boundary data from LOCAL Go: epoch={}, timestamp_ms={}, boundary_block={}, validator_count={} (attempt {})",
                            epoch, timestamp_ms, boundary_block, validators.len(), attempt
                        );

                        // Build committee from validators
                        match crate::node::committee::build_committee_from_validator_info_list(
                            &validators,
                            target_epoch,
                        )
                        .await
                        {
                            Ok(committee) => {
                                info!(
                                    "✅ [COMMITTEE SOURCE] Successfully built committee with {} authorities from LOCAL source",
                                    committee.size()
                                );
                                return Ok(committee);
                            }
                            Err(e) => {
                                if should_log {
                                    warn!(
                                        "⚠️ [COMMITTEE SOURCE] build_committee failed: {} (attempt {}). Waiting for sync...",
                                        e, attempt
                                    );
                                }
                            }
                        }
                    } else if should_log {
                        info!(
                            "⏳ [COMMITTEE SOURCE] Local Go at epoch {}, waiting for epoch {} (attempt {}). Sync in progress...",
                            epoch, target_epoch, attempt
                        );
                    }
                }
                Err(e) => {
                    if should_log {
                        info!(
                            "⏳ [COMMITTEE SOURCE] Local Go not ready: {} (attempt {}). Waiting for sync to complete...",
                            e, attempt
                        );
                    }
                }
            }

            // Wait and retry - sync must complete before proceeding
            tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
            delay_ms = std::cmp::min(delay_ms * 2, MAX_DELAY_MS);
        }
    }

    /// Fetch committee AND timestamp from Go (UNIFIED SOURCE)
    ///
    /// CRITICAL: This returns the timestamp from Go's get_epoch_boundary_data response.
    /// Go derives this timestamp deterministically:
    /// - Epoch 0: Genesis timestamp from genesis.json
    /// - Epoch N: boundaryBlock.Header().TimeStamp() * 1000
    ///
    /// Using this timestamp ensures ALL nodes have identical genesis blocks = NO FORK!
    pub async fn fetch_committee_with_timestamp(
        &self,
        send_socket: &str,
        target_epoch: u64,
    ) -> Result<(Committee, u64)> {
        let client = self.create_executor_client(send_socket);

        info!(
            "📋 [COMMITTEE SOURCE] Fetching committee+timestamp for epoch {} from LOCAL Go (unified source)",
            target_epoch
        );

        const INITIAL_DELAY_MS: u64 = 500;
        const MAX_DELAY_MS: u64 = 5000;
        const LOG_INTERVAL: u32 = 10;
        const MAX_ATTEMPTS: u32 = 60; // ~5 minutes with exponential backoff

        let mut attempt = 0u32;
        let mut delay_ms = INITIAL_DELAY_MS;

        loop {
            attempt += 1;

            // CRITICAL FIX: Prevent infinite loop when Go doesn't have epoch data
            // This can happen when transition_mode_only is called before Go syncs
            if attempt > MAX_ATTEMPTS {
                return Err(anyhow::anyhow!(
                    "Timeout waiting for Go to have epoch {} data after {} attempts. \
                    Go may not have synced to this epoch yet.",
                    target_epoch,
                    MAX_ATTEMPTS
                ));
            }

            let should_log = attempt == 1 || attempt % LOG_INTERVAL == 0;

            match client.get_epoch_boundary_data(target_epoch).await {
                Ok((epoch, timestamp_ms, boundary_block, validators)) => {
                    if epoch == target_epoch {
                        info!(
                            "✅ [UNIFIED TIMESTAMP] Got from Go: epoch={}, timestamp_ms={}, boundary_block={} (attempt {})",
                            epoch, timestamp_ms, boundary_block, attempt
                        );

                        // Build committee from validators
                        match crate::node::committee::build_committee_from_validator_info_list(
                            &validators,
                            target_epoch,
                        )
                        .await
                        {
                            Ok(committee) => {
                                info!(
                                    "✅ [UNIFIED TIMESTAMP] Committee size={}, AUTHORITATIVE timestamp={} ms",
                                    committee.size(), timestamp_ms
                                );
                                return Ok((committee, timestamp_ms));
                            }
                            Err(e) => {
                                if should_log {
                                    warn!(
                                        "⚠️ [UNIFIED TIMESTAMP] build_committee failed: {} (attempt {})",
                                        e, attempt
                                    );
                                }
                            }
                        }
                    } else if should_log {
                        info!(
                            "⏳ [UNIFIED TIMESTAMP] Local Go at epoch {}, waiting for epoch {} (attempt {})",
                            epoch, target_epoch, attempt
                        );
                    }
                }
                Err(e) => {
                    if should_log {
                        info!(
                            "⏳ [UNIFIED TIMESTAMP] Local Go not ready: {} (attempt {})",
                            e, attempt
                        );
                    }
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
            delay_ms = std::cmp::min(delay_ms * 2, MAX_DELAY_MS);
        }
    }

    /// Validate that this source matches expected epoch
    /// Returns true if matches, logs warning and returns false otherwise
    pub fn validate_epoch(&self, expected_epoch: u64) -> bool {
        if self.epoch != expected_epoch {
            warn!(
                "⚠️ [COMMITTEE SOURCE] Epoch mismatch! Expected={}, Source={}. \
                 This may indicate network partition or stale local state.",
                expected_epoch, self.epoch
            );
            false
        } else {
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_committee_source_local_only() {
        // Test with empty peer list - should use local
        // This is a placeholder - actual test would need mock clients
    }
}
