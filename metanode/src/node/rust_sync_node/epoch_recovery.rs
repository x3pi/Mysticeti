// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

//! Queue processing and deferred epoch transition handling.

use super::RustSyncNode;
use anyhow::Result;
use consensus_core::{CommitAPI, CommitRef, CommittedSubDag};
use std::sync::atomic::Ordering;
use std::time::Duration;
use tracing::{debug, error, info, warn};

use crate::network::peer_rpc::query_peer_epoch_boundary_data;

impl RustSyncNode {
    /// Process queue - drain ready commits and send to Go sequentially (Phase 3)
    pub(super) async fn process_queue(&self) -> Result<u32> {
        let mut ready_commits = {
            let mut queue = self.block_queue.lock().await;
            queue.drain_ready()
        };

        if ready_commits.is_empty() {
            return Ok(0);
        }

        let mut blocks_sent = 0u32;
        let mut failed_at: Option<usize> = None;

        for (idx, commit_data) in ready_commits.iter().enumerate() {
            // Construct CommittedSubDag
            let subdag = CommittedSubDag::new(
                commit_data.commit.leader(),
                commit_data.blocks.clone(),
                commit_data.commit.timestamp_ms(),
                CommitRef::new(
                    commit_data.commit.index(),
                    consensus_core::CommitDigest::MIN,
                ),
                commit_data.commit.global_exec_index(),
            );

            // ═══════════════════════════════════════════════════════════════════════════════
            // CRITICAL FIX: Ensure Single Source of Truth
            // Strict Retry Loop: We MUST resolve the leader_address from our cache.
            // If we cannot, we wait and retry. We NEVER send None to Go.
            // ═══════════════════════════════════════════════════════════════════════════════
            let epoch = commit_data.epoch;
            let leader_author_index = subdag.leader.author.value() as usize;

            let mut retry_count = 0;
            let leader_address = loop {
                // 1. Try to resolve from cache
                {
                    let mut cache = self.epoch_eth_addresses.lock().await;

                    // Load if missing
                    if !cache.contains_key(&epoch) {
                        info!(
                            "📥 [RUST-SYNC] Cache miss for epoch {}. Trying local Go...",
                            epoch
                        );

                        let mut loaded = false;

                        // 1. Try local Go first
                        match self.executor_client.get_epoch_boundary_data(epoch).await {
                            Ok((_e, _ts, _boundary, validators)) => {
                                let mut sorted_validators = validators.clone();
                                sorted_validators
                                    .sort_by(|a, b| a.authority_key.cmp(&b.authority_key));

                                let addr_list: Vec<Vec<u8>> = sorted_validators
                                    .iter()
                                    .map(|v| {
                                        hex::decode(&v.address.trim_start_matches("0x"))
                                            .unwrap_or_default()
                                    })
                                    .collect();
                                cache.insert(epoch, addr_list);
                                info!(
                                    "✅ [RUST-SYNC] Cache populated from local Go for epoch {}",
                                    epoch
                                );
                                loaded = true;
                            }
                            Err(e) => {
                                warn!(
                                    "⚠️ [RUST-SYNC] Local Go failed for epoch {}: {}. Trying peer...",
                                    epoch, e
                                );
                            }
                        }

                        // 2. Fallback to peer if local Go failed
                        if !loaded {
                            // Get peer addresses from config
                            if !self.config.peer_rpc_addresses.is_empty() {
                                for peer_addr in &self.config.peer_rpc_addresses {
                                    match query_peer_epoch_boundary_data(peer_addr, epoch).await {
                                        Ok(response) => {
                                            if response.error.is_none() {
                                                let mut sorted = response.validators.clone();
                                                sorted.sort_by(|a, b| {
                                                    a.authority_key.cmp(&b.authority_key)
                                                });
                                                let addr_list: Vec<Vec<u8>> = sorted
                                                    .iter()
                                                    .map(|v| {
                                                        hex::decode(
                                                            &v.address.trim_start_matches("0x"),
                                                        )
                                                        .unwrap_or_default()
                                                    })
                                                    .collect();
                                                cache.insert(epoch, addr_list);
                                                info!("✅ [RUST-SYNC] Cache populated from PEER {} for epoch {}", peer_addr, epoch);
                                                loaded = true;
                                                break;
                                            }
                                        }
                                        Err(e) => {
                                            warn!(
                                                "⚠️ [RUST-SYNC] Peer {} query failed: {}",
                                                peer_addr, e
                                            );
                                        }
                                    }
                                }
                            }

                            if !loaded {
                                warn!(
                                    "⚠️ [RUST-SYNC] All sources failed for epoch {}. Will retry...",
                                    epoch
                                );
                            }
                        }
                    }

                    // Lookup
                    if let Some(addrs) = cache.get(&epoch) {
                        if leader_author_index < addrs.len() {
                            let addr = &addrs[leader_author_index];
                            if addr.len() == 20 {
                                break Some(addr.clone()); // SUCCESS
                            } else {
                                error!(
                                    "🚨 [FATAL] Invalid address length {} for index {}",
                                    addr.len(),
                                    leader_author_index
                                );
                            }
                        } else {
                            error!(
                                "🚨 [FATAL] Leader index {} out of range (size {})",
                                leader_author_index,
                                addrs.len()
                            );
                        }
                    }
                }

                // 2. Backoff and Retry
                retry_count += 1;
                if retry_count % 10 == 0 {
                    warn!("⏳ [RUST-SYNC] Still waiting for leader address... (epoch={}, index={}, retry={})", 
                        epoch, leader_author_index, retry_count);
                }

                tokio::time::sleep(Duration::from_millis(1000)).await;
            };

            // Send to Go executor - DIRECT SEND (bypass buffer)
            match self
                .executor_client
                .send_committed_subdag_direct(
                    &subdag,
                    commit_data.epoch,
                    commit_data.commit.global_exec_index(),
                    leader_address, // Now properly resolved from cache!
                )
                .await
            {
                Ok(_) => {
                    debug!(
                        "✅ [RUST-SYNC] Sent block {} (commit {}) to Go",
                        commit_data.commit.global_exec_index(),
                        commit_data.commit.index()
                    );
                    blocks_sent += 1;
                }
                Err(e) => {
                    warn!(
                        "⚠️ [RUST-SYNC] Failed to send block {} to Go: {}. Re-queuing {} commits.",
                        commit_data.commit.global_exec_index(),
                        e,
                        ready_commits.len() - idx
                    );
                    failed_at = Some(idx);
                    break;
                }
            }
        }

        // Re-queue all unprocessed commits (from failed_at to end)
        if let Some(start_idx) = failed_at {
            let mut queue = self.block_queue.lock().await;
            // Drain unprocessed commits and re-queue them
            for commit_data in ready_commits.drain(start_idx..) {
                queue.push(commit_data);
            }
        }
        // Successfully processed commits are now dropped, freeing memory

        // Update metrics
        self.metrics
            .blocks_sent_to_go_total
            .inc_by(blocks_sent as f64);

        Ok(blocks_sent)
    }

    /// Check and process pending epoch transitions (from deferred AdvanceEpoch)
    /// This is called after processing sync queue - if Go has synced up to a pending
    /// transition's boundary, we can now safely advance the epoch.
    pub(super) async fn check_and_process_pending_epoch_transitions(&self, go_last_block: u64) {
        // Access the global ConsensusNode to check pending transitions
        use crate::node::get_transition_handler_node;

        if let Some(node) = get_transition_handler_node().await {
            let mut pending_to_process = Vec::new();

            // Scope the lock to avoid holding it across the advance_epoch call
            {
                let node_guard = node.lock().await;
                let mut pending = node_guard.pending_epoch_transitions.lock().await;

                // Check each pending transition
                let mut processed_indices = Vec::new();
                for (idx, trans) in pending.iter().enumerate() {
                    // =============================================================================
                    // CRITICAL FIX: Update epoch_base IMMEDIATELY from pending queue!
                    // =============================================================================
                    let current_base = self.epoch_base_index.load(Ordering::SeqCst);
                    let current_epoch = self.current_epoch.load(Ordering::SeqCst);

                    if trans.epoch > current_epoch || trans.boundary_block > current_base {
                        info!(
                            "🔄 [DEFERRED EPOCH] Updating RustSyncNode to epoch {} (base {} -> {}) for fetching",
                            trans.epoch, current_base, trans.boundary_block
                        );
                        self.current_epoch.store(trans.epoch, Ordering::SeqCst);
                        self.epoch_base_index
                            .store(trans.boundary_block, Ordering::SeqCst);
                    }

                    if go_last_block >= trans.boundary_block {
                        info!(
                            "✅ [DEFERRED EPOCH] Sync complete! Go block {} >= boundary {}. \
                             Processing epoch {} transition.",
                            go_last_block, trans.boundary_block, trans.epoch
                        );
                        pending_to_process.push(trans.clone());
                        processed_indices.push(idx);
                    } else {
                        info!(
                            "⏳ [DEFERRED EPOCH] Still waiting for sync. Go block {} < boundary {} for epoch {}.",
                            go_last_block, trans.boundary_block, trans.epoch
                        );
                    }
                }

                // Remove processed transitions (in reverse order to preserve indices)
                for idx in processed_indices.into_iter().rev() {
                    pending.remove(idx);
                }
            }

            // Process transitions outside the lock
            for trans in pending_to_process {
                let epoch_timer = self.metrics.epoch_transition_duration_seconds.start_timer();
                info!(
                    "📤 [DEFERRED EPOCH] Now calling advance_epoch for epoch {} (boundary: {})",
                    trans.epoch, trans.boundary_block
                );

                if let Err(e) = self
                    .executor_client
                    .advance_epoch(trans.epoch, trans.timestamp_ms, trans.boundary_block)
                    .await
                {
                    warn!(
                        "⚠️ [DEFERRED EPOCH] Failed to advance epoch {}: {}. Will retry next sync cycle.",
                        trans.epoch, e
                    );

                    // Re-queue if failed
                    if let Some(node) = get_transition_handler_node().await {
                        let node_guard = node.lock().await;
                        let mut pending = node_guard.pending_epoch_transitions.lock().await;
                        pending.push(trans);
                    }
                } else {
                    info!(
                        "✅ [DEFERRED EPOCH] Successfully advanced to epoch {} with boundary {}",
                        trans.epoch, trans.boundary_block
                    );

                    // Update Rust epoch tracker
                    self.current_epoch.store(trans.epoch, Ordering::SeqCst);
                    self.epoch_base_index
                        .store(trans.boundary_block, Ordering::SeqCst);

                    // CRITICAL FIX: Trigger full epoch transition to check committee
                    info!(
                        "🔄 [DEFERRED EPOCH] Triggering full transition to check committee and potentially promote to Validator"
                    );

                    // Send epoch transition signal
                    if let Err(e) = self.epoch_transition_sender.send((
                        trans.epoch,
                        trans.timestamp_ms,
                        trans.boundary_block,
                    )) {
                        warn!(
                            "⚠️ [DEFERRED EPOCH] Failed to send epoch transition signal: {}",
                            e
                        );
                    } else {
                        info!(
                            "✅ [DEFERRED EPOCH] Sent epoch transition signal for epoch {} to trigger mode check",
                            trans.epoch
                        );
                    }
                }
                epoch_timer.observe_duration();
            }
        }
    }
}
