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
    /// Fixed epoch start timestamp (CRITICAL for genesis hash)
    pub epoch_timestamp_ms: u64,
    /// Last committed block from best source
    pub last_block: u64,
    /// Whether this source is from a peer (not local)
    pub is_peer: bool,
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
        let local_timestamp = local_client.get_epoch_start_timestamp().await.unwrap_or(0);

        info!(
            "📊 [COMMITTEE SOURCE] Local Go Master: epoch={}, block={}, timestamp={}",
            local_epoch, local_block, local_timestamp
        );

        // If no peer sockets configured, use local
        if config.peer_go_master_sockets.is_empty() {
            info!("ℹ️ [COMMITTEE SOURCE] No peer sockets configured, using local Go Master");
            return Ok(Self {
                socket_path: config.executor_receive_socket_path.clone(),
                epoch: local_epoch,
                epoch_timestamp_ms: local_timestamp,
                last_block: local_block,
                is_peer: false,
            });
        }

        // Query peers to find the best source
        let mut best_epoch = local_epoch;
        let mut best_block = local_block;
        let mut best_timestamp = local_timestamp;
        let mut best_socket = config.executor_receive_socket_path.clone();
        let mut is_peer = false;

        for peer_socket in &config.peer_go_master_sockets {
            if peer_socket == &config.executor_receive_socket_path {
                continue; // Skip self
            }

            let peer_client = ExecutorClient::new(
                true,
                false,
                String::new(), // Send socket not needed for read
                peer_socket.clone(),
                None,
            );

            match peer_client.get_current_epoch().await {
                Ok(peer_epoch) => {
                    let peer_block = peer_client.get_last_block_number().await.unwrap_or(0);
                    let peer_timestamp = peer_client.get_epoch_start_timestamp().await.unwrap_or(0);

                    debug!(
                        "📊 [COMMITTEE SOURCE] Peer {}: epoch={}, block={}, timestamp={}",
                        peer_socket, peer_epoch, peer_block, peer_timestamp
                    );

                    // Use peer if:
                    // 1. Higher epoch (network has advanced)
                    // 2. Same epoch but higher block (more up-to-date)
                    if peer_epoch > best_epoch
                        || (peer_epoch == best_epoch && peer_block > best_block)
                    {
                        best_epoch = peer_epoch;
                        best_block = peer_block;
                        best_timestamp = peer_timestamp;
                        best_socket = peer_socket.clone();
                        is_peer = true;

                        info!(
                            "✅ [COMMITTEE SOURCE] New best source: {} (epoch={}, block={})",
                            peer_socket, peer_epoch, peer_block
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        "⚠️ [COMMITTEE SOURCE] Failed to query peer {}: {}",
                        peer_socket, e
                    );
                }
            }
        }

        // Validate timestamp is non-zero
        if best_timestamp == 0 {
            warn!("⚠️ [COMMITTEE SOURCE] Best source has zero timestamp, this may cause issues");
        }

        info!(
            "✅ [COMMITTEE SOURCE] Selected source: {} (epoch={}, block={}, timestamp={}, is_peer={})",
            best_socket, best_epoch, best_block, best_timestamp, is_peer
        );

        Ok(Self {
            socket_path: best_socket,
            epoch: best_epoch,
            epoch_timestamp_ms: best_timestamp,
            last_block: best_block,
            is_peer,
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
    /// CRITICAL: Retries INDEFINITELY until success since epoch transition MUST succeed
    /// for the network to progress. Without correct committee, consensus cannot continue.
    pub async fn fetch_committee(&self, send_socket: &str, target_epoch: u64) -> Result<Committee> {
        let client = self.create_executor_client(send_socket);

        info!(
            "📋 [COMMITTEE SOURCE] Fetching committee for target_epoch {} from {} (will retry until success)",
            target_epoch, self.socket_path
        );

        // Retry configuration - NO LIMIT, will retry until success
        const INITIAL_DELAY_MS: u64 = 500;
        const MAX_DELAY_MS: u64 = 5000;
        const LOG_INTERVAL: u32 = 10; // Log detailed info every N attempts

        let mut attempt = 0u32;
        let mut delay_ms = INITIAL_DELAY_MS;

        loop {
            attempt += 1;
            let should_log = attempt == 1 || attempt % LOG_INTERVAL == 0;

            // FIX: Use get_epoch_boundary_data() with TARGET EPOCH for consistent validator snapshot
            match client.get_epoch_boundary_data(target_epoch).await {
                Ok((epoch, timestamp_ms, boundary_block, validators)) => {
                    // Verify we got the expected epoch
                    if epoch != target_epoch {
                        if should_log {
                            warn!(
                                "⚠️ [COMMITTEE SOURCE] Epoch mismatch! Expected={}, Got={}. Waiting for Go to advance... (attempt {})",
                                target_epoch, epoch, attempt
                            );
                        }
                        tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                        delay_ms = std::cmp::min(delay_ms * 2, MAX_DELAY_MS);
                        continue; // Keep retrying until correct epoch
                    }

                    info!(
                        "✅ [COMMITTEE SOURCE] Got epoch boundary data: epoch={}, timestamp_ms={}, boundary_block={}, validator_count={} (attempt {})",
                        epoch, timestamp_ms, boundary_block, validators.len(), attempt
                    );

                    // Build committee from boundary validators using TARGET EPOCH
                    return crate::node::committee::build_committee_from_validator_info_list(
                        &validators,
                        target_epoch,
                    )
                    .await;
                }
                Err(e) => {
                    if should_log {
                        warn!(
                            "⚠️ [COMMITTEE SOURCE] get_epoch_boundary_data failed: {} (attempt {}). Will keep retrying...",
                            e, attempt
                        );
                    }
                    tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                    delay_ms = std::cmp::min(delay_ms * 2, MAX_DELAY_MS);
                    continue; // Keep retrying - epoch transition MUST succeed
                }
            }
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

    /// Get the epoch timestamp (CRITICAL for genesis hash consistency)
    /// This is a fixed value set when epoch started, not a dynamic timestamp
    pub fn get_epoch_timestamp(&self) -> u64 {
        self.epoch_timestamp_ms
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
