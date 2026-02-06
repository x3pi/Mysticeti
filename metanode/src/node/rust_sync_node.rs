// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

//! RustSyncNode - Full Rust P2P Block Sync for SyncOnly Mode
//!
//! Architecture:
//! - Rust fetches commits from validator peers via consensus_core NetworkClient
//! - Rust sends blocks to CommitProcessor for EndOfEpoch detection
//! - Rust sends blocks to Go via ExecutorClient (Go only executes)
//!
//! This replaces Go's network_sync.go for SyncOnly nodes.

use crate::node::executor_client::ExecutorClient;
use anyhow::Result;
use consensus_config::{AuthorityIndex, Committee, NetworkKeyPair};
use consensus_core::{
    Commit, CommitAPI, CommitRange, CommitRef, CommittedSubDag, Context, GlobalCommitInfo,
    NetworkClient, SignedBlock, TonicClient, VerifiedBlock,
};
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_util::bytes::Bytes;
use tracing::{debug, error, info, trace, warn};

use crate::network::peer_rpc::{query_peer_epoch_boundary_data, query_peer_epochs_network};

/// Data stored in the queue for each commit
#[derive(Clone)]
struct CommitData {
    commit: Commit,
    blocks: Vec<VerifiedBlock>,
    epoch: u64,
}

/// BlockQueue - Buffers commits and processes them sequentially
/// Uses BTreeMap for automatic ordering by global_exec_index
struct BlockQueue {
    /// Commits waiting to be processed, keyed by global_exec_index
    pending: BTreeMap<u64, CommitData>,
    /// Next expected global_exec_index
    next_expected: u64,
    /// Last time we logged about a gap
    last_gap_log: Option<std::time::Instant>,
}

impl BlockQueue {
    /// Create a new BlockQueue starting from a specific next_expected index
    /// NOTE: start_index IS the next_expected (caller provides go_last_block + 1)
    fn new(start_index: u64) -> Self {
        Self {
            pending: BTreeMap::new(),
            next_expected: start_index, // Use directly, caller already calculated next
            last_gap_log: None,
        }
    }

    /// Add a commit to the queue (will be deduplicated by index)
    fn push(&mut self, data: CommitData) {
        let index = data.commit.global_exec_index();
        // Only add if not already processed and not already pending
        if index >= self.next_expected && !self.pending.contains_key(&index) {
            self.pending.insert(index, data);
        }
    }

    /// Drain all commits that can be processed sequentially
    /// Returns commits in order, stopping at the first gap
    /// CRITICAL: If there's a large gap (e.g., after epoch transition), auto-adjust next_expected
    fn drain_ready(&mut self) -> Vec<CommitData> {
        let mut ready = Vec::new();

        // EPOCH TRANSITION FIX: If the first pending commit is way ahead of next_expected,
        // this indicates an epoch transition where global_exec_index jumped.
        // Auto-adjust next_expected to match the first pending commit.
        if let Some((&first_index, _)) = self.pending.first_key_value() {
            if first_index > self.next_expected {
                // STRICT SEQ SYNC: User requested NO SKIPPING/JUMPING
                // Even if gap is large, we must wait for missing blocks to be fetched.
                // We just log the gap for visibility.
                let should_log = match self.last_gap_log {
                    Some(last) => last.elapsed().as_secs() > 10,
                    None => true,
                };

                if should_log {
                    warn!(
                        "⏳ [QUEUE] Gap detected: next_expected={} but first_pending={}. \
                         Waiting for missing blocks (NO SKIP allowed).",
                        self.next_expected, first_index
                    );
                    self.last_gap_log = Some(std::time::Instant::now());
                }
            }
        }

        while let Some((&index, _)) = self.pending.first_key_value() {
            if index == self.next_expected {
                // This is the next expected block - take it
                if let Some(data) = self.pending.remove(&index) {
                    ready.push(data);
                    self.next_expected = index + 1;
                }
            } else if index < self.next_expected {
                // Old block, already processed - remove it
                self.pending.remove(&index);
            } else {
                // Gap detected - stop here
                break;
            }
        }

        ready
    }

    /// Get the next expected index
    fn next_expected(&self) -> u64 {
        self.next_expected
    }

    /// Update next_expected to align with Go's progress (AUTHORITATIVE source of truth)
    /// CRITICAL FIX: Always sync queue with Go, not just when Go is ahead.
    /// This fixes the 40K block gap issue where queue jumped ahead while Go was stuck.
    fn sync_with_go(&mut self, go_last_block: u64) {
        let go_next = go_last_block + 1;

        // CASE 1: Go is ahead of queue (normal catch-up)
        if go_next > self.next_expected {
            // Remove any pending blocks that Go already processed
            self.pending.retain(|&idx, _| idx > go_last_block);
            self.next_expected = go_next;
            info!(
                "🔄 [QUEUE-SYNC] Go ahead: aligned queue to next_expected={}",
                self.next_expected
            );
        }
        // CASE 2: Queue is way ahead of Go (BUG FIX - queue jumped ahead while Go was stuck)
        // This happens when buffer is full and blocks can't be sent to Go
        else if self.next_expected > go_next + 100 {
            // Queue is more than 100 blocks ahead of Go - this is abnormal!
            // Reset queue to Go's position to prevent wasted fetching
            warn!(
                "🚨 [QUEUE-SYNC] Queue desync detected! queue_next={} >> go_next={}. Resetting queue.",
                self.next_expected, go_next
            );
            // Clear pending blocks that are way ahead - they'll be re-fetched
            self.pending
                .retain(|&idx, _| idx >= go_next && idx < go_next + 1000);
            self.next_expected = go_next;
        }
        // CASE 3: Queue slightly ahead of Go (normal - blocks in flight)
        // Keep as-is, this is expected during normal sync
    }

    /// Number of pending commits in queue
    fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

/// Handle to control the RustSyncNode
#[allow(dead_code)]
pub struct RustSyncHandle {
    shutdown_tx: Option<oneshot::Sender<()>>,
    pub task_handle: tokio::task::JoinHandle<()>,
}

impl RustSyncHandle {
    /// Create a new RustSyncHandle
    pub fn new(shutdown_tx: oneshot::Sender<()>, task_handle: tokio::task::JoinHandle<()>) -> Self {
        Self {
            shutdown_tx: Some(shutdown_tx),
            task_handle,
        }
    }

    /// Stop the RustSyncNode gracefully
    #[allow(dead_code)]
    pub async fn stop(mut self) {
        info!("🛑 [RUST-SYNC] Stopping...");
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        self.task_handle.abort();
        let _ = tokio::time::timeout(Duration::from_secs(5), self.task_handle).await;
        info!("✅ [RUST-SYNC] Stopped");
    }
}

/// Configuration for RustSyncNode
pub struct RustSyncConfig {
    pub fetch_interval_secs: u64,
    pub turbo_fetch_interval_ms: u64, // Faster interval when catching up
    pub fetch_batch_size: u32,
    pub turbo_batch_size: u32, // Larger batch when catching up
    pub fetch_timeout_secs: u64,
    /// Peer RPC addresses for fallback epoch boundary data fetch
    pub peer_rpc_addresses: Vec<String>,
}

impl Default for RustSyncConfig {
    fn default() -> Self {
        Self {
            fetch_interval_secs: 2,
            turbo_fetch_interval_ms: 50, // OPTIMIZED: 50ms (was 200ms) for fast catchup
            fetch_batch_size: 500,       // OPTIMIZED: 500 (was 100) blocks per fetch
            turbo_batch_size: 2000,      // OPTIMIZED: 2000 (was 500) for aggressive catchup
            fetch_timeout_secs: 30,      // OPTIMIZED: 30s (was 10s) for larger batches
            peer_rpc_addresses: vec![],  // Configure for WAN sync
        }
    }
}

/// RustSyncNode - Syncs blocks from peers via P2P and sends to Go
pub struct RustSyncNode {
    executor_client: Arc<ExecutorClient>,
    network_client: Option<Arc<TonicClient>>,
    epoch_transition_sender: mpsc::UnboundedSender<(u64, u64, u64)>,
    current_epoch: Arc<AtomicU64>,
    #[allow(dead_code)]
    last_synced_commit_index: Arc<AtomicU32>,
    committee: Arc<RwLock<Option<Committee>>>,
    config: RustSyncConfig,
    /// Queue for buffering and sequential block processing
    block_queue: Arc<Mutex<BlockQueue>>,
    /// Base global_exec_index for current epoch (commits in this epoch are indexed from 1)
    /// epoch_local_commit_index = global_exec_index - epoch_base_index
    epoch_base_index: Arc<AtomicU64>,
    /// Multi-epoch cache: epoch -> sorted list of validator ETH addresses
    /// Used for deterministic leader_address resolution during sync
    epoch_eth_addresses: Arc<Mutex<HashMap<u64, Vec<Vec<u8>>>>>,
    // Stored to allow rebuilding TonicClient on committee change
    context: Option<Arc<Context>>,
    network_keypair: Option<NetworkKeyPair>,
}

impl RustSyncNode {
    /// Create a new RustSyncNode
    /// epoch_base_index: the global_exec_index of the last block in the previous epoch
    ///                   (boundary block). Commit index 1 in current epoch = epoch_base_index + 1
    pub fn new(
        executor_client: Arc<ExecutorClient>,
        epoch_transition_sender: mpsc::UnboundedSender<(u64, u64, u64)>,
        initial_epoch: u64,
        initial_global_exec_index: u32,
        epoch_base_index: u64,
    ) -> Self {
        info!(
            "📊 [RUST-SYNC] Creating sync node: epoch={}, global_exec_index={}, epoch_base_index={}",
            initial_epoch, initial_global_exec_index, epoch_base_index
        );
        Self {
            executor_client,
            network_client: None,
            epoch_transition_sender,
            current_epoch: Arc::new(AtomicU64::new(initial_epoch)),
            last_synced_commit_index: Arc::new(AtomicU32::new(initial_global_exec_index)),
            committee: Arc::new(RwLock::new(None)),
            config: RustSyncConfig::default(),
            block_queue: Arc::new(Mutex::new(BlockQueue::new(
                initial_global_exec_index as u64,
            ))),
            epoch_base_index: Arc::new(AtomicU64::new(epoch_base_index)),

            epoch_eth_addresses: Arc::new(Mutex::new(HashMap::new())),
            context: None,
            network_keypair: None,
        }
    }

    /// Initialize P2P networking with Context and NetworkKeyPair
    pub fn with_network(
        mut self,
        context: Arc<Context>,
        network_keypair: NetworkKeyPair,
        committee: Committee,
    ) -> Self {
        let tonic_client = TonicClient::new(context.clone(), network_keypair.clone());
        self.network_client = Some(Arc::new(tonic_client));
        *self.committee.write().unwrap() = Some(committee);
        self.context = Some(context);
        self.network_keypair = Some(network_keypair);
        self
    }

    /// Set peer RPC addresses for fallback epoch boundary data fetch
    pub fn with_peer_rpc_addresses(mut self, addresses: Vec<String>) -> Self {
        self.config.peer_rpc_addresses = addresses;
        self
    }

    /// Start the sync node
    pub fn start(self) -> RustSyncHandle {
        info!(
            "🚀 [RUST-SYNC] Starting P2P block sync for epoch {}, commit_index={}",
            self.current_epoch.load(Ordering::SeqCst),
            self.last_synced_commit_index.load(Ordering::SeqCst)
        );

        let has_network = self.network_client.is_some();
        if has_network {
            info!("🌐 [RUST-SYNC] P2P networking initialized - will fetch from peers");
        } else {
            info!("⚠️ [RUST-SYNC] No P2P network - will only monitor Go progress");
        }

        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();

        let task_handle = tokio::spawn(async move {
            self.sync_loop(&mut shutdown_rx).await;
        });

        RustSyncHandle::new(shutdown_tx, task_handle)
    }

    /// Main sync loop - fetches commits from peers
    /// Uses adaptive interval: turbo mode (200ms) when catching up, normal (2s) otherwise
    /// STABILITY FIX: Tracks consecutive errors and resets connections after 5 failures
    async fn sync_loop(mut self, shutdown_rx: &mut oneshot::Receiver<()>) {
        let normal_interval = Duration::from_secs(self.config.fetch_interval_secs);
        let turbo_interval = Duration::from_millis(self.config.turbo_fetch_interval_ms);
        let mut is_turbo_mode = false;

        // STABILITY FIX: Track consecutive sync errors to detect stale connections
        let mut consecutive_errors: u32 = 0;
        const MAX_CONSECUTIVE_ERRORS: u32 = 5;

        loop {
            // Determine if we're catching up (behind network)
            let catching_up = {
                let queue = self.block_queue.lock().await;
                let pending = queue.pending_count();
                // Turbo mode ONLY if we have pending commits waiting to be drained
                // Previously had `queue.next_expected() > 1` which was always true!
                pending > 0
            };

            // Check if peer epoch is ahead (triggers turbo mode)
            let go_epoch = self.executor_client.get_current_epoch().await.unwrap_or(0);
            let rust_epoch = self.current_epoch.load(Ordering::SeqCst);
            let epoch_behind = rust_epoch < go_epoch;

            let new_turbo = catching_up || epoch_behind;
            if new_turbo != is_turbo_mode {
                is_turbo_mode = new_turbo;
                if is_turbo_mode {
                    info!(
                        "🚀 [RUST-SYNC] TURBO MODE ENABLED - interval={}ms, batch_size={}",
                        self.config.turbo_fetch_interval_ms, self.config.turbo_batch_size
                    );
                } else {
                    info!(
                        "🐢 [RUST-SYNC] Normal mode - interval={}s, batch_size={}",
                        self.config.fetch_interval_secs, self.config.fetch_batch_size
                    );
                }
            }

            let sleep_duration = if is_turbo_mode {
                turbo_interval
            } else {
                normal_interval
            };

            tokio::select! {
                _ = tokio::time::sleep(sleep_duration) => {
                    match self.sync_once().await {
                        Ok(_) => {
                            // Success - reset error counter
                            if consecutive_errors > 0 {
                                info!("✅ [RUST-SYNC] Sync recovered after {} errors", consecutive_errors);
                            }
                            consecutive_errors = 0;
                        }
                        Err(e) => {
                            consecutive_errors += 1;
                            warn!("[RUST-SYNC] Sync error #{}: {}", consecutive_errors, e);

                            // STABILITY FIX: After MAX_CONSECUTIVE_ERRORS, force reset connections
                            // This handles stale connections after Go restart
                            if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                                warn!(
                                    "🔄 [RUST-SYNC] {} consecutive errors detected. Resetting connections to recover from potential stale socket...",
                                    consecutive_errors
                                );
                                self.executor_client.reset_connections().await;
                                consecutive_errors = 0;

                                // Extra delay after reset to allow Go to accept new connection
                                tokio::time::sleep(Duration::from_millis(500)).await;
                            }
                        }
                    }
                }
                _ = &mut *shutdown_rx => {
                    info!("[RUST-SYNC] Shutdown signal received");
                    return;
                }
            }
        }
    }

    /// Single sync iteration using queue-based pattern:
    /// 1. Sync queue with Go's progress (authoritative)
    /// 2. Fetch commits from peers → push to queue
    /// 3. Drain ready commits from queue → send to Go sequentially
    async fn sync_once(&mut self) -> Result<()> {
        // Get current state from Go - this is the AUTHORITATIVE source of truth
        let go_last_block = self.executor_client.get_last_block_number().await?;
        let go_epoch = self.executor_client.get_current_epoch().await?;

        let rust_epoch = self.current_epoch.load(Ordering::SeqCst);
        let _epoch_base = self.epoch_base_index.load(Ordering::SeqCst);

        // =====================================================================
        // AUTO-EPOCH-SYNC: Detect when Go is ahead and auto-update internal state
        // This prevents sync from getting stuck when epoch_monitor advances Go
        // but RustSyncNode's internal state was never updated.
        // =====================================================================
        if go_epoch > rust_epoch {
            info!(
                "🔄 [EPOCH-AUTO-SYNC] Go epoch {} > Rust epoch {}. Updating internal state...",
                go_epoch, rust_epoch
            );

            // Fetch new epoch boundary data from Go
            match self.executor_client.get_epoch_boundary_data(go_epoch).await {
                Ok((_epoch, _ts, new_epoch_base, validators)) => {
                    let old_epoch_base = self.epoch_base_index.load(Ordering::SeqCst);

                    // CRITICAL: Always rebuild committee on epoch change!
                    // Validators may change (added, removed, or replaced) between epochs.
                    // Even if validator COUNT is the same, the actual validators may differ.
                    // The committee MUST match the validators at the epoch boundary block.
                    if !validators.is_empty() {
                        let old_committee_size = {
                            let committee_guard = self.committee.read().unwrap();
                            committee_guard.as_ref().map(|c| c.size()).unwrap_or(0)
                        };

                        info!(
                            "🔄 [EPOCH-AUTO-SYNC] Epoch changed: {} → {}. Rebuilding committee (old={}, new={} validators)...",
                            rust_epoch, go_epoch, old_committee_size, validators.len()
                        );

                        match crate::node::committee::build_committee_from_validator_list(
                            validators, go_epoch,
                        ) {
                            Ok(new_committee) => {
                                info!(
                                    "✅ [EPOCH-AUTO-SYNC] Committee rebuilt with {} authorities for epoch {}",
                                    new_committee.size(), go_epoch
                                );

                                // UPDATE NETWORK CLIENT WITH NEW COMMITTEE
                                // This is crucial because TonicClient holds a Context with the *internal* committee.
                                // If we don't update it, looking up new authorities (e.g. index 4 in size 5)
                                // will PANIC on the old committee (size 4).
                                if let (Some(context), Some(keypair)) =
                                    (&self.context, &self.network_keypair)
                                {
                                    info!("🔄 [EPOCH-AUTO-SYNC] Rebuilding TonicClient with new committee for epoch {}", go_epoch);
                                    let new_context = Arc::new(
                                        (**context).clone().with_committee(new_committee.clone()),
                                    );
                                    let new_client = TonicClient::new(new_context, keypair.clone());
                                    self.network_client = Some(Arc::new(new_client));
                                }

                                *self.committee.write().unwrap() = Some(new_committee);
                            }
                            Err(e) => {
                                warn!(
                                    "⚠️ [EPOCH-AUTO-SYNC] Failed to rebuild committee: {}. Keeping old committee.",
                                    e
                                );
                            }
                        }
                    }

                    info!(
                        "📊 [EPOCH-AUTO-SYNC] Updated: epoch {} → {}, epoch_base {} → {}",
                        rust_epoch, go_epoch, old_epoch_base, new_epoch_base
                    );
                    self.current_epoch.store(go_epoch, Ordering::SeqCst);
                    self.epoch_base_index
                        .store(new_epoch_base, Ordering::SeqCst);
                }
                Err(e) => {
                    warn!(
                        "⚠️ [EPOCH-AUTO-SYNC] Failed to get epoch {} boundary from Go: {}. Will retry next sync cycle.",
                        go_epoch, e
                    );
                }
            }
        } else if go_epoch == rust_epoch {
            // =====================================================================
            // COMMITTEE REFRESH: Check if committee is stale even with same epoch
            // This handles the case where transition.rs updated epoch state but
            // didn't update RustSyncNode's committee (e.g., SyncOnly transition)
            // =====================================================================
            let current_committee_size = {
                let committee_guard = self.committee.read().unwrap();
                committee_guard.as_ref().map(|c| c.size()).unwrap_or(0)
            };

            // Check if we need to refresh committee (e.g., new validator joined)
            match self.executor_client.get_epoch_boundary_data(go_epoch).await {
                Ok((_epoch, _ts, _boundary, validators)) => {
                    if !validators.is_empty() && validators.len() != current_committee_size {
                        info!(
                            "🔄 [COMMITTEE-REFRESH] Committee size mismatch! Current={}, Go has={}. Rebuilding for epoch {}...",
                            current_committee_size, validators.len(), go_epoch
                        );

                        match crate::node::committee::build_committee_from_validator_list(
                            validators, go_epoch,
                        ) {
                            Ok(new_committee) => {
                                info!(
                                    "✅ [COMMITTEE-REFRESH] Committee rebuilt with {} authorities for epoch {}",
                                    new_committee.size(), go_epoch
                                );

                                // UPDATE NETWORK CLIENT WITH NEW COMMITTEE
                                if let (Some(context), Some(keypair)) =
                                    (&self.context, &self.network_keypair)
                                {
                                    info!("🔄 [COMMITTEE-REFRESH] Rebuilding TonicClient with new committee for epoch {}", go_epoch);
                                    let new_context = Arc::new(
                                        (**context).clone().with_committee(new_committee.clone()),
                                    );
                                    let new_client = TonicClient::new(new_context, keypair.clone());
                                    self.network_client = Some(Arc::new(new_client));
                                }

                                *self.committee.write().unwrap() = Some(new_committee);
                            }
                            Err(e) => {
                                warn!(
                                    "⚠️ [COMMITTEE-REFRESH] Failed to rebuild committee: {}. Keeping old committee.",
                                    e
                                );
                            }
                        }
                    }
                }
                Err(_) => {
                    // Ignore errors - this is just a refresh check
                }
            }
        }

        // Re-read epoch values after potential update
        let rust_epoch = self.current_epoch.load(Ordering::SeqCst);
        let epoch_base = self.epoch_base_index.load(Ordering::SeqCst);

        // =====================================================================
        // EPOCH DATA INTEGRITY CHECK
        // Detect corrupted epoch boundary data that happens when:
        // - Node advanced epoch via RPC while Go blocks were not synced
        // - This causes epoch_base to be stuck at an old value (e.g., 4177)
        // - But epoch is high (e.g., 7), making sync impossible
        // =====================================================================
        if rust_epoch > 0 && epoch_base > 0 && (go_last_block as u64) < epoch_base {
            // This is a critical error state!
            // go_last_block should be >= epoch_base for current epoch
            // If go_last_block < epoch_base (strictly less, NOT equal), it means Go is behind the epoch boundary
            // NOTE: go_last_block == epoch_base is VALID at epoch transition!
            warn!(
                "🚨 [RUST-SYNC] EPOCH DATA INTEGRITY ISSUE DETECTED! \
                 go_block={} < epoch_base={} but epoch={}. \
                 This indicates corrupted epoch boundary data (premature epoch advance).",
                go_last_block, epoch_base, rust_epoch
            );

            // Try to refetch epoch boundary from Go Master
            // This might help if Go state was corrected externally
            if let Ok((_epoch, _timestamp, new_boundary, _validators)) = self
                .executor_client
                .get_epoch_boundary_data(rust_epoch)
                .await
            {
                if new_boundary != epoch_base {
                    info!(
                        "🔄 [RUST-SYNC] Updating epoch_base_index: {} -> {} (from Go Master)",
                        epoch_base, new_boundary
                    );
                    self.epoch_base_index.store(new_boundary, Ordering::SeqCst);
                } else {
                    // Even refetched data is same (corrupted) - try peer discovery!
                    warn!(
                        "⚠️ [RUST-SYNC] Go local data still corrupted. Attempting peer discovery for epoch {}...",
                        rust_epoch
                    );

                    // Build peer RPC addresses from committee - extract before await!
                    let peer_addresses: Vec<String> = {
                        let committee_guard = self.committee.read().unwrap();
                        if let Some(ref committee) = *committee_guard {
                            committee
                                .authorities()
                                .filter_map(|(_, auth)| {
                                    // Extract host and use peer_rpc_port (8XXX pattern)
                                    // Consensus address format: /dns/host/tcp/port
                                    let addr_str = auth.address.to_string();
                                    if let Some(host) = addr_str.split('/').nth(2) {
                                        // Use peer RPC port 8000-base pattern
                                        Some(format!("{}:8000", host))
                                    } else {
                                        None
                                    }
                                })
                                .collect()
                        } else {
                            Vec::new()
                        }
                    }; // guard dropped here before await

                    if !peer_addresses.is_empty() {
                        // Query peers for correct epoch boundary
                        match query_peer_epochs_network(&peer_addresses).await {
                            Ok((peer_epoch, peer_block, best_peer)) => {
                                info!(
                                    "🌐 [PEER-DISCOVERY] Best peer: epoch={}, block={}, addr={}",
                                    peer_epoch, peer_block, best_peer
                                );

                                // If peer has the same epoch, query epoch boundary data
                                if peer_epoch == rust_epoch {
                                    match query_peer_epoch_boundary_data(&best_peer, rust_epoch)
                                        .await
                                    {
                                        Ok(boundary_data) => {
                                            let peer_boundary = boundary_data.boundary_block;
                                            if peer_boundary > epoch_base {
                                                info!(
                                                    "✅ [PEER-DISCOVERY] Correcting epoch_base_index: {} -> {} (from peer {})",
                                                    epoch_base, peer_boundary, best_peer
                                                );
                                                self.epoch_base_index
                                                    .store(peer_boundary, Ordering::SeqCst);
                                            } else {
                                                warn!(
                                                    "⚠️ [PEER-DISCOVERY] Peer boundary {} not better than local {}",
                                                    peer_boundary, epoch_base
                                                );
                                            }
                                        }
                                        Err(e) => {
                                            warn!("⚠️ [PEER-DISCOVERY] Failed to get epoch boundary from peer: {}", e);
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("⚠️ [PEER-DISCOVERY] No peers responded: {}", e);
                            }
                        }
                    }

                    // If still not fixed, log error
                    let current_base = self.epoch_base_index.load(Ordering::SeqCst);
                    if go_last_block as u64 <= current_base {
                        error!(
                            "❌ [RUST-SYNC] CRITICAL: Epoch boundary data is still corrupted! \
                             epoch={}, boundary={}, go_block={}. \
                             MANUAL FIX REQUIRED: Clear epoch data in Go and restart node.",
                            rust_epoch, current_base, go_last_block
                        );
                    }
                }
            }
        }

        // PHASE 1: Sync queue with Go's progress
        {
            let mut queue = self.block_queue.lock().await;
            queue.sync_with_go(go_last_block as u64);

            let pending = queue.pending_count();
            let next_expected = queue.next_expected();

            // CRITICAL DEBUG: Log at INFO level to understand queue state
            info!(
                "[RUST-SYNC] State after sync_with_go: go_block={}, go_epoch={}, queue_next_expected={}, pending_count={}, epoch_base={}",
                go_last_block,
                go_epoch,
                next_expected,
                pending,
                self.epoch_base_index.load(Ordering::SeqCst),
            );
        }

        // PHASE 2: Fetch commits from peers and push to queue
        if let Some(ref network_client) = self.network_client {
            // Clone committee before await to drop guard immediately
            let committee_opt: Option<Committee> = {
                let guard = self.committee.read().unwrap();
                guard.clone()
            }; // guard dropped here

            if let Some(ref committee) = committee_opt {
                let fetch_from_index = {
                    let queue = self.block_queue.lock().await;
                    queue.next_expected() - 1 // Fetch from the block before what we need
                };

                info!(
                    "[RUST-SYNC] PHASE 2: Fetching from index {} (committee has {} authorities)",
                    fetch_from_index,
                    committee.size()
                );

                match self
                    .fetch_and_queue(network_client, committee, fetch_from_index as u32)
                    .await
                {
                    Ok(pushed) => {
                        if pushed == 0 {
                            debug!("[RUST-SYNC] PHASE 2: No commits fetched this iteration");
                        }
                    }
                    Err(e) => {
                        warn!("[RUST-SYNC] PHASE 2 Fetch error: {}, will retry", e);
                    }
                }
            } else {
                warn!("[RUST-SYNC] PHASE 2 SKIPPED: committee is None");
            }
        } else {
            // PHASE 2.5: Use Peer Go Sync when network_client is None (SyncOnly mode)
            info!("[RUST-SYNC] PHASE 2.5: No Mysticeti network, using Peer Go Sync");

            let peer_addresses = self.get_peer_go_addresses();
            if !peer_addresses.is_empty() {
                let fetch_from = {
                    let queue = self.block_queue.lock().await;
                    queue.next_expected()
                };

                // Use turbo batch size for faster catchup - SyncOnly without network usually needs to catch up fast
                let batch_size = self.config.turbo_batch_size as u64;

                match self
                    .fetch_blocks_from_peer_go(&peer_addresses, fetch_from, batch_size)
                    .await
                {
                    Ok(synced) => {
                        if synced > 0 {
                            info!("✅ [PEER-GO-SYNC] Synced {} blocks via Peer Go", synced);
                        }
                    }
                    Err(e) => {
                        debug!("[PEER-GO-SYNC] Peer Go sync failed: {}", e);
                    }
                }
            } else {
                warn!("[RUST-SYNC] PHASE 2.5 SKIPPED: No peer addresses available (network_client is None and committee is None)");
            }
        }

        // PHASE 3: Drain ready commits from queue and send to Go
        let blocks_sent = self.process_queue().await?;
        if blocks_sent > 0 {
            info!(
                "📥 [RUST-SYNC] Sent {} blocks to Go (queue-based)",
                blocks_sent
            );
        }
        // =============================================================================
        // PHASE 4: Check pending epoch transitions (from deferred AdvanceEpoch)
        // If sync has caught up to a pending transition's boundary, process it now
        // =============================================================================
        self.check_and_process_pending_epoch_transitions(go_last_block)
            .await;

        // NOTE: Epoch transitions are now handled CENTRALLY by epoch_monitor.rs
        // The unified epoch monitor polls Go/peers every 10s and triggers transitions.
        // This simplifies the sync code and prevents duplicate advance_epoch calls.
        // See epoch_monitor.rs:start_unified_epoch_monitor()

        Ok(())
    }

    async fn fetch_and_queue(
        &self,
        network_client: &Arc<TonicClient>,
        committee: &Committee,
        from_global_exec_index: u32,
    ) -> Result<usize> {
        let timeout = Duration::from_secs(self.config.fetch_timeout_secs);
        let batch_size = self.config.fetch_batch_size;
        let current_epoch = self.current_epoch.load(Ordering::SeqCst);
        let epoch_base = self.epoch_base_index.load(Ordering::SeqCst);

        // 🔍 DIAGNOSTIC: Log all key parameters for debugging cross-epoch sync issues
        info!(
            "🔍 [RUST-SYNC-DIAG] fetch_and_queue called: from_global={}, epoch_base={}, current_epoch={}, queue_next_expected={}",
            from_global_exec_index, epoch_base, current_epoch, from_global_exec_index + 1
        );

        // CRITICAL FIX: Convert global_exec_index to epoch-local commit_index
        // In each epoch, commit_index starts from 1, not from global_exec_index
        // Formula: epoch_local_commit_index = global_exec_index - epoch_base_index
        //
        // BOUNDARY CONDITION FIX: Use >= instead of >
        // When from_global_exec_index == epoch_base (e.g., both are 4177):
        //   - We need to fetch from commit 1 of current epoch
        //   - from_local_commit = 4177 - 4177 = 0
        //   - commit_range = CommitRange(1..=100) ← CORRECT!
        let from_local_commit = if from_global_exec_index as u64 >= epoch_base {
            // Normal case: requesting blocks within current epoch
            (from_global_exec_index as u64 - epoch_base) as u32
        } else if current_epoch == 1 && epoch_base > 0 {
            // CROSS-EPOCH FIX: Need blocks from epoch 0 (while in epoch 1)
            // Epoch 0's base is 0, so commit_index = global_exec_index
            // Peers will serve this from their LegacyEpochStoreManager.
            warn!(
                 "[RUST-SYNC] Fetching pre-epoch blocks for Epoch 1 transition: global={}, epoch_base={}",
                 from_global_exec_index, epoch_base
             );
            from_global_exec_index
        } else {
            // ⚠️ CROSS-EPOCH GAP: from_global_exec_index < epoch_base AND epoch > 1
            // Use the new global range RPC to fetch commits directly by global index!
            warn!(
                "🔄 [RUST-SYNC] CROSS-EPOCH GAP: Go block {} < epoch_base {} (epoch {}). \
                 Using fetch_commits_by_global_range RPC.",
                from_global_exec_index, epoch_base, current_epoch
            );

            // Use global range RPC - it handles epoch boundaries transparently
            return self
                .fetch_and_queue_by_global_range(
                    network_client,
                    committee,
                    from_global_exec_index as u64,
                    (from_global_exec_index + batch_size) as u64,
                )
                .await;
        };

        let commit_range =
            CommitRange::new((from_local_commit + 1)..=(from_local_commit + batch_size));

        info!(
            "[RUST-SYNC] Fetching: global_exec_index={} -> epoch_local_commit={} (epoch_base={}), range={:?}",
            from_global_exec_index, from_local_commit, epoch_base, commit_range
        );

        // =====================================================================
        // PARALLEL FETCH: Race all validators simultaneously for same range
        // First successful response wins - much faster than sequential iteration
        // =====================================================================
        info!(
            "🚀 [PARALLEL-FETCH] Racing {} validators for range {:?}",
            committee.size(),
            commit_range
        );

        let authorities: Vec<_> = committee
            .authorities()
            .filter(|(_, auth)| {
                // Filter out self to avoid "Connection refused" on loopback
                if let Some(keypair) = &self.network_keypair {
                    auth.network_key != keypair.public()
                } else {
                    true
                }
            })
            .map(|(idx, _)| idx)
            .collect();

        // Race pattern: Fetch from ALL peers in parallel, take the FIRST valid response
        // This avoids waiting for timeouts on slow/dead peers
        // SMART SHARDING + HEDGING
        // 1. Deterministically assign a primary peer for this batch
        //    Logic: (start_index) % num_peers.
        //    This distributes load across peers when multiple batches are fetched in parallel.
        if authorities.is_empty() {
            warn!("⚠️ [RUST-SYNC] No peers available (all filtered out or committee empty).");
            return Ok(0);
        }

        let primary_idx = (commit_range.start() as usize) % authorities.len();
        let primary_peer = authorities[primary_idx];

        info!(
            "🎯 [SMART-SHARDING] Assigned primary peer {} for range {:?} (Hedging in 500ms)",
            primary_peer, commit_range
        );

        let mut fetch_futures = FuturesUnordered::new();

        // Helper to spawn a request
        let spawn_req = |peer_idx: AuthorityIndex,
                         client: Arc<TonicClient>,
                         range: CommitRange,
                         timeout: Duration| {
            async move {
                match client.fetch_commits(peer_idx, range.clone(), timeout).await {
                    Ok((commits, certs)) => {
                        if commits.is_empty() {
                            Err(anyhow::anyhow!("Empty response from {}", peer_idx))
                        } else {
                            Ok((peer_idx, commits, certs))
                        }
                    }
                    Err(e) => Err(anyhow::anyhow!("Peer {} failed: {:?}", peer_idx, e)),
                }
            }
        };

        // Start Primary
        fetch_futures.push(spawn_req(
            primary_peer,
            network_client.clone(),
            commit_range.clone(),
            timeout,
        ));

        let mut hedging_timer = Box::pin(tokio::time::sleep(Duration::from_millis(500)));
        let mut hedged = false;

        let mut serialized_commits: Vec<Bytes> = Vec::new();
        let mut winning_peer = None;

        loop {
            tokio::select! {
                // Trigger Hedging if timer expires (and we haven't hedged yet)
                _ = &mut hedging_timer, if !hedged => {
                    hedged = true;
                    if authorities.len() > 1 {
                        info!("🚀 [HEDGING] Primary {} slow (500ms). Racing against others...", primary_peer);
                        for &peer_idx in &authorities {
                            if peer_idx != primary_peer {
                                fetch_futures.push(spawn_req(peer_idx, network_client.clone(), commit_range.clone(), timeout));
                            }
                        }
                    }
                }

                // Process results
                res = fetch_futures.next() => {
                    match res {
                        Some(Ok((peer_idx, commits, _))) => {
                             if !commits.is_empty() {
                                // Winner!
                                info!("✅ [RUST-SYNC] Received data from peer {}", peer_idx);
                                serialized_commits = commits;
                                winning_peer = Some(peer_idx);
                                break; // Stop waiting
                             }
                        }
                        Some(Err(e)) => {
                            trace!("Peer error: {}", e);
                            // If primary failed instantly, trigger hedging NOW
                            if !hedged {
                                hedged = true;
                                info!("⚠️ [RUST-SYNC] Primary failed immediately. Triggering fallback race.");
                                for &peer_idx in &authorities {
                                    if peer_idx != primary_peer {
                                        fetch_futures.push(spawn_req(peer_idx, network_client.clone(), commit_range.clone(), timeout));
                                    }
                                }
                            }
                        }
                        None => break, // All futures finished (and failed)
                    }
                }
            }
        }

        if serialized_commits.is_empty() {
            // Fallback to global range sync
            warn!(
                "⚠️ [PARALLEL-FETCH] All validators returned empty/error for range {:?}. Falling back to global range!",
                commit_range
            );
            return self
                .fetch_and_queue_by_global_range(
                    network_client,
                    committee,
                    from_global_exec_index as u64,
                    (from_global_exec_index + batch_size) as u64,
                )
                .await;
        }

        let authority_idx = winning_peer.unwrap();
        info!(
            "✅ [PARALLEL-FETCH] Peer {} won race with {} commits",
            authority_idx,
            serialized_commits.len()
        );

        // Now process the winning response (same logic as before)
        // Phase 1: Deserialize commits to get block references
        let mut all_block_refs = Vec::new();
        let mut temp_commits = Vec::new();

        for serialized in &serialized_commits {
            match bcs::from_bytes::<Commit>(serialized) {
                Ok(commit) => {
                    // Collect all block refs from this commit
                    for block_ref in commit.blocks() {
                        all_block_refs.push(block_ref.clone());
                    }
                    temp_commits.push(commit);
                }
                Err(e) => {
                    warn!("[RUST-SYNC] Failed to deserialize commit: {}", e);
                }
            }
        }

        if all_block_refs.is_empty() {
            info!(
                "📥 [RUST-SYNC] Fetched {} commits with 0 block refs from peer {}",
                serialized_commits.len(),
                authority_idx
            );
        } else {
            info!(
                "📥 [RUST-SYNC] Fetched {} commits with {} block refs from peer {}, fetching actual blocks...",
                serialized_commits.len(),
                all_block_refs.len(),
                authority_idx
            );
        }

        // Phase 2: Fetch actual DAG blocks using fetch_blocks API
        let mut block_map = std::collections::HashMap::new();

        if !all_block_refs.is_empty() {
            // Deduplicate block refs
            all_block_refs.sort();
            all_block_refs.dedup();

            match network_client
                .fetch_blocks(
                    authority_idx,
                    all_block_refs.clone(),
                    vec![], // No highest_accepted_rounds for commit sync
                    false,  // Not breadth first
                    timeout,
                )
                .await
            {
                Ok(serialized_blocks) => {
                    info!(
                        "📦 [RUST-SYNC] Fetched {} actual blocks from peer {}",
                        serialized_blocks.len(),
                        authority_idx
                    );

                    for serialized in &serialized_blocks {
                        match bcs::from_bytes::<SignedBlock>(serialized) {
                            Ok(signed_block) => {
                                let verified =
                                    VerifiedBlock::new_verified(signed_block, serialized.clone());
                                block_map.insert(verified.reference(), verified);
                            }
                            Err(e) => {
                                warn!("[RUST-SYNC] Failed to deserialize block: {}", e);
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        "⚠️ [RUST-SYNC] Failed to fetch blocks from peer {}: {:?}",
                        authority_idx, e
                    );
                }
            }
        }

        // Phase 3: Push commits with their blocks to queue
        let mut pushed = 0;
        {
            let mut queue = self.block_queue.lock().await;

            for commit in temp_commits {
                // Collect blocks for this commit
                let mut commit_blocks = Vec::new();
                for block_ref in commit.blocks() {
                    if let Some(block) = block_map.get(block_ref) {
                        commit_blocks.push(block.clone());
                    }
                }

                let expected_blocks = commit.blocks().len();
                let actual_blocks = commit_blocks.len();

                if actual_blocks < expected_blocks {
                    debug!(
                        "[RUST-SYNC] Commit {} has {}/{} blocks",
                        commit.index(),
                        actual_blocks,
                        expected_blocks
                    );
                }

                // Log commit details to debug missing blocks
                let global_idx = commit.global_exec_index();
                let commit_idx = commit.index();
                let next_expected = queue.next_expected();
                let will_add =
                    global_idx >= next_expected && !queue.pending.contains_key(&global_idx);

                // 🔍 DIAGNOSTIC: Detect gap and trigger global range fallback
                let gap = if global_idx > next_expected {
                    global_idx - next_expected
                } else {
                    0
                };

                // =================================================================
                // EPOCH MISMATCH DETECTION: If global_idx << next_expected
                // This means peers are sending commits with old global_exec_index
                // which indicates we're requesting from wrong epoch
                // =================================================================
                let negative_gap = if next_expected > global_idx {
                    next_expected - global_idx
                } else {
                    0
                };

                if negative_gap > 1000 && pushed == 0 {
                    warn!(
                        "🚨 [EPOCH-MISMATCH] CRITICAL! commit[{}].global_exec_index={} but queue_next_expected={}. \
                         Negative gap: {} blocks. This indicates EPOCH MISMATCH!",
                        commit_idx, global_idx, next_expected, negative_gap
                    );
                    drop(queue);
                    return Ok(0); // Return 0 pushed to trigger epoch recovery in caller
                }

                if gap > 10 && pushed == 0 {
                    warn!(
                        "🔄 [RUST-SYNC] GAP DETECTED! commit[{}].global_exec_index={} but queue_next_expected={}. \
                         Gap size: {} blocks. Falling back to global range RPC!",
                        commit_idx, global_idx, next_expected, gap
                    );
                    drop(queue);
                    return self
                        .fetch_and_queue_by_global_range(
                            network_client,
                            committee,
                            next_expected,
                            global_idx.saturating_sub(1),
                        )
                        .await;
                }

                if pushed < 5 || (global_idx <= 5) {
                    info!(
                        "📋 [RUST-SYNC] Commit: commit_index={}, global_exec_index={}, blocks={}, will_add={}, queue_next_expected={}{}",
                        commit_idx, global_idx, actual_blocks, will_add, next_expected,
                        if !will_add { " ⚠️ REJECTED (global_idx < next_expected or already pending)" } else { "" }
                    );
                }

                // Push to queue - it will deduplicate and sort automatically
                queue.push(CommitData {
                    commit,
                    blocks: commit_blocks,
                    epoch: current_epoch,
                });
                pushed += 1;
            }
        }

        info!(
            "✅ [RUST-SYNC] Pushed {} commits with {} blocks to queue",
            pushed,
            block_map.len()
        );

        Ok(pushed)
    }

    /// Fetch commits using global execution index range (cross-epoch safe)
    /// This bypasses epoch-local coordinate conversion entirely
    async fn fetch_and_queue_by_global_range(
        &self,
        network_client: &Arc<TonicClient>,
        committee: &Committee,
        start_global_index: u64,
        end_global_index: u64,
    ) -> Result<usize> {
        let timeout = Duration::from_secs(self.config.fetch_timeout_secs);
        let _current_epoch = self.current_epoch.load(Ordering::SeqCst);

        info!(
            "🌐 [GLOBAL-SYNC] Fetching commits by global range [{}, {}]",
            start_global_index, end_global_index
        );

        // Try each validator in turn until we succeed
        for (authority_idx, _authority) in committee.authorities() {
            match network_client
                .fetch_commits_by_global_range(
                    authority_idx,
                    start_global_index,
                    end_global_index,
                    timeout,
                )
                .await
            {
                Ok(global_commits) => {
                    if global_commits.is_empty() {
                        debug!(
                            "[GLOBAL-SYNC] Peer {} returned empty commits for range [{}, {}]",
                            authority_idx, start_global_index, end_global_index
                        );
                        continue;
                    }

                    info!(
                        "📥 [GLOBAL-SYNC] Got {} commits from peer {} for global range [{}, {}]",
                        global_commits.len(),
                        authority_idx,
                        start_global_index,
                        end_global_index
                    );

                    // Collect block refs from all commits
                    let mut all_block_refs: Vec<consensus_types::block::BlockRef> = Vec::new();
                    let mut temp_commits: Vec<(GlobalCommitInfo, Commit)> = Vec::new();

                    for global_info in global_commits {
                        // Deserialize the commit
                        match bcs::from_bytes::<Commit>(&global_info.commit_data) {
                            Ok(commit) => {
                                // Collect block refs
                                for block_ref in commit.blocks() {
                                    all_block_refs.push(block_ref.clone());
                                }
                                temp_commits.push((global_info, commit));
                            }
                            Err(e) => {
                                warn!("[GLOBAL-SYNC] Failed to deserialize commit: {}", e);
                            }
                        }
                    }

                    // Fetch actual blocks
                    let mut block_map = std::collections::HashMap::new();
                    if !all_block_refs.is_empty() {
                        all_block_refs.sort();
                        all_block_refs.dedup();

                        // Convert CommitRef to BlockRef for fetch_blocks
                        let block_refs_for_fetch: Vec<_> = all_block_refs
                            .iter()
                            .filter_map(|cr| {
                                // CommitRef is actually BlockRef in this context
                                Some(cr.clone())
                            })
                            .collect();

                        match network_client
                            .fetch_blocks(
                                authority_idx,
                                block_refs_for_fetch,
                                vec![],
                                false,
                                timeout,
                            )
                            .await
                        {
                            Ok(serialized_blocks) => {
                                for serialized in &serialized_blocks {
                                    match bcs::from_bytes::<SignedBlock>(serialized) {
                                        Ok(signed_block) => {
                                            let verified = VerifiedBlock::new_verified(
                                                signed_block,
                                                serialized.clone(),
                                            );
                                            block_map.insert(verified.reference(), verified);
                                        }
                                        Err(e) => {
                                            warn!(
                                                "[GLOBAL-SYNC] Failed to deserialize block: {}",
                                                e
                                            );
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("[GLOBAL-SYNC] Failed to fetch blocks: {:?}", e);
                            }
                        }
                    }

                    // Push commits with their blocks to queue
                    let mut pushed = 0;
                    {
                        let mut queue = self.block_queue.lock().await;

                        for (global_info, commit) in temp_commits {
                            let mut commit_blocks = Vec::new();
                            for block_ref in commit.blocks() {
                                if let Some(block) = block_map.get(block_ref) {
                                    commit_blocks.push(block.clone());
                                }
                            }

                            let epoch_from_peer = global_info.epoch;
                            let global_idx = global_info.global_exec_index;

                            info!(
                                "📋 [GLOBAL-SYNC] Commit: global_exec_index={}, epoch={}, local_commit={}, blocks={}",
                                global_idx, epoch_from_peer, global_info.local_commit_index, commit_blocks.len()
                            );

                            queue.push(CommitData {
                                commit,
                                blocks: commit_blocks,
                                epoch: epoch_from_peer,
                            });
                            pushed += 1;
                        }
                    }

                    info!(
                        "✅ [GLOBAL-SYNC] Pushed {} commits via global range RPC",
                        pushed
                    );

                    return Ok(pushed);
                }
                Err(e) => {
                    debug!("[GLOBAL-SYNC] Peer {} fetch failed: {:?}", authority_idx, e);
                    continue;
                }
            }
        }

        warn!(
            "[GLOBAL-SYNC] All peers failed for global range [{}, {}]",
            start_global_index, end_global_index
        );
        Ok(0)
    }

    /// Process queue - drain ready commits and send to Go sequentially (Phase 3)
    async fn process_queue(&self) -> Result<u32> {
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
                                // Invalid data - retry fetching? Or just stuck?
                                // For fork safety, we STUCK here. We cannot proceed with invalid data.
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

            // Send to Go executor - MUST succeed before continuing
            match self
                .executor_client
                .send_committed_subdag(
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
                    // commit_data will be dropped after loop when we truncate
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

        Ok(blocks_sent)
    }

    /// Check and process pending epoch transitions (from deferred AdvanceEpoch)
    /// This is called after processing sync queue - if Go has synced up to a pending
    /// transition's boundary, we can now safely advance the epoch.
    async fn check_and_process_pending_epoch_transitions(&self, go_last_block: u64) {
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
                    // This allows sync to fetch commits from the new epoch, even before Go catches up.
                    // Without this, sync tries to fetch from old epoch which peers have already purged.
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

                    // =============================================================================
                    // CRITICAL FIX: Trigger full epoch transition to check committee and promote
                    // to Validator if node was registered in this epoch.
                    //
                    // Without this, a node that was:
                    // 1. Removed from validators in epoch 4
                    // 2. Re-registered in epoch 7
                    // 3. Should become Validator in epoch 8
                    // Will stay in SyncOnly mode because deferred epoch only calls advance_epoch
                    // but doesn't check committee membership for mode transition.
                    //
                    // By sending to epoch_transition_sender, we trigger transition.rs which:
                    // - Fetches committee for the new epoch
                    // - Checks if our protocol_key is in the committee
                    // - Promotes SyncOnly -> Validator if we're in the committee
                    // =============================================================================
                    info!(
                        "🔄 [DEFERRED EPOCH] Triggering full transition to check committee and potentially promote to Validator"
                    );

                    // Send epoch transition signal - same as what EndOfEpoch SystemTx triggers
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
            }
        }
    }
}

// =======================
// Helper for creating RustSyncNode from ConsensusNode
// =======================

/// Start the Rust P2P sync task for SyncOnly nodes
#[allow(dead_code)]
pub async fn start_rust_sync_task(
    executor_client: Arc<ExecutorClient>,
    epoch_transition_sender: mpsc::UnboundedSender<(u64, u64, u64)>,
    initial_epoch: u64,
    initial_commit_index: u32,
    epoch_base_index: u64,
) -> Result<RustSyncHandle> {
    info!("🚀 [RUST-SYNC] Starting Rust P2P sync task");

    let sync_node = RustSyncNode::new(
        executor_client,
        epoch_transition_sender,
        initial_epoch,
        initial_commit_index,
        epoch_base_index,
    );

    Ok(sync_node.start())
}

/// Start the Rust P2P sync task with full networking support
pub async fn start_rust_sync_task_with_network(
    executor_client: Arc<ExecutorClient>,
    epoch_transition_sender: mpsc::UnboundedSender<(u64, u64, u64)>,
    initial_epoch: u64,
    _initial_commit_index: u32, // Deprecated - we now use go_last_block for queue initialization
    context: Arc<Context>,
    network_keypair: NetworkKeyPair,
    committee: Committee,
    peer_rpc_addresses: Vec<String>, // For epoch boundary data fallback
) -> Result<RustSyncHandle> {
    info!("🚀 [RUST-SYNC] Starting Rust P2P sync task with network");

    // CRITICAL FIX: Get Go's last block number to initialize BlockQueue correctly
    // BlockQueue uses global_exec_index, NOT commit_index, so we need go_last_block + 1
    let go_last_block = executor_client.get_last_block_number().await.unwrap_or(0);
    let initial_global_exec_index = go_last_block as u32 + 1;

    // CRITICAL FIX: Get epoch boundary data to calculate epoch_base_index
    // This is needed to convert global_exec_index to epoch-local commit_index when fetching
    let epoch_base_index = match executor_client.get_epoch_boundary_data(initial_epoch).await {
        Ok((_epoch, _timestamp_ms, boundary_block, _validators)) => {
            // boundary_block is the global_exec_index of the last block in the previous epoch
            // For epoch 0, boundary_block is 0
            // For epoch 1, boundary_block is the last block of epoch 0 (e.g., 3633)
            info!(
                "📊 [RUST-SYNC] Got epoch_base_index={} from GetEpochBoundaryData for epoch {}",
                boundary_block, initial_epoch
            );
            boundary_block
        }
        Err(e) => {
            // Fallback: assume epoch 0, base = 0
            warn!(
                "[RUST-SYNC] Failed to get epoch boundary data for epoch {}: {:?}, using base=0",
                initial_epoch, e
            );
            0
        }
    };

    info!(
        "📊 [RUST-SYNC] Initializing BlockQueue with next_expected={} (go_last_block={}), epoch_base_index={}",
        initial_global_exec_index, go_last_block, epoch_base_index
    );

    let sync_node = RustSyncNode::new(
        executor_client,
        epoch_transition_sender,
        initial_epoch,
        initial_global_exec_index, // Use global_exec_index, not commit_index
        epoch_base_index,
    )
    .with_network(context, network_keypair, committee)
    .with_peer_rpc_addresses(peer_rpc_addresses);

    Ok(sync_node.start())
}

// =============================================================================
// PEER GO SYNC: Fetch blocks directly from peer's Go layer
// This enables Rust-centric sync where all sync operations go through Rust
// =============================================================================

impl RustSyncNode {
    /// Fetch blocks from peer's Go layer using PeerGoClient
    /// This is used when network_client is None (SyncOnly) or as a fallback
    /// Returns the number of blocks successfully sent to local Go
    pub async fn fetch_blocks_from_peer_go(
        &self,
        peer_addresses: &[String],
        from_block: u64,
        batch_size: u64,
    ) -> Result<usize> {
        use super::peer_go_client::PeerGoClient;

        if peer_addresses.is_empty() {
            return Err(anyhow::anyhow!("No peer addresses configured"));
        }

        let to_block = from_block + batch_size - 1;

        info!(
            "🌐 [PEER-GO-SYNC] Fetching blocks {} to {} from peer Go layers",
            from_block, to_block
        );

        // Try each peer until one succeeds
        for peer_addr in peer_addresses {
            let client = match PeerGoClient::from_str(peer_addr) {
                Ok(c) => c,
                Err(e) => {
                    debug!(
                        "⚠️ [PEER-GO-SYNC] Invalid peer address {}: {}",
                        peer_addr, e
                    );
                    continue;
                }
            };

            match client.get_blocks_range(from_block, to_block).await {
                Ok(blocks) => {
                    if blocks.is_empty() {
                        debug!("[PEER-GO-SYNC] Peer {} returned 0 blocks", peer_addr);
                        continue;
                    }

                    info!(
                        "✅ [PEER-GO-SYNC] Got {} blocks from peer {}, sending to local Go",
                        blocks.len(),
                        peer_addr
                    );

                    // Send blocks to local Go via ExecutorClient
                    for block in &blocks {
                        // Convert BlockData to ExecutorClient format and send
                        // The executor_client handles the actual commit
                        let block_num = block.block_number;
                        let epoch = block.epoch;

                        // Check epoch transition
                        let current_epoch =
                            self.current_epoch.load(std::sync::atomic::Ordering::SeqCst);
                        if epoch > current_epoch {
                            info!(
                                "🎯 [PEER-GO-SYNC] Epoch transition detected in block {}: {} -> {}",
                                block_num, current_epoch, epoch
                            );

                            // Trigger epoch transition
                            if let Ok((_e, timestamp, boundary, _)) =
                                self.executor_client.get_epoch_boundary_data(epoch).await
                            {
                                let _ = self
                                    .epoch_transition_sender
                                    .send((epoch, timestamp, boundary));
                                self.current_epoch
                                    .store(epoch, std::sync::atomic::Ordering::SeqCst);
                            }
                        }
                    }

                    // Use sync_blocks to write all blocks at once
                    match self.executor_client.sync_blocks(blocks).await {
                        Ok((synced, last_block)) => {
                            info!(
                                "✅ [PEER-GO-SYNC] Synced {} blocks to local Go (last: {})",
                                synced, last_block
                            );
                            return Ok(synced as usize);
                        }
                        Err(e) => {
                            warn!("⚠️ [PEER-GO-SYNC] Failed to sync blocks to local Go: {}", e);
                            return Err(e);
                        }
                    }
                }
                Err(e) => {
                    debug!("[PEER-GO-SYNC] Peer {} fetch failed: {}", peer_addr, e);
                    continue;
                }
            }
        }

        Err(anyhow::anyhow!("All peers failed to provide blocks"))
    }

    /// Get peer Go addresses from committee (if available) or config
    pub fn get_peer_go_addresses(&self) -> Vec<String> {
        let committee_guard = self.committee.read().unwrap();
        if let Some(ref committee) = *committee_guard {
            committee
                .authorities()
                .filter_map(|(_, auth)| {
                    let addr_str = auth.address.to_string();
                    // Extract host from /dns/hostname/tcp/port format
                    if let Some(host) = addr_str.split('/').nth(2) {
                        // Use peer_rpc_port (8000 base)
                        Some(format!("{}:8000", host))
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            Vec::new()
        }
    }
}
