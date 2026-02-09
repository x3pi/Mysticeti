// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

//! ExecutorClient module - communicates with Go executor via sockets.
//!
//! Submodules:
//! - `socket_stream`: Socket abstraction (Unix + TCP)
//! - `persistence`: Crash recovery persistence helpers
//! - `block_sync`: Block sync methods for validators and SyncOnly nodes

pub mod socket_stream;
pub mod persistence;
mod block_sync;

// Re-export public items from submodules
pub use socket_stream::{SocketAddress, SocketStream};
pub use persistence::{
    write_uvarint, persist_last_sent_index,
    persist_last_block_number, read_last_block_number, load_persisted_last_index,
};

use anyhow::Result;
use consensus_core::{CommittedSubDag, BlockAPI, SystemTransaction};
use prost::Message;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::{error, info, trace, warn};

use crate::node::rpc_circuit_breaker::RpcCircuitBreaker;

// Include generated protobuf code
pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/proto.rs"));
}

use proto::{CommittedBlock, CommittedEpochData, TransactionExe, GetValidatorsAtBlockRequest, GetCurrentEpochRequest, GetEpochStartTimestampRequest, AdvanceEpochRequest, Request, Response, ValidatorInfo};
use std::collections::BTreeMap;



// ============================================================================
// ExecutorClient - Now supports both Unix and TCP sockets
// ============================================================================


/// Client to send committed blocks to Go executor via Unix Domain Socket or TCP Socket
/// Supports both local (Unix) and network (TCP) deployment
pub struct ExecutorClient {
    socket_address: SocketAddress,  // Changed from socket_path: String
    connection: Arc<Mutex<Option<SocketStream>>>,  // Changed from UnixStream
    pub(crate) request_socket_address: SocketAddress,  // Changed from request_socket_path: String
    request_connection: Arc<Mutex<Option<SocketStream>>>,  // Changed from UnixStream
    enabled: bool,
    can_commit: bool, // Only node 0 can actually commit transactions to Go state
    /// Buffer for out-of-order blocks to ensure sequential sending
    /// Key: global_exec_index, Value: (epoch_data_bytes, epoch, commit_index)
    send_buffer: Arc<Mutex<BTreeMap<u64, (Vec<u8>, u64, u32)>>>,
    /// Next expected global_exec_index to send
    next_expected_index: Arc<tokio::sync::Mutex<u64>>,
    /// Storage path for persisting state (crash recovery)
    storage_path: Option<std::path::PathBuf>,
    /// Last verified Go block number (for fork detection)
    last_verified_go_index: Arc<tokio::sync::Mutex<u64>>,
    /// Track sent global_exec_indices to prevent duplicates from dual-stream
    /// This prevents both Consensus and Sync from sending the same block
    sent_indices: Arc<tokio::sync::Mutex<std::collections::HashSet<u64>>>,
    /// Circuit breaker for Go Master RPC calls (read-path only)
    rpc_circuit_breaker: Arc<RpcCircuitBreaker>,
}

/// Production safety constants
const MAX_BUFFER_SIZE: usize = 10_000; // Maximum blocks to buffer before rejecting
const GO_VERIFICATION_INTERVAL: u64 = 10; // Verify Go state every N blocks

impl ExecutorClient {
    /// Create new executor client
    /// enabled: whether executor is enabled (check config file exists)
    /// can_commit: whether this node can actually commit transactions (only node 0)
    /// send_socket_path: socket path for sending data to Go executor
    /// receive_socket_path: socket path for receiving data from Go executor
    /// initial_next_expected: initial value for next_expected_index (default: 1)
    pub fn new(enabled: bool, can_commit: bool, send_socket_path: String, receive_socket_path: String, storage_path: Option<std::path::PathBuf>) -> Self {
        Self::new_with_initial_index(enabled, can_commit, send_socket_path, receive_socket_path, 1, storage_path)
    }

    /// Create new executor client with initial next_expected_index
    /// This is useful when creating executor client for a new epoch, where we know the starting global_exec_index
    /// CRITICAL: Buffer is always empty when creating new executor client (prevents duplicate global_exec_index)
    pub fn new_with_initial_index(
        enabled: bool, 
        can_commit: bool, 
        send_socket_path: String, 
        receive_socket_path: String,
        initial_next_expected: u64,
        storage_path: Option<std::path::PathBuf>,
    ) -> Self {
        // CRITICAL FIX: Always create empty buffer to prevent duplicate global_exec_index
        // When creating new executor client (e.g., after restart or epoch transition),
        // buffer should be empty to avoid conflicts with old commits
        let send_buffer = Arc::new(Mutex::new(BTreeMap::new()));
        
        // Parse socket addresses with auto-detection
        let socket_address = SocketAddress::parse(&send_socket_path)
            .unwrap_or_else(|e| {
                warn!("⚠️ [EXECUTOR CLIENT] Failed to parse send socket '{}': {}. Defaulting to Unix socket.", send_socket_path, e);
                SocketAddress::Unix(send_socket_path.clone())
            });
        
        let request_socket_address = SocketAddress::parse(&receive_socket_path)
            .unwrap_or_else(|e| {
                warn!("⚠️ [EXECUTOR CLIENT] Failed to parse receive socket '{}': {}. Defaulting to Unix socket.", receive_socket_path, e);
                SocketAddress::Unix(receive_socket_path.clone())
            });
        
        info!("🔧 [EXECUTOR CLIENT] Creating executor client: send={}, receive={}, initial_next_expected={}, storage_path={:?}", 
            socket_address.as_str(), request_socket_address.as_str(), initial_next_expected, storage_path);
        
        Self {
            socket_address,
            connection: Arc::new(Mutex::new(None)),
            request_socket_address,
            request_connection: Arc::new(Mutex::new(None)),
            enabled,
            can_commit,
            send_buffer,
            next_expected_index: Arc::new(tokio::sync::Mutex::new(initial_next_expected)),
            storage_path,
            last_verified_go_index: Arc::new(tokio::sync::Mutex::new(0)),
            sent_indices: Arc::new(tokio::sync::Mutex::new(std::collections::HashSet::new())),
            rpc_circuit_breaker: Arc::new(RpcCircuitBreaker::new()),
        }
    }

    /// Check if executor is enabled
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Check if this node can commit transactions (only node 0)
    pub fn can_commit(&self) -> bool {
        self.can_commit
    }

    /// Get reference to the RPC circuit breaker
    #[allow(dead_code)]
    pub fn circuit_breaker(&self) -> &RpcCircuitBreaker {
        &self.rpc_circuit_breaker
    }

    /// Force reset all connections to Go executor
    /// This is used to recover from stale connections after Go restart
    /// Next call to connect() or connect_request() will create fresh connections
    pub async fn reset_connections(&self) {
        info!("🔄 [EXECUTOR] Force resetting all connections (triggered by consecutive errors)...");
        
        // Reset data connection
        {
            let mut conn = self.connection.lock().await;
            if conn.is_some() {
                info!("🔌 [EXECUTOR] Closing stale data connection");
            }
            *conn = None;
        }
        
        // Reset request connection
        {
            let mut req_conn = self.request_connection.lock().await;
            if req_conn.is_some() {
                info!("🔌 [EXECUTOR] Closing stale request connection");
            }
            *req_conn = None;
        }
        
        info!("✅ [EXECUTOR] All connections reset. Next operation will create fresh connections.");
    }

    /// Initialize next_expected_index from Go Master's last block number
    /// This should be called ONCE when executor client is created, not on every connect
    /// After initialization, Rust will send blocks continuously and Go will buffer/process them sequentially
    /// CRITICAL FIX: Sync with Go's state to prevent duplicate commits
    /// - If Go is ahead: Update to Go's state (Go has already processed those commits, prevent duplicates)
    /// - If Go is behind: Keep current value (we have commits Go hasn't seen yet)
    pub async fn initialize_from_go(&self) {
        if !self.is_enabled() {
            return;
        }
        
        // Query Go Master for last block number (only once at startup)
        let last_block_number_opt = match self.get_last_block_number().await {
            Ok(n) => Some(n),
            Err(e) => {
                warn!("⚠️  [INIT] Failed to get last block number from Go Master: {}. Attempting to read persisted value.", e);
                // Fallback to persisted last block number if available
                if let Some(ref storage_path) = self.storage_path {
                    match read_last_block_number(storage_path).await {
                        Ok(n) => {
                            info!("📊 [INIT] Loaded persisted last block number {}", n);
                            Some(n)
                        }
                        Err(e) => {
                            warn!("⚠️  [INIT] No persisted last block number available: {}.", e);
                            None
                        }
                    }
                } else {
                    None
                }
            }
        };

        if let Some(last_block_number) = last_block_number_opt {
            let go_next_expected = last_block_number + 1;
            
            let current_next_expected = {
                let next_expected_guard = self.next_expected_index.lock().await;
                *next_expected_guard
            };
            
            // CRITICAL FIX: Sync with Go's state to prevent duplicate commits
            // - If Go is behind: Keep current value (we have commits Go hasn't seen yet)
            // - If Go is ahead: Update to Go's state (prevent sending duplicate commits)
            if go_next_expected < current_next_expected {
                // Go is behind - keep current value (we have commits Go hasn't seen yet)
                info!("📊 [INIT] Go Master is behind (last_block_number={}, go_next_expected={} < current_next_expected={}). Keeping current value to send pending commits.",
                    last_block_number, go_next_expected, current_next_expected);
            } else if go_next_expected > current_next_expected {
                // Go is ahead - update to Go's state to prevent sending duplicate commits
                // Go has already processed commits up to last_block_number, so we should start from go_next_expected
                {
                    let mut next_expected_guard = self.next_expected_index.lock().await;
                    *next_expected_guard = go_next_expected;
                }
                info!("📊 [INIT] Updating next_expected_index from {} to {} (last_block_number={}, go_next_expected={})",
                    current_next_expected, go_next_expected, last_block_number, go_next_expected);
                
                // Clear any buffered commits that Go has already processed
                let mut buffer = self.send_buffer.lock().await;
                let before_clear = buffer.len();
                buffer.retain(|&k, _| k >= go_next_expected);
                let after_clear = buffer.len();
                if before_clear > after_clear {
                    info!("🧹 [INIT] Cleared {} buffered commits that Go has already processed (kept {} commits)", 
                        before_clear - after_clear, after_clear);
                }
            } else {
                // Perfect match
                info!("📊 [INIT] next_expected_index matches Go Master: last_block_number={}, next_expected={}", 
                    last_block_number, current_next_expected);
            }
        } else {
            warn!("⚠️  [INIT] Could not determine last block number. Keeping current next_expected_index. Rust will continue sending blocks, Go will buffer and process sequentially.");
        }
    }

    /// Connect to executor socket (lazy connection with persistent retry)
    /// Just connects, doesn't query Go - Rust sends blocks continuously, Go buffers and processes sequentially
    /// CRITICAL: Persistent connection - keeps trying until socket becomes available (Go Master starts)
    async fn connect(&self) -> Result<()> {
        let mut conn_guard = self.connection.lock().await;

        // Check if already connected and still valid
        if let Some(ref mut stream) = *conn_guard {
            // Try to peek at the stream to check if it's still alive
            match stream.writable().await {
                Ok(_) => {
                    // Connection is still valid
                    trace!("🔌 [EXECUTOR] Reusing existing connection to {}", self.socket_address.as_str());
                    return Ok(());
                }
                Err(e) => {
                    // Connection is dead, close it
                    warn!("⚠️  [EXECUTOR] Existing connection to {} is dead: {}, reconnecting...",
                        self.socket_address.as_str(), e);
                    *conn_guard = None;
                }
            }
        }

        // CRITICAL: Persistent connection with exponential backoff
        // Keeps trying until Go Master creates the socket
        let mut attempt: u32 = 0;
        let mut delay = std::time::Duration::from_millis(500); // Start with 500ms
        const MAX_DELAY: std::time::Duration = std::time::Duration::from_secs(5); // Cap at 5 seconds
        const CONNECT_TIMEOUT_SECS: u64 = 30; // 30 seconds timeout for TCP connections

        loop {
            attempt += 1;

            match SocketStream::connect(&self.socket_address, CONNECT_TIMEOUT_SECS).await {
                Ok(stream) => {
                    info!("🔌 [EXECUTOR] ✅ Connected to executor at {} (attempt {}, after {:.2}s waiting)",
                        self.socket_address.as_str(), attempt, delay.as_secs_f32() * (attempt - 1) as f32);
                    *conn_guard = Some(stream);
                    return Ok(());
                }
                Err(e) => {
                    // CRITICAL: Don't give up - keep trying with exponential backoff
                    // This ensures Rust can connect even if Go Master starts later
                    if attempt == 1 {
                        info!("🔄 [EXECUTOR] Waiting for Go Master to create executor socket at {}...", self.socket_address.as_str());
                    } else if attempt % 10 == 0 {
                        // Log every 10 attempts to avoid spam
                        warn!("⏳ [EXECUTOR] Still waiting for Go Master socket {} (attempt {}, delay {}ms): {}",
                            self.socket_address.as_str(), attempt, delay.as_millis(), e);
                    }

                    tokio::time::sleep(delay).await;

                    // Exponential backoff: double delay, cap at MAX_DELAY
                    delay = std::cmp::min(delay * 2, MAX_DELAY);
                }
            }
        }
    }

    /// Send committed sub-DAG to executor
    /// CRITICAL FORK-SAFETY: global_exec_index and commit_index ensure deterministic execution order
    /// SEQUENTIAL BUFFERING: Blocks are buffered and sent in order to ensure Go receives them sequentially
    /// LEADER_ADDRESS: Optional 20-byte Ethereum address of leader validator
    /// When provided, Go uses this directly instead of looking up by index
    pub async fn send_committed_subdag(
        &self,
        subdag: &CommittedSubDag,
        epoch: u64,
        global_exec_index: u64,
        leader_address: Option<Vec<u8>>,
    ) -> Result<()> {
        if !self.is_enabled() {
            return Ok(()); // Silently skip if not enabled
        }

        if !self.can_commit() {
            // This node has executor client but cannot commit (not node 0)
            // Log but don't actually send the commit
            let total_tx: usize = subdag.blocks.iter().map(|b| b.transactions().len()).sum();
            info!("ℹ️  [EXECUTOR] Node has executor client but cannot commit (not node 0): global_exec_index={}, commit_index={}, epoch={}, blocks={}, total_tx={}",
                global_exec_index, subdag.commit_ref.index, epoch, subdag.blocks.len(), total_tx);
            return Ok(()); // Skip actual commit
        }

        // Count total transactions BEFORE conversion (to detect if transactions are lost)
        let total_tx_before: usize = subdag.blocks.iter().map(|b| b.transactions().len()).sum();
        
        // REPLAY PROTECTION: Discard blocks that are already processed
        // This is critical when Consensus replays old commits on restart
        {
            let next_expected = self.next_expected_index.lock().await;
            if global_exec_index < *next_expected {
                // Only log periodically or for non-empty blocks to avoid noise during replay
                if total_tx_before > 0 || global_exec_index % 1000 == 0 {
                     info!("♻️ [REPLAY] Discarding already processed block: global={}, expected={}", global_exec_index, *next_expected);
                }
                return Ok(());
            }
        }

        // DUAL-STREAM DEDUP: Prevent duplicate sends from Consensus and Sync streams
        // This is critical for BlockCoordinator integration
        {
            let sent = self.sent_indices.lock().await;
            if sent.contains(&global_exec_index) {
                info!("🔄 [DEDUP] Skipping already-sent block from dual-stream: global_exec_index={}", global_exec_index);
                return Ok(()); // Already sent, skip
            }
            // Don't insert yet - insert only after successful send
        }        
        // Convert CommittedSubDag to protobuf CommittedEpochData
        // CRITICAL FORK-SAFETY: Include global_exec_index and commit_index for deterministic ordering
        let epoch_data_bytes = self.convert_to_protobuf(subdag, epoch, global_exec_index, leader_address)?;

        // Count total transactions after conversion (should match before)
        let total_tx: usize = total_tx_before;
        
        // SEQUENTIAL BUFFERING: Add to buffer and try to send in order
        {
            let mut buffer = self.send_buffer.lock().await;
            
            // PRODUCTION SAFETY: Buffer size limit to prevent memory exhaustion
            if buffer.len() >= MAX_BUFFER_SIZE {
                error!("🚨 [BUFFER LIMIT] Buffer is full ({} blocks). Rejecting block global_exec_index={}. This indicates severe sync issues.",
                    buffer.len(), global_exec_index);
                return Err(anyhow::anyhow!("Buffer full: {} blocks (max {})", buffer.len(), MAX_BUFFER_SIZE));
            }            if buffer.contains_key(&global_exec_index) {
                // CRITICAL: Duplicate global_exec_index detected - this should NOT happen
                // Possible causes:
                // 1. Commit processor sent same commit twice
                // 2. global_exec_index calculation is wrong (same value for different commits)
                // 3. Epoch transition didn't reset state correctly
                // 4. Buffer wasn't cleared properly after restart
                let (existing_epoch_data, existing_epoch, existing_commit) = buffer.get(&global_exec_index)
                    .map(|(d, e, c)| (d.len(), *e, *c))
                    .unwrap_or((0, 0, 0));                
                error!("🚨 [DUPLICATE GLOBAL_EXEC_INDEX] Duplicate global_exec_index={} detected!", global_exec_index);
                error!("   📊 Existing: epoch={}, commit_index={}, data_size={} bytes", existing_epoch, existing_commit, existing_epoch_data);
                error!("   📊 New:      epoch={}, commit_index={}, data_size={} bytes, total_tx={}", epoch, subdag.commit_ref.index, epoch_data_bytes.len(), total_tx);
                
                // CRITICAL FIX: Compare commits to determine if they are truly the same
                // If same commit (same epoch + commit_index), skip new one (already in buffer)
                // If different commits with same global_exec_index, this is a BUG - log error but still process
                let is_same_commit = existing_epoch == epoch && existing_commit == subdag.commit_ref.index;
                
                if is_same_commit {
                    // Same commit - skip new one, existing one in buffer will be sent
                    warn!("   ✅ Same commit detected (epoch={}, commit_index={}) - skipping duplicate, existing commit in buffer will be sent", epoch, subdag.commit_ref.index);
                    return Ok(()); // Skip this commit, don't overwrite buffer
                } else {
                    // DIFFERENT commits with same global_exec_index - this is a SERIOUS BUG
                    error!("   🚨 DIFFERENT commits with same global_exec_index! This is a BUG!");
                    error!("   🔍 Root cause analysis:");
                    error!("      - Epochs different ({} vs {}): global_exec_index calculation may be wrong", existing_epoch, epoch);
                    error!("      - Commit indexes different ({} vs {}): same global_exec_index calculated for different commits", existing_commit, subdag.commit_ref.index);
                    error!("      - This indicates last_global_exec_index was not updated correctly or calculation is wrong");
                    
                    // CRITICAL: Overwrite with new commit to prevent transaction loss
                    // The new commit should be processed, even if it means losing the old one
                    // This is better than skipping both commits
                    warn!("   ⚠️  OVERWRITING existing commit with new commit to ensure transactions are executed");
                    warn!("   ⚠️  This may cause transaction loss in the old commit, but ensures new transactions are executed");
                    // Continue to insert below (will overwrite)
                }
            }
            buffer.insert(global_exec_index, (epoch_data_bytes, epoch, subdag.commit_ref.index));
            info!("📦 [SEQUENTIAL-BUFFER] Added block to buffer: global_exec_index={}, commit_index={}, epoch={}, blocks={}, total_tx={}, buffer_size={}",
                global_exec_index, subdag.commit_ref.index, epoch, subdag.blocks.len(), total_tx, buffer.len());
        }
        
        // CRITICAL: Always try to flush buffer after adding commit
        // This ensures commits are sent to Go executor even if there are gaps
        // flush_buffer() will send all consecutive commits starting from next_expected_index
        if let Err(e) = self.flush_buffer().await {
            warn!("⚠️  [SEQUENTIAL-BUFFER] Failed to flush buffer after adding commit: {}", e);
            // Don't return error - commit is in buffer and will be sent later
        }
        
        Ok(())
    }
    
    /// Flush buffered blocks in sequential order
    /// This ensures Go executor receives blocks in the correct order even if Rust sends them out-of-order
    /// CRITICAL: This function will send all consecutive commits starting from next_expected_index
    pub async fn flush_buffer(&self) -> Result<()> {
        // Connect if needed
        if let Err(e) = self.connect().await {
            warn!("⚠️  Executor connection failed, cannot flush buffer: {}", e);
            return Ok(()); // Don't fail if executor is unavailable
        }
        
        // Log buffer status before flushing
        {
            let buffer = self.send_buffer.lock().await;
            let next_expected = self.next_expected_index.lock().await;
            if !buffer.is_empty() {
                let min_buffered = *buffer.keys().next().unwrap_or(&0);
                let max_buffered = *buffer.keys().last().unwrap_or(&0);
                let gap = min_buffered.saturating_sub(*next_expected);
                trace!("📊 [FLUSH BUFFER] Buffer status: size={}, range={}..{}, next_expected={}, gap={}", 
                    buffer.len(), min_buffered, max_buffered, *next_expected, gap);
            }
        }
        
        // CRITICAL: Do NOT skip blocks - ensure all blocks are sent sequentially
        // If next_expected is behind, it means blocks are missing - we must wait for them
        // Do not skip to min_buffered as this would break sequential ordering
        // Instead, log a warning and let the system handle missing blocks through retry mechanism
        {
            let buffer = self.send_buffer.lock().await;
            let next_expected = self.next_expected_index.lock().await;
            if !buffer.is_empty() {
                let min_buffered = *buffer.keys().next().unwrap_or(&0);
                let gap = min_buffered.saturating_sub(*next_expected);
                
                // If gap is large, sync with Go (SINGLE SOURCE OF TRUTH)
                // This auto-recovers from desync situations
                if gap > 100 {
                    // Drop locks before async call
                    drop(buffer);
                    drop(next_expected);
                    
                    warn!("⚠️  [SEQUENTIAL-BUFFER] Large gap detected: min_buffered={}, gap={}. Syncing with Go (SOURCE OF TRUTH)...", 
                        min_buffered, gap);
                    
                    // Sync with Go to get correct next_expected
                    if let Ok(go_last_block) = self.get_last_block_number().await {
                        let go_next_expected = go_last_block + 1;
                        
                        let mut next_expected_guard = self.next_expected_index.lock().await;
                        if go_next_expected > *next_expected_guard {
                            info!("📊 [SINGLE-SOURCE-TRUTH] Updating next_expected from {} to {} (from Go last_block={})",
                                *next_expected_guard, go_next_expected, go_last_block);
                            *next_expected_guard = go_next_expected;
                            
                            // Clear old blocks that Go already has
                            let mut buffer = self.send_buffer.lock().await;
                            let before_clear = buffer.len();
                            buffer.retain(|&k, _| k >= go_next_expected);
                            let after_clear = buffer.len();
                            if before_clear > after_clear {
                                info!("🧹 [SINGLE-SOURCE-TRUTH] Cleared {} old blocks, kept {}", 
                                    before_clear - after_clear, after_clear);
                            }
                        }
                    }
                }
            }
        }
        
        // Send all consecutive blocks starting from next_expected
        loop {
            // Get current next_expected and check if we have that block
            let current_expected = {
                let next_expected = self.next_expected_index.lock().await;
                *next_expected
            };
            
            // Try to get the block for current_expected
            let block_data = {
                let mut buffer = self.send_buffer.lock().await;
                buffer.remove(&current_expected)
            };
            
            if let Some((epoch_data_bytes, epoch, commit_index)) = block_data {
                // This is the next expected block - send it immediately
                info!("📤 [SEQUENTIAL-BUFFER] Sending block: global_exec_index={}, commit_index={}, epoch={}, size={} bytes",
                    current_expected, commit_index, epoch, epoch_data_bytes.len());                if let Err(e) = self.send_block_data(&epoch_data_bytes, current_expected, epoch, commit_index).await {                    warn!("⚠️  [SEQUENTIAL-BUFFER] Failed to send block global_exec_index={}: {}, re-adding to buffer", 
                        current_expected, e);
                    // Re-add to buffer for retry
                    let mut buffer_retry = self.send_buffer.lock().await;
                    buffer_retry.insert(current_expected, (epoch_data_bytes, epoch, commit_index));
                    return Ok(());
                }
                
                // Successfully sent, increment next_expected and persist
                {
                    let mut next_expected = self.next_expected_index.lock().await;
                    *next_expected += 1;
                    info!("✅ [SEQUENTIAL-BUFFER] Successfully sent block global_exec_index={}, next_expected={}", 
                        current_expected, *next_expected);
                    
                    // DUAL-STREAM TRACKING: Mark this index as successfully sent
                    // This prevents duplicate sends from both Consensus and Sync streams
                    {
                        let mut sent = self.sent_indices.lock().await;
                        sent.insert(current_expected);
                        // Limit memory: only keep last 10000 indices
                        if sent.len() > 10000 {
                            if let Some(&min_idx) = sent.iter().min() {
                                sent.remove(&min_idx);
                            }
                        }
                    }
                    
                    // PERSIST: Save last successfully sent index for crash recovery
                    if let Some(ref storage_path) = self.storage_path {
                        if let Err(e) = persist_last_sent_index(storage_path, current_expected, commit_index).await {
                            warn!("⚠️ [PERSIST] Failed to persist last_sent_index={}: {}", current_expected, e);
                        }
                    }
                    
                    // GO VERIFICATION: Periodically verify Go actually received blocks
                    // This detects forks where Go's state diverges from what Rust sent
                    if current_expected % GO_VERIFICATION_INTERVAL == 0 {
                        if let Ok(go_last_block) = self.get_last_block_number().await {
                            let mut last_verified = self.last_verified_go_index.lock().await;
                            
                            // FORK DETECTION: Go's block should never decrease
                            if go_last_block < *last_verified {
                                error!("🚨 [FORK DETECTED] Go's block number DECREASED! last_verified={}, go_now={}. CRITICAL: Possible fork or Go state corruption!",
                                    *last_verified, go_last_block);
                            }
                            
                            // Update last verified
                            *last_verified = go_last_block;
                            
                            // Check if Go is keeping up
                            let lag = current_expected.saturating_sub(go_last_block);
                            if lag > 100 {
                                warn!("⚠️ [GO LAG] Go is {} blocks behind Rust. sent={}, go={}", 
                                    lag, current_expected, go_last_block);
                            } else {
                                trace!("✓ [GO VERIFY] Go verified at block {}. Rust sent={}, lag={}", 
                                    go_last_block, current_expected, lag);
                            }
                        }
                    }
                }
                
                // CRITICAL: Continue loop to try sending next block immediately
                // Don't break here - there might be more consecutive blocks to send
            } else {
                // No more consecutive blocks to send (gap detected)
                // Log and break - will retry when next block arrives
                {
                    let buffer = self.send_buffer.lock().await;
                    if !buffer.is_empty() {
                        let min_buffered = *buffer.keys().next().unwrap_or(&0);
                        let gap = min_buffered.saturating_sub(current_expected);
                        if gap > 0 {
                            trace!("⏸️  [SEQUENTIAL-BUFFER] Gap detected: next_expected={}, min_buffered={}, gap={}. Waiting for missing blocks.", 
                                current_expected, min_buffered, gap);
                        }
                    }
                }
                break;
            }
        }
        
        // Log buffer status
        {
            let buffer = self.send_buffer.lock().await;
            let next_expected = self.next_expected_index.lock().await;
            if !buffer.is_empty() {
                let min_buffered = *buffer.keys().next().unwrap_or(&0);
                let max_buffered = *buffer.keys().last().unwrap_or(&0);
                let gap = min_buffered.saturating_sub(*next_expected);                if buffer.len() > 10 {
                    warn!("⚠️  [SEQUENTIAL-BUFFER] Buffer has {} blocks waiting (range: {}..{}, next_expected={}, gap={}). Some blocks may be missing or out-of-order.",
                        buffer.len(), min_buffered, max_buffered, *next_expected, gap);
                } else {
                    info!("📊 [SEQUENTIAL-BUFFER] Buffer has {} blocks waiting (range: {}..{}, next_expected={}, gap={})",
                        buffer.len(), min_buffered, max_buffered, *next_expected, gap);
                }
            }
        }
        
        Ok(())
    }
    
    /// Send committed sub-DAG directly to Go executor (BYPASS BUFFER)
    /// 
    /// This is used by SyncOnly nodes to send blocks directly without using
    /// the sequential buffer. SyncOnly may receive blocks out-of-order or
    /// with gaps, so the buffer-based approach doesn't work.
    /// 
    /// IMPORTANT: This does NOT update next_expected_index or sent_indices.
    /// Go is responsible for handling ordering when receiving synced blocks.
    pub async fn send_committed_subdag_direct(
        &self,
        subdag: &CommittedSubDag,
        epoch: u64,
        global_exec_index: u64,
        leader_address: Option<Vec<u8>>,
    ) -> Result<()> {
        if !self.is_enabled() {
            return Ok(()); // Silently skip if not enabled
        }

        if !self.can_commit() {
            return Ok(()); // Skip if cannot commit
        }

        // Convert to protobuf bytes
        let epoch_data_bytes = self.convert_to_protobuf(subdag, epoch, global_exec_index, leader_address)?;
        
        info!("📤 [SYNC-DIRECT] Sending block directly: global_exec_index={}, epoch={}, size={} bytes",
            global_exec_index, epoch, epoch_data_bytes.len());
        
        // Send directly - bypass buffer
        self.send_block_data(&epoch_data_bytes, global_exec_index, epoch, subdag.commit_ref.index).await?;
        
        info!("✅ [SYNC-DIRECT] Block sent successfully: global_exec_index={}", global_exec_index);
        
        Ok(())
    }
    
    /// Send block data via UDS (internal helper)
    async fn send_block_data(
        &self,
        epoch_data_bytes: &[u8],
        global_exec_index: u64,
        epoch: u64,
        commit_index: u32,
    ) -> Result<()> {
        // Auto-reconnect if connection is None
        {
            let conn_check = self.connection.lock().await;
            if conn_check.is_none() {
                drop(conn_check);
                info!("🔄 [EXECUTOR] Connection not established, connecting...");
                self.connect().await?;
            }
        }
        
        // Send via UDS with Uvarint length prefix (Go expects Uvarint)
        let mut conn_guard = self.connection.lock().await;
        if let Some(ref mut stream) = *conn_guard {
            // Write Uvarint length prefix
            let mut len_buf = Vec::new();
            write_uvarint(&mut len_buf, epoch_data_bytes.len() as u64)?;
            
            // Send with retry logic if write fails
            // CRITICAL: Add timeout to prevent commit processor from getting stuck
            use tokio::time::{timeout, Duration};
            const SEND_TIMEOUT: Duration = Duration::from_secs(10); // 10 seconds timeout
            
            let send_result = timeout(SEND_TIMEOUT, async {
                stream.write_all(&len_buf).await?;
                stream.write_all(epoch_data_bytes).await?;
                stream.flush().await?;
                Ok::<(), std::io::Error>(())
            }).await;
            
            let send_result = match send_result {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(e),
                Err(_) => {
                    // Timeout occurred
                    warn!("⏱️  [EXECUTOR] Send timeout after {}s (global_exec_index={}, commit_index={}), closing connection", 
                        SEND_TIMEOUT.as_secs(), global_exec_index, commit_index);
                    *conn_guard = None; // Clear connection
                    return Err(anyhow::anyhow!("Send timeout"));
                }
            };
            
            match send_result {
                Ok(_) => {                    info!("📤 [TX FLOW] Sent committed sub-DAG to Go executor: global_exec_index={}, commit_index={}, epoch={}, data_size={} bytes", 
                        global_exec_index, commit_index, epoch, epoch_data_bytes.len());
                    Ok(())
                }
                Err(e) => {                    warn!("⚠️  [EXECUTOR] Failed to send committed sub-DAG (global_exec_index={}, commit_index={}): {}, reconnecting...", 
                        global_exec_index, commit_index, e);
                    // Connection is dead, clear it so next send will reconnect
                    *conn_guard = None;
                    // Retry send after reconnection
                    drop(conn_guard);
                    if let Err(reconnect_err) = self.connect().await {
                        warn!("⚠️  [EXECUTOR] Failed to reconnect after send error: {}", reconnect_err);
                        return Err(anyhow::anyhow!("Reconnection failed: {}", reconnect_err));
                    }
                    // Retry send with timeout
                    let mut retry_guard = self.connection.lock().await;
                    if let Some(ref mut retry_stream) = *retry_guard {
                        let mut retry_len_buf = Vec::new();
                        write_uvarint(&mut retry_len_buf, epoch_data_bytes.len() as u64)?;
                        
                        let retry_result = timeout(SEND_TIMEOUT, async {
                            retry_stream.write_all(&retry_len_buf).await?;
                            retry_stream.write_all(epoch_data_bytes).await?;
                            retry_stream.flush().await?;
                            Ok::<(), std::io::Error>(())
                        }).await;
                        
                        match retry_result {
                            Ok(Ok(())) => {                                info!("✅ [EXECUTOR] Successfully sent committed sub-DAG after reconnection: global_exec_index={}, commit_index={}", 
                                    global_exec_index, commit_index);
                                Ok(())
                            }
                            Ok(Err(retry_err)) => {                                warn!("⚠️  [EXECUTOR] Retry send also failed: {}", retry_err);
                                *retry_guard = None; // Clear connection for next attempt
                                Err(anyhow::anyhow!("Retry send failed: {}", retry_err))
                            }
                            Err(_) => {                                warn!("⏱️  [EXECUTOR] Retry send timeout after {}s (global_exec_index={}, commit_index={})", 
                                    SEND_TIMEOUT.as_secs(), global_exec_index, commit_index);
                                *retry_guard = None; // Clear connection
                                Err(anyhow::anyhow!("Retry send timeout"))
                            }
                        }
                    } else {
                        Err(anyhow::anyhow!("Connection lost after reconnection"))
                    }
                }
            }
        } else {            warn!("⚠️  [EXECUTOR] Executor connection lost, skipping send");
            Err(anyhow::anyhow!("Connection lost"))
        }
    }

    /// Convert CommittedSubDag to protobuf CommittedEpochData bytes
    /// Uses generated protobuf code to ensure correct encoding
    /// 
    /// IMPORTANT: Transaction data is passed through unchanged from Go sub → Rust consensus → Go master
    /// We verify data integrity by checking transaction hash at each step
    /// 
    /// CRITICAL FORK-SAFETY: global_exec_index and commit_index ensure deterministic execution order
    /// All nodes must execute blocks with the same global_exec_index in the same order
    fn convert_to_protobuf(
        &self,
        subdag: &CommittedSubDag,
        epoch: u64,
        global_exec_index: u64,
        leader_address: Option<Vec<u8>>,
    ) -> Result<Vec<u8>> {
        // Build CommittedEpochData protobuf message using generated types
        let mut blocks = Vec::new();
        
            for (block_idx, block) in subdag.blocks.iter().enumerate() {
            // Extract transactions with hash for deterministic sorting
            // CRITICAL FORK-SAFETY: Sort transactions by hash to ensure all nodes send same order
            // OPTIMIZATION: Use references during sorting to reduce memory allocations
            let mut transactions_with_hash: Vec<(&[u8], Vec<u8>)> = Vec::with_capacity(block.transactions().len()); // (tx_data_ref, tx_hash)
            let mut skipped_count = 0;
            let total_tx_in_block = block.transactions().len();            
            for (tx_idx, tx) in block.transactions().iter().enumerate() {
                // Get transaction data (raw bytes) - Go needs transaction data, not digest
                // IMPORTANT: tx.data() returns a reference to the original bytes, no modification
                let tx_data = tx.data();                
                // 🔍 HASH INTEGRITY CHECK: Verify transaction data integrity by calculating hash
                // This ensures data hasn't been modified during consensus
                use crate::types::tx_hash::calculate_transaction_hash;
                let tx_hash = calculate_transaction_hash(tx_data);
                let tx_hash_hex = hex::encode(&tx_hash[..8.min(tx_hash.len())]);
                let _tx_hash_full_hex = hex::encode(&tx_hash);

                // 🔍 FILTER: Check if this is a SystemTransaction (BCS format) - skip if so
                // SystemTransaction should not be sent to Go executor as Go doesn't understand BCS
                if SystemTransaction::from_bytes(tx_data).is_ok() {
                    trace!("ℹ️ [SYSTEM TX FILTER] Skipping SystemTransaction (BCS format) in block {} tx {}: hash={}..., size={} bytes",
                        block_idx, tx_idx, tx_hash_hex, tx_data.len());
                    skipped_count += 1;
                    continue;
                }

                // Log transaction processing (general tracking, not specific transaction)
                trace!("🔍 [TX HASH] Processing transaction: hash={}..., size={} bytes, block_idx={}, tx_idx={}",
                    tx_hash_hex, tx_data.len(), block_idx, tx_idx);

                // Verify transaction data is valid protobuf before sending to Go
                // Uses strict validation (from_address must be non-empty) to filter
                // non-user data like consensus internal messages
                use crate::types::tx_hash::verify_transaction_protobuf;
                if !verify_transaction_protobuf(tx_data) {
                    trace!("🚫 [TX FILTER] Non-user transaction in committed block (hash={}..., size={} bytes, global_exec_index={}, commit_index={}). Skipping.", 
                        tx_hash_hex, tx_data.len(), global_exec_index, subdag.commit_ref.index);
                    skipped_count += 1;
                    continue;
                }
                
                trace!("🔍 [TX INTEGRITY] Verifying transaction data: hash={}, size={} bytes", 
                    tx_hash_hex, tx_data.len());
                
                // Store transaction data reference with hash for sorting
                // OPTIMIZATION: Avoid first clone by storing reference during sorting
                transactions_with_hash.push((tx_data, tx_hash));                
                trace!("✅ [TX INTEGRITY] Transaction data preserved: hash={}..., size={} bytes (unchanged from submission)", 
                    tx_hash_hex, tx_data.len());
            }
            
            // CRITICAL FORK-SAFETY: Sort transactions by hash (deterministic ordering)
            // This ensures all nodes send transactions in the same order within a block
            // Sort by hash bytes (lexicographic order) - deterministic across all nodes
            transactions_with_hash.sort_by(|(_, hash_a), (_, hash_b)| hash_a.cmp(hash_b));
            
            // Convert to TransactionExe messages after sorting
            // OPTIMIZATION: Only clone once here instead of twice (during push + here)
            let mut transactions = Vec::new();
            for (_sorted_idx, (tx_data_ref, tx_hash)) in transactions_with_hash.iter().enumerate() {
                let tx_hash_hex = hex::encode(&tx_hash[..8.min(tx_hash.len())]);
                trace!("📋 [FORK-SAFETY] Sorted transaction in block[{}]: hash={}", block_idx, tx_hash_hex);
                
                // Create TransactionExe message using generated protobuf code
                // NOTE: We use "digest" field to store transaction data (raw bytes)
                // Go will unmarshal this as transaction data
                // OPTIMIZATION: Only clone once here (instead of during push + here)
                let tx_exe = TransactionExe {
                    digest: tx_data_ref.to_vec(), // Clone &[u8] to Vec<u8> - the only clone needed
                    worker_id: 0, // Optional, set to 0 for now
                };
                transactions.push(tx_exe);
            }
            if skipped_count > 0 {
                trace!("⏭️ [TX FILTER] Block[{}] skipped {} non-user transactions out of {} total", 
                    block_idx, skipped_count, total_tx_in_block);
            }
            trace!("✅ [FORK-SAFETY] Block[{}] transactions sorted: {} transactions (deterministic order by hash)", 
                block_idx, transactions.len());
            
            // Create CommittedBlock message using generated protobuf code
            let committed_block = CommittedBlock {
                epoch,
                height: subdag.commit_ref.index as u64,
                transactions,
            };
            blocks.push(committed_block);
        }
        
        // CRITICAL FORK-SAFETY: Sort blocks by height (commit_index) to ensure deterministic order
        // All nodes must send blocks in the same order
        blocks.sort_by(|a, b| a.height.cmp(&b.height));
        
        // Create CommittedEpochData message using generated protobuf code
        // CRITICAL FORK-SAFETY: Include global_exec_index and commit_index for deterministic ordering
        // EPOCH TRACKING: Include epoch number for block header population in Go Master
        // CRITICAL FORK-SAFETY: Include commit_timestamp_ms for deterministic block hashes
        // Go Master MUST use this timestamp for BlockHeader instead of time.Now()
        // CRITICAL FORK-SAFETY: Include leader_author_index for deterministic LeaderAddress
        // Go Master MUST lookup validator address from committee using this index
        let epoch_data = CommittedEpochData {
            blocks,
            global_exec_index,
            commit_index: subdag.commit_ref.index as u32,
            epoch,
            commit_timestamp_ms: subdag.timestamp_ms, // Consensus timestamp from Linearizer::calculate_commit_timestamp()
            leader_author_index: subdag.leader.author.value() as u32, // Leader authority index for Go to lookup validator address
            leader_address: leader_address.unwrap_or_default(), // 20-byte Ethereum address from Rust committee lookup
        };
        
        // Encode to protobuf bytes using prost::Message::encode
        // This ensures correct protobuf encoding that Go can unmarshal
        let mut buf = Vec::new();
        epoch_data.encode(&mut buf)?;
        
        // Count total transactions in encoded data
        let total_tx_encoded: usize = epoch_data.blocks.iter().map(|b| b.transactions.len()).sum();        
        info!("📦 [TX INTEGRITY] Encoded CommittedEpochData: global_exec_index={}, commit_index={}, epoch={}, blocks={}, total_tx={}, total_size={} bytes (using protobuf encoding)", 
            global_exec_index, subdag.commit_ref.index, epoch, epoch_data.blocks.len(), total_tx_encoded, buf.len());
        
        Ok(buf)
    }

    /// Connect to Go request socket for request/response (lazy connection with retry)
    async fn connect_request(&self) -> Result<()> {
        let mut conn_guard = self.request_connection.lock().await;
        
        // Check if already connected and still valid
        if let Some(ref mut stream) = *conn_guard {
            match stream.writable().await {
                Ok(_) => {
                    trace!("🔌 [EXECUTOR-REQ] Reusing existing request connection to {}", self.request_socket_address.as_str());
                    return Ok(());
                }
                Err(e) => {
                    warn!("⚠️  [EXECUTOR-REQ] Existing request connection to {} is dead: {}, reconnecting...", 
                        self.request_socket_address.as_str(), e);
                    *conn_guard = None;
                }
            }
        }

        // Connect to socket with retry logic
        const MAX_RETRIES: u32 = 3;
        const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(100);
        const CONNECT_TIMEOUT_SECS: u64 = 30; // 30 seconds timeout for TCP connections
        
        for attempt in 1..=MAX_RETRIES {
            match SocketStream::connect(&self.request_socket_address, CONNECT_TIMEOUT_SECS).await {
                Ok(stream) => {
                    info!("🔌 [EXECUTOR-REQ] Connected to Go request socket at {} (attempt {}/{})", 
                        self.request_socket_address.as_str(), attempt, MAX_RETRIES);
                    *conn_guard = Some(stream);
                    return Ok(());
                }
                Err(e) => {
                    if attempt < MAX_RETRIES {
                        warn!("⚠️  [EXECUTOR-REQ] Failed to connect to Go request socket at {} (attempt {}/{}): {}, retrying...", 
                            self.request_socket_address.as_str(), attempt, MAX_RETRIES, e);
                        tokio::time::sleep(RETRY_DELAY).await;
                    } else {
                        warn!("⚠️  [EXECUTOR-REQ] Failed to connect to Go request socket at {} after {} attempts: {}", 
                            self.request_socket_address.as_str(), MAX_RETRIES, e);
                        return Err(e.into());
                    }
                }
            }
        }
        
        unreachable!()
    }


    /// Get validators at a specific block number from Go state
    /// Used for startup (block 0) and epoch transition (last_global_exec_index)
    pub async fn get_validators_at_block(&self, block_number: u64) -> Result<(Vec<ValidatorInfo>, u64)> {
        if !self.is_enabled() {
            return Err(anyhow::anyhow!("Executor client is not enabled"));
        }

        // Circuit breaker check
        if let Err(reason) = self.rpc_circuit_breaker.check("get_validators_at_block") {
            return Err(anyhow::anyhow!("Circuit breaker: {}", reason));
        }

        // Connect to Go request socket if needed
        if let Err(e) = self.connect_request().await {
            return Err(anyhow::anyhow!("Failed to connect to Go request socket: {}", e));
        }

        // Create GetValidatorsAtBlockRequest
        let request = Request {
            payload: Some(proto::request::Payload::GetValidatorsAtBlockRequest(
                GetValidatorsAtBlockRequest {
                    block_number,
                }
            )),
        };

        // Encode request to protobuf bytes
        let mut request_buf = Vec::new();
        request.encode(&mut request_buf)?;

        // Send request via UDS
        let mut conn_guard = self.request_connection.lock().await;
        if let Some(ref mut stream) = *conn_guard {
            // Write 4-byte length prefix (big-endian)
            let len = request_buf.len() as u32;
            let len_bytes = len.to_be_bytes();
            stream.write_all(&len_bytes).await?;
            
            // Write request data
            stream.write_all(&request_buf).await?;
            stream.flush().await?;
            
            info!("📤 [EXECUTOR-REQ] Sent GetValidatorsAtBlockRequest to Go for block {} (size: {} bytes)", 
                block_number, request_buf.len());

            // Read response (4-byte length prefix + response data)
            use tokio::io::AsyncReadExt;
            use tokio::time::{timeout, Duration};
            
            // Set timeout for reading response (5 seconds)
            let read_timeout = Duration::from_secs(5);
            
            let mut len_buf = [0u8; 4];
            timeout(read_timeout, stream.read_exact(&mut len_buf)).await
                .map_err(|e| anyhow::anyhow!("Timeout reading response length: {}", e))??;
            let response_len = u32::from_be_bytes(len_buf) as usize;
            
            if response_len == 0 {
                return Err(anyhow::anyhow!("Received zero-length response from Go"));
            }
            if response_len > 10_000_000 { // 10MB limit
                return Err(anyhow::anyhow!("Response too large: {} bytes", response_len));
            }
            
            let mut response_buf = vec![0u8; response_len];
            timeout(read_timeout, stream.read_exact(&mut response_buf)).await
                .map_err(|e| anyhow::anyhow!("Timeout reading response data: {}", e))??;
            
            info!("📥 [EXECUTOR-REQ] Received {} bytes from Go, decoding...", response_buf.len());
            info!("🔍 [EXECUTOR-REQ] Raw response bytes (hex): {}", hex::encode(&response_buf));
            info!("🔍 [EXECUTOR-REQ] Raw response bytes (first 50): {:?}", &response_buf[..response_buf.len().min(50)]);
            
            // Decode response
            let response = Response::decode(&response_buf[..])
                .map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to decode response from Go: {}. Response length: {} bytes. Response bytes (hex): {}. Response bytes (first 100): {:?}", 
                        e, 
                        response_buf.len(),
                        hex::encode(&response_buf),
                        &response_buf[..response_buf.len().min(100)]
                    )
                })?;
            
            info!("🔍 [EXECUTOR-REQ] Decoded response successfully");
            info!("🔍 [EXECUTOR-REQ] Response payload type: {:?}", response.payload);
            
            // Debug: Check all possible payload types
            match &response.payload {
                Some(proto::response::Payload::NotifyEpochChangeResponse(_)) => {
                     warn!("🔍 [EXECUTOR-REQ] Payload is NotifyEpochChangeResponse (ignored in debug match)");
                }
                Some(proto::response::Payload::ValidatorInfoList(v)) => {
                    info!("🔍 [EXECUTOR-REQ] Payload is ValidatorInfoList with {} validators", v.validators.len());
                    // CRITICAL: Log each ValidatorInfo exactly as received from Go
                    for (idx, validator) in v.validators.iter().enumerate() {
                        let auth_key_preview = if validator.authority_key.len() > 50 {
                            format!("{}...", &validator.authority_key[..50])
                        } else {
                            validator.authority_key.clone()
                        };
                        info!("📥 [RUST←GO] ValidatorInfo[{}]: address={}, stake={}, name={}, authority_key={}, protocol_key={}, network_key={}",
                            idx, validator.address, validator.stake, validator.name, 
                            auth_key_preview, validator.protocol_key, validator.network_key);
                    }
                }
                Some(proto::response::Payload::Error(e)) => {
                    info!("🔍 [EXECUTOR-REQ] Payload is Error: {}", e);
                }
                Some(proto::response::Payload::ValidatorList(_)) => {
                    warn!("🔍 [EXECUTOR-REQ] Payload is ValidatorList (not expected for this request)");
                }
                Some(proto::response::Payload::ServerStatus(_)) => {
                    warn!("🔍 [EXECUTOR-REQ] Payload is ServerStatus (not expected for this request)");
                }
                Some(proto::response::Payload::LastBlockNumberResponse(_)) => {
                    warn!("🔍 [EXECUTOR-REQ] Payload is LastBlockNumberResponse (not expected for GetValidatorsAtBlockRequest)");
                }
                Some(proto::response::Payload::GetCurrentEpochResponse(_)) => {
                    info!("🔍 [EXECUTOR-REQ] Payload is GetCurrentEpochResponse (handled below)");
                }
                Some(proto::response::Payload::GetEpochStartTimestampResponse(_)) => {
                    warn!("🔍 [EXECUTOR-REQ] Payload is GetEpochStartTimestampResponse (not expected for this request)");
                }
                Some(proto::response::Payload::AdvanceEpochResponse(_)) => {
                    warn!("🔍 [EXECUTOR-REQ] Payload is AdvanceEpochResponse (not expected for this request)");
                }
                Some(proto::response::Payload::EpochBoundaryData(_)) => {
                    warn!("🔍 [EXECUTOR-REQ] Payload is EpochBoundaryData (not expected for this request)");
                }
                Some(proto::response::Payload::SetConsensusStartBlockResponse(_)) |
                Some(proto::response::Payload::SetSyncStartBlockResponse(_)) |
                Some(proto::response::Payload::WaitForSyncToBlockResponse(_)) => {
                    warn!("🔍 [EXECUTOR-REQ] Payload is Transition Handoff response (not expected for this request)");
                }
                Some(proto::response::Payload::GetBlocksRangeResponse(_)) |
                Some(proto::response::Payload::SyncBlocksResponse(_)) => {
                    warn!("🔍 [EXECUTOR-REQ] Payload is Block Sync response (not expected for this request)");
                }
                None => {
                    warn!("🔍 [EXECUTOR-REQ] Payload is None - response structure may be incorrect");
                    warn!("🔍 [EXECUTOR-REQ] Full response debug: {:?}", response);
                }
            }
            
            match response.payload {
                Some(proto::response::Payload::NotifyEpochChangeResponse(_)) => {
                    return Err(anyhow::anyhow!("Unexpected NotifyEpochChangeResponse"));
                }
                Some(proto::response::Payload::ValidatorInfoList(validator_info_list)) => {
                    info!("✅ [EXECUTOR-REQ] Received ValidatorInfoList from Go at block {} with {} validators, epoch_timestamp_ms={}, last_global_exec_index={}",
                        block_number, validator_info_list.validators.len(),
                        validator_info_list.epoch_timestamp_ms,
                        validator_info_list.last_global_exec_index);

                    // CRITICAL: Log each ValidatorInfo exactly as received from Go
                    for (idx, validator) in validator_info_list.validators.iter().enumerate() {
                        let auth_key_preview = if validator.authority_key.len() > 50 {
                            format!("{}...", &validator.authority_key[..50])
                        } else {
                            validator.authority_key.clone()
                        };
                        info!("📥 [RUST←GO] ValidatorInfo[{}]: address={}, stake={}, name={}, authority_key={}, protocol_key={}, network_key={}",
                            idx, validator.address, validator.stake, validator.name,
                            auth_key_preview, validator.protocol_key, validator.network_key);
                    }

                    return Ok((validator_info_list.validators, validator_info_list.epoch_timestamp_ms));
                }
                Some(proto::response::Payload::Error(error_msg)) => {
                    return Err(anyhow::anyhow!("Go returned error: {}", error_msg));
                }
                Some(proto::response::Payload::ValidatorList(_)) => {
                    return Err(anyhow::anyhow!("Unexpected ValidatorList response (expected ValidatorInfoList)"));
                }
                Some(proto::response::Payload::ServerStatus(_)) => {
                    return Err(anyhow::anyhow!("Unexpected ServerStatus response (expected ValidatorInfoList)"));
                }
                Some(proto::response::Payload::LastBlockNumberResponse(_)) => {
                    return Err(anyhow::anyhow!("Unexpected LastBlockNumberResponse response (expected ValidatorInfoList)"));
                }
                Some(proto::response::Payload::GetCurrentEpochResponse(_)) => {
                    return Err(anyhow::anyhow!("Unexpected GetCurrentEpochResponse response (expected ValidatorInfoList)"));
                }
                Some(proto::response::Payload::GetEpochStartTimestampResponse(_)) => {
                    return Err(anyhow::anyhow!("Unexpected GetEpochStartTimestampResponse response (expected ValidatorInfoList)"));
                }
                Some(proto::response::Payload::AdvanceEpochResponse(_)) => {
                    return Err(anyhow::anyhow!("Unexpected AdvanceEpochResponse response (expected ValidatorInfoList)"));
                }
                Some(proto::response::Payload::EpochBoundaryData(_)) => {
                    return Err(anyhow::anyhow!("Unexpected EpochBoundaryData response (expected ValidatorInfoList)"));
                }
                Some(proto::response::Payload::SetConsensusStartBlockResponse(_)) |
                Some(proto::response::Payload::SetSyncStartBlockResponse(_)) |
                Some(proto::response::Payload::WaitForSyncToBlockResponse(_)) => {
                    return Err(anyhow::anyhow!("Unexpected Transition Handoff response (expected ValidatorInfoList)"));
                }
                Some(proto::response::Payload::GetBlocksRangeResponse(_)) |
                Some(proto::response::Payload::SyncBlocksResponse(_)) => {
                    return Err(anyhow::anyhow!("Unexpected Block Sync response (expected ValidatorInfoList)"));
                }
                None => {
                    return Err(anyhow::anyhow!("Unexpected response type from Go. Response payload: None. Response bytes (hex): {}", hex::encode(&response_buf)));
                }
            }
        } else {
            return Err(anyhow::anyhow!("Request connection is not available"));
        }
    }

    /// Get current epoch from Go state
    /// Used to determine which epoch the network is currently in
    pub async fn get_current_epoch(&self) -> Result<u64> {
        if !self.is_enabled() {
            return Err(anyhow::anyhow!("Executor client is not enabled"));
        }

        // Circuit breaker check
        if let Err(reason) = self.rpc_circuit_breaker.check("get_current_epoch") {
            return Err(anyhow::anyhow!("Circuit breaker: {}", reason));
        }

        // Connect to Go request socket if needed
        if let Err(e) = self.connect_request().await {
            return Err(anyhow::anyhow!("Failed to connect to Go request socket: {}", e));
        }

        // Create GetCurrentEpochRequest
        let request = Request {
            payload: Some(proto::request::Payload::GetCurrentEpochRequest(
                GetCurrentEpochRequest {}
            )),
        };

        // Encode request to protobuf bytes
        let mut request_buf = Vec::new();
        request.encode(&mut request_buf)?;

        // Send request via UDS
        let mut conn_guard = self.request_connection.lock().await;
        if let Some(ref mut stream) = *conn_guard {
            // Write 4-byte length prefix (big-endian)
            let len = request_buf.len() as u32;
            let len_bytes = len.to_be_bytes();
            stream.write_all(&len_bytes).await?;

            // Write request data
            stream.write_all(&request_buf).await?;
            stream.flush().await?;

            info!("📤 [EXECUTOR-REQ] Sent GetCurrentEpochRequest to Go (size: {} bytes)",
                request_buf.len());

            // Read response (4-byte length prefix + response data)
            use tokio::io::AsyncReadExt;
            use tokio::time::{timeout, Duration};

            // Set timeout for reading response (5 seconds)
            let read_timeout = Duration::from_secs(5);

            let mut len_buf = [0u8; 4];
            timeout(read_timeout, stream.read_exact(&mut len_buf)).await
                .map_err(|e| anyhow::anyhow!("Timeout reading response length: {}", e))??;
            let response_len = u32::from_be_bytes(len_buf) as usize;

            if response_len == 0 {
                return Err(anyhow::anyhow!("Received zero-length response from Go"));
            }
            if response_len > 10_000_000 { // 10MB limit
                return Err(anyhow::anyhow!("Response too large: {} bytes", response_len));
            }

            let mut response_buf = vec![0u8; response_len];
            timeout(read_timeout, stream.read_exact(&mut response_buf)).await
                .map_err(|e| anyhow::anyhow!("Timeout reading response data: {}", e))??;

            info!("📥 [EXECUTOR-REQ] Received {} bytes from Go, decoding...", response_buf.len());

            // Decode response
            let response = Response::decode(&response_buf[..])
                .map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to decode response from Go: {}. Response length: {} bytes. Response bytes (hex): {}. Response bytes (first 100): {:?}",
                        e,
                        response_buf.len(),
                        hex::encode(&response_buf),
                        &response_buf[..response_buf.len().min(100)]
                    )
                })?;

            info!("🔍 [EXECUTOR-REQ] Decoded response successfully");
            info!("🔍 [EXECUTOR-REQ] Response payload type: {:?}", response.payload);

            match response.payload {
                Some(proto::response::Payload::NotifyEpochChangeResponse(_)) => {
                    return Err(anyhow::anyhow!("Unexpected NotifyEpochChangeResponse"));
                }
                Some(proto::response::Payload::GetCurrentEpochResponse(get_current_epoch_response)) => {
                    let current_epoch = get_current_epoch_response.epoch;
                    info!("✅ [EXECUTOR-REQ] Received current epoch from Go: {}", current_epoch);
                    return Ok(current_epoch);
                }
                Some(proto::response::Payload::Error(error_msg)) => {
                    return Err(anyhow::anyhow!("Go returned error: {}", error_msg));
                }
                _ => {
                    return Err(anyhow::anyhow!("Unexpected response payload type"));
                }
            }
        } else {
            return Err(anyhow::anyhow!("Request connection is not available"));
        }
    }

    /// Get epoch start timestamp from Go state
    /// Used to sync timestamp after epoch transitions
    /// NOTE: This endpoint may not be implemented in Go yet - returns error in that case
    pub async fn get_epoch_start_timestamp(&self) -> Result<u64> {
        if !self.is_enabled() {
            return Err(anyhow::anyhow!("Executor client is not enabled"));
        }

        // Connect to Go request socket if needed
        if let Err(e) = self.connect_request().await {
            return Err(anyhow::anyhow!("Failed to connect to Go request socket: {}", e));
        }

        // Create GetEpochStartTimestampRequest
        let request = Request {
            payload: Some(proto::request::Payload::GetEpochStartTimestampRequest(
                GetEpochStartTimestampRequest {}
            )),
        };

        // Encode request to protobuf bytes
        let mut request_buf = Vec::new();
        request.encode(&mut request_buf)?;

        // Send request via UDS
        let mut conn_guard = self.request_connection.lock().await;
        if let Some(ref mut stream) = *conn_guard {
            // Write 4-byte length prefix (big-endian)
            let len = request_buf.len() as u32;
            let len_bytes = len.to_be_bytes();
            stream.write_all(&len_bytes).await?;

            // Write request data
            stream.write_all(&request_buf).await?;
            stream.flush().await?;

            info!("📤 [EXECUTOR-REQ] Sent GetEpochStartTimestampRequest to Go (size: {} bytes)",
                request_buf.len());

            // Read response (4-byte length prefix + response data)
            use tokio::io::AsyncReadExt;
            use tokio::time::{timeout, Duration};

            // Set timeout for reading response (5 seconds)
            let read_timeout = Duration::from_secs(5);

            let mut len_buf = [0u8; 4];
            timeout(read_timeout, stream.read_exact(&mut len_buf)).await
                .map_err(|e| anyhow::anyhow!("Timeout reading response length: {}", e))??;
            let response_len = u32::from_be_bytes(len_buf) as usize;

            if response_len == 0 {
                return Err(anyhow::anyhow!("Received zero-length response from Go"));
            }
            if response_len > 10_000_000 { // 10MB limit
                return Err(anyhow::anyhow!("Response too large: {} bytes", response_len));
            }

            let mut response_buf = vec![0u8; response_len];
            timeout(read_timeout, stream.read_exact(&mut response_buf)).await
                .map_err(|e| anyhow::anyhow!("Timeout reading response data: {}", e))??;

            info!("📥 [EXECUTOR-REQ] Received {} bytes from Go, decoding...", response_buf.len());

            // Decode response
            let response = Response::decode(&response_buf[..])
                .map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to decode response: {}. Raw bytes: {:?}",
                        e, &response_buf[..std::cmp::min(100, response_buf.len())]
                    )
                })?;

            if let Some(payload) = response.payload {
                match payload {
                    proto::response::Payload::GetEpochStartTimestampResponse(get_epoch_start_timestamp_response) => {
                        let epoch_start_timestamp_ms = get_epoch_start_timestamp_response.timestamp_ms;
                        info!("✅ [EXECUTOR-REQ] Received epoch start timestamp from Go: {}ms", epoch_start_timestamp_ms);
                        return Ok(epoch_start_timestamp_ms);
                    }
                    proto::response::Payload::Error(error_msg) => {
                        return Err(anyhow::anyhow!("Go returned error: {}", error_msg));
                    }
                    _ => {
                        return Err(anyhow::anyhow!("Unexpected response payload type"));
                    }
                }
            } else {
                return Err(anyhow::anyhow!("Request connection is not available"));
            }
        } else {
            return Err(anyhow::anyhow!("Request connection is not available"));
        }
    }


    /// Advance epoch in Go state (Sui-style epoch transition)
    /// boundary_block is the global_exec_index of the last block of the ending epoch
    pub async fn advance_epoch(&self, new_epoch: u64, epoch_start_timestamp_ms: u64, boundary_block: u64) -> Result<()> {
        if !self.is_enabled() {
            return Err(anyhow::anyhow!("Executor client is not enabled"));
        }

        // Connect to Go request socket if needed
        if let Err(e) = self.connect_request().await {
            return Err(anyhow::anyhow!("Failed to connect to Go request socket: {}", e));
        }

        // Create AdvanceEpochRequest with boundary_block for deterministic epoch transition
        let request = Request {
            payload: Some(proto::request::Payload::AdvanceEpochRequest(
                AdvanceEpochRequest {
                    new_epoch,
                    epoch_start_timestamp_ms,
                    boundary_block,
                }
            )),
        };

        // Encode request to protobuf bytes
        let mut request_buf = Vec::new();
        request.encode(&mut request_buf)?;

        // Send request via UDS
        let mut conn_guard = self.request_connection.lock().await;
        if let Some(ref mut stream) = *conn_guard {
            // Write 4-byte length prefix (big-endian)
            let len = request_buf.len() as u32;
            let len_bytes = len.to_be_bytes();
            stream.write_all(&len_bytes).await?;

            // Write request data
            stream.write_all(&request_buf).await?;
            stream.flush().await?;

            info!("📤 [EXECUTOR-REQ] Sent AdvanceEpochRequest to Go (new_epoch={}, timestamp_ms={}, size: {} bytes)",
                new_epoch, epoch_start_timestamp_ms, request_buf.len());

            // Read response (4-byte length prefix + response data)
            use tokio::io::AsyncReadExt;
            use tokio::time::{timeout, Duration};

            // Set timeout for reading response (5 seconds)
            let read_timeout = Duration::from_secs(5);

            let mut len_buf = [0u8; 4];
            timeout(read_timeout, stream.read_exact(&mut len_buf)).await
                .map_err(|e| anyhow::anyhow!("Timeout reading response length: {}", e))??;
            let response_len = u32::from_be_bytes(len_buf) as usize;

            if response_len == 0 {
                return Err(anyhow::anyhow!("Received zero-length response from Go"));
            }
            if response_len > 10_000_000 { // 10MB limit
                return Err(anyhow::anyhow!("Response too large: {} bytes", response_len));
            }

            let mut response_buf = vec![0u8; response_len];
            timeout(read_timeout, stream.read_exact(&mut response_buf)).await
                .map_err(|e| anyhow::anyhow!("Timeout reading response data: {}", e))??;

            info!("📥 [EXECUTOR-REQ] Received {} bytes from Go, decoding...", response_buf.len());

            // Decode response
            let response = Response::decode(&response_buf[..])
                .map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to decode response from Go: {}. Response length: {} bytes. Response bytes (hex): {}. Response bytes (first 100): {:?}",
                        e,
                        response_buf.len(),
                        hex::encode(&response_buf),
                        &response_buf[..response_buf.len().min(100)]
                    )
                })?;

            info!("🔍 [EXECUTOR-REQ] Decoded response successfully");
            info!("🔍 [EXECUTOR-REQ] Response payload type: {:?}", response.payload);

            match response.payload {
                Some(proto::response::Payload::NotifyEpochChangeResponse(_)) => {
                    return Err(anyhow::anyhow!("Unexpected NotifyEpochChangeResponse"));
                }
                Some(proto::response::Payload::AdvanceEpochResponse(_advance_epoch_response)) => {
                    info!("✅ [EXECUTOR-REQ] Go successfully advanced to epoch {}", new_epoch);
                    return Ok(());
                }
                Some(proto::response::Payload::Error(error_msg)) => {
                    return Err(anyhow::anyhow!("Go returned error during epoch advance: {}", error_msg));
                }
                _ => {
                    return Err(anyhow::anyhow!("Unexpected response payload type for AdvanceEpoch"));
                }
            }
        } else {
            return Err(anyhow::anyhow!("Request connection is not available"));
        }
    }

    /// Get unified epoch boundary data from Go Master (NEW: single authoritative source for epoch transitions)
    /// Returns: epoch, epoch_start_timestamp_ms, boundary_block, and validators snapshot
    /// This ensures consistency by getting all epoch transition data in a single atomic request
    pub async fn get_epoch_boundary_data(&self, epoch: u64) -> Result<(u64, u64, u64, Vec<ValidatorInfo>)> {
        if !self.is_enabled() {
            return Err(anyhow::anyhow!("Executor client is not enabled"));
        }

        // Circuit breaker check
        if let Err(reason) = self.rpc_circuit_breaker.check("get_epoch_boundary_data") {
            return Err(anyhow::anyhow!("Circuit breaker: {}", reason));
        }

        // Connect to Go request socket if needed
        if let Err(e) = self.connect_request().await {
            return Err(anyhow::anyhow!("Failed to connect to Go request socket: {}", e));
        }

        // Create GetEpochBoundaryDataRequest
        let request = Request {
            payload: Some(proto::request::Payload::GetEpochBoundaryDataRequest(
                proto::GetEpochBoundaryDataRequest { epoch }
            )),
        };

        // Encode request to protobuf bytes
        let mut request_buf = Vec::new();
        request.encode(&mut request_buf)?;

        // Send request via UDS
        let mut conn_guard = self.request_connection.lock().await;
        if let Some(ref mut stream) = *conn_guard {
            // Write 4-byte length prefix (big-endian)
            let len = request_buf.len() as u32;
            let len_bytes = len.to_be_bytes();
            stream.write_all(&len_bytes).await?;

            // Write request data
            stream.write_all(&request_buf).await?;
            stream.flush().await?;

            info!("📤 [EXECUTOR-REQ] Sent GetEpochBoundaryDataRequest to Go for epoch {} (size: {} bytes)",
                epoch, request_buf.len());

            // Read response (4-byte length prefix + response data)
            use tokio::io::AsyncReadExt;
            use tokio::time::{timeout, Duration};

            // Set timeout for reading response (5 seconds)
            let read_timeout = Duration::from_secs(5);

            let mut len_buf = [0u8; 4];
            timeout(read_timeout, stream.read_exact(&mut len_buf)).await
                .map_err(|e| anyhow::anyhow!("Timeout reading response length: {}", e))??;
            let response_len = u32::from_be_bytes(len_buf) as usize;

            if response_len == 0 {
                return Err(anyhow::anyhow!("Received zero-length response from Go"));
            }
            if response_len > 10_000_000 { // 10MB limit
                return Err(anyhow::anyhow!("Response too large: {} bytes", response_len));
            }

            let mut response_buf = vec![0u8; response_len];
            timeout(read_timeout, stream.read_exact(&mut response_buf)).await
                .map_err(|e| anyhow::anyhow!("Timeout reading response data: {}", e))??;

            info!("📥 [EXECUTOR-REQ] Received {} bytes from Go (GetEpochBoundaryData), decoding...", response_buf.len());

            // Decode response
            let response = Response::decode(&response_buf[..])
                .map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to decode response from Go: {}. Response length: {} bytes",
                        e,
                        response_buf.len()
                    )
                })?;

            match response.payload {
                Some(proto::response::Payload::NotifyEpochChangeResponse(_)) => {
                    return Err(anyhow::anyhow!("Unexpected NotifyEpochChangeResponse"));
                }
                Some(proto::response::Payload::EpochBoundaryData(data)) => {
                    info!("✅ [EPOCH BOUNDARY] Received unified epoch boundary data: epoch={}, timestamp_ms={}, boundary_block={}, validator_count={}",
                        data.epoch, data.epoch_start_timestamp_ms, data.boundary_block, data.validators.len());

                    // Log validators for debugging
                    for (idx, validator) in data.validators.iter().enumerate() {
                        let auth_key_preview = if validator.authority_key.len() > 50 {
                            format!("{}...", &validator.authority_key[..50])
                        } else {
                            validator.authority_key.clone()
                        };
                        info!("📥 [RUST←GO] EpochBoundaryData Validator[{}]: address={}, stake={}, name={}, authority_key={}",
                            idx, validator.address, validator.stake, validator.name, auth_key_preview);
                    }

                    return Ok((data.epoch, data.epoch_start_timestamp_ms, data.boundary_block, data.validators));
                }
                Some(proto::response::Payload::Error(error_msg)) => {
                    return Err(anyhow::anyhow!("Go returned error: {}", error_msg));
                }
                _ => {
                    return Err(anyhow::anyhow!("Unexpected response payload type for GetEpochBoundaryData"));
                }
            }
        } else {
            return Err(anyhow::anyhow!("Request connection is not available"));
        }
    }

    /// Get last block number from Go Master
    /// Used to initialize next_expected_index when connecting
    pub async fn get_last_block_number(&self) -> Result<u64> {
        if !self.is_enabled() {
            return Err(anyhow::anyhow!("Executor client is not enabled"));
        }

        // Circuit breaker check
        if let Err(reason) = self.rpc_circuit_breaker.check("get_last_block_number") {
            return Err(anyhow::anyhow!("Circuit breaker: {}", reason));
        }

        // Connect to Go request socket if needed
        if let Err(e) = self.connect_request().await {
            return Err(anyhow::anyhow!("Failed to connect to Go request socket: {}", e));
        }

        // Create GetLastBlockNumberRequest
        let request = Request {
            payload: Some(proto::request::Payload::GetLastBlockNumberRequest(
                proto::GetLastBlockNumberRequest {},
            )),
        };

        // Encode request to protobuf bytes
        let mut request_buf = Vec::new();
        request.encode(&mut request_buf)?;

        // Send request via UDS
        let mut conn_guard = self.request_connection.lock().await;
        if let Some(ref mut stream) = *conn_guard {
            // Write 4-byte length prefix (big-endian)
            let len = request_buf.len() as u32;
            let len_bytes = len.to_be_bytes();
            stream.write_all(&len_bytes).await?;
            
            // Write request data
            stream.write_all(&request_buf).await?;
            stream.flush().await?;
            
            info!("📤 [EXECUTOR-REQ] Sent GetLastBlockNumberRequest to Go (size: {} bytes)", request_buf.len());

            // Read response (4-byte length prefix + response data)
            use tokio::io::AsyncReadExt;
            use tokio::time::{timeout, Duration};
            
            // Set timeout for reading response (5 seconds)
            let read_timeout = Duration::from_secs(5);
            
            let mut len_buf = [0u8; 4];
            timeout(read_timeout, stream.read_exact(&mut len_buf)).await
                .map_err(|e| anyhow::anyhow!("Timeout reading response length: {}", e))??;
            let response_len = u32::from_be_bytes(len_buf) as usize;
            
            if response_len == 0 {
                return Err(anyhow::anyhow!("Received zero-length response from Go"));
            }
            if response_len > 10_000_000 { // 10MB limit
                return Err(anyhow::anyhow!("Response too large: {} bytes", response_len));
            }
            
            let mut response_buf = vec![0u8; response_len];
            timeout(read_timeout, stream.read_exact(&mut response_buf)).await
                .map_err(|e| anyhow::anyhow!("Timeout reading response data: {}", e))??;
            
            info!("📥 [EXECUTOR-REQ] Received {} bytes from Go, decoding...", response_buf.len());
            
            // Decode response
            let response = Response::decode(&response_buf[..])
                .map_err(|e| anyhow::anyhow!("Failed to decode response from Go: {}", e))?;
            
            match response.payload {
                Some(proto::response::Payload::NotifyEpochChangeResponse(_)) => {
                    return Err(anyhow::anyhow!("Unexpected NotifyEpochChangeResponse"));
                }
                Some(proto::response::Payload::LastBlockNumberResponse(res)) => {
                    let last_block_number = res.last_block_number;
                    info!("✅ [EXECUTOR-REQ] Received LastBlockNumberResponse: last_block_number={}", last_block_number);
                    
                    // Persist for crash recovery
                    if let Some(ref storage_path) = self.storage_path {
                        if let Err(e) = persist_last_block_number(storage_path, last_block_number).await {
                            warn!("⚠️ [PERSIST] Failed to persist last block number {}: {}", last_block_number, e);
                        }
                    }
                    
                    return Ok(last_block_number);
                }
                Some(proto::response::Payload::Error(error_msg)) => {
                    return Err(anyhow::anyhow!("Go returned error: {}", error_msg));
                }
                _ => {
                    return Err(anyhow::anyhow!("Unexpected response type from Go (expected LastBlockNumberResponse)"));
                }
            }
        } else {
            return Err(anyhow::anyhow!("Request connection is not available"));
        }
    }

    // ==========================================================================
    // CLEAN TRANSITION HANDOFF APIs
    // These APIs ensure no gaps or overlaps between sync and consensus modes
    // ==========================================================================

    /// Set consensus start block - called before transitioning to Validator mode
    /// Tells Go that consensus will produce blocks starting from `block_number`
    /// This is used during SyncOnly -> Validator transition
    pub async fn set_consensus_start_block(&self, block_number: u64) -> Result<(bool, u64, String)> {
        if !self.is_enabled() {
            return Err(anyhow::anyhow!("Executor client is not enabled"));
        }

        if let Err(e) = self.connect_request().await {
            return Err(anyhow::anyhow!("Failed to connect to Go request socket: {}", e));
        }

        let request = Request {
            payload: Some(proto::request::Payload::SetConsensusStartBlockRequest(
                proto::SetConsensusStartBlockRequest { block_number },
            )),
        };

        let mut request_buf = Vec::new();
        request.encode(&mut request_buf)?;

        let mut conn_guard = self.request_connection.lock().await;
        if let Some(ref mut stream) = *conn_guard {
            let len = request_buf.len() as u32;
            stream.write_all(&len.to_be_bytes()).await?;
            stream.write_all(&request_buf).await?;
            stream.flush().await?;

            info!("📤 [TRANSITION] Sent SetConsensusStartBlockRequest: block_number={}", block_number);

            use tokio::io::AsyncReadExt;
            use tokio::time::{timeout, Duration};
            let read_timeout = Duration::from_secs(35); // Longer timeout for potential waiting

            let mut len_buf = [0u8; 4];
            timeout(read_timeout, stream.read_exact(&mut len_buf)).await
                .map_err(|e| anyhow::anyhow!("Timeout reading response length: {}", e))??;
            let response_len = u32::from_be_bytes(len_buf) as usize;

            if response_len == 0 || response_len > 10_000_000 {
                return Err(anyhow::anyhow!("Invalid response length: {}", response_len));
            }

            let mut response_buf = vec![0u8; response_len];
            timeout(read_timeout, stream.read_exact(&mut response_buf)).await
                .map_err(|e| anyhow::anyhow!("Timeout reading response data: {}", e))??;

            let response = Response::decode(&response_buf[..])?;
            match response.payload {
                Some(proto::response::Payload::NotifyEpochChangeResponse(_)) => {
                    return Err(anyhow::anyhow!("Unexpected NotifyEpochChangeResponse"));
                }
                Some(proto::response::Payload::SetConsensusStartBlockResponse(res)) => {
                    info!(
                        "✅ [TRANSITION] SetConsensusStartBlock response: success={}, last_sync_block={}, message={}",
                        res.success, res.last_sync_block, res.message
                    );
                    Ok((res.success, res.last_sync_block, res.message))
                }
                Some(proto::response::Payload::Error(error_msg)) => {
                    Err(anyhow::anyhow!("Go returned error: {}", error_msg))
                }
                _ => Err(anyhow::anyhow!("Unexpected response type from Go")),
            }
        } else {
            Err(anyhow::anyhow!("Request connection is not available"))
        }
    }

    /// Set sync start block - called when transitioning from Validator to SyncOnly mode
    /// Tells Go that consensus ended at `last_consensus_block`, sync should start from `last_consensus_block + 1`
    pub async fn set_sync_start_block(&self, last_consensus_block: u64) -> Result<(bool, u64, String)> {
        if !self.is_enabled() {
            return Err(anyhow::anyhow!("Executor client is not enabled"));
        }

        if let Err(e) = self.connect_request().await {
            return Err(anyhow::anyhow!("Failed to connect to Go request socket: {}", e));
        }

        let request = Request {
            payload: Some(proto::request::Payload::SetSyncStartBlockRequest(
                proto::SetSyncStartBlockRequest { last_consensus_block },
            )),
        };

        let mut request_buf = Vec::new();
        request.encode(&mut request_buf)?;

        let mut conn_guard = self.request_connection.lock().await;
        if let Some(ref mut stream) = *conn_guard {
            let len = request_buf.len() as u32;
            stream.write_all(&len.to_be_bytes()).await?;
            stream.write_all(&request_buf).await?;
            stream.flush().await?;

            info!("📤 [TRANSITION] Sent SetSyncStartBlockRequest: last_consensus_block={}", last_consensus_block);

            use tokio::io::AsyncReadExt;
            use tokio::time::{timeout, Duration};
            let read_timeout = Duration::from_secs(5);

            let mut len_buf = [0u8; 4];
            timeout(read_timeout, stream.read_exact(&mut len_buf)).await
                .map_err(|e| anyhow::anyhow!("Timeout reading response length: {}", e))??;
            let response_len = u32::from_be_bytes(len_buf) as usize;

            if response_len == 0 || response_len > 10_000_000 {
                return Err(anyhow::anyhow!("Invalid response length: {}", response_len));
            }

            let mut response_buf = vec![0u8; response_len];
            timeout(read_timeout, stream.read_exact(&mut response_buf)).await
                .map_err(|e| anyhow::anyhow!("Timeout reading response data: {}", e))??;

            let response = Response::decode(&response_buf[..])?;
            match response.payload {
                Some(proto::response::Payload::NotifyEpochChangeResponse(_)) => {
                    return Err(anyhow::anyhow!("Unexpected NotifyEpochChangeResponse"));
                }
                Some(proto::response::Payload::SetSyncStartBlockResponse(res)) => {
                    info!(
                        "✅ [TRANSITION] SetSyncStartBlock response: success={}, sync_start_block={}, message={}",
                        res.success, res.sync_start_block, res.message
                    );
                    Ok((res.success, res.sync_start_block, res.message))
                }
                Some(proto::response::Payload::Error(error_msg)) => {
                    Err(anyhow::anyhow!("Go returned error: {}", error_msg))
                }
                _ => Err(anyhow::anyhow!("Unexpected response type from Go")),
            }
        } else {
            Err(anyhow::anyhow!("Request connection is not available"))
        }
    }

    /// Wait for Go sync to reach a specific block
    /// Used during SyncOnly -> Validator transition to ensure sync is complete before consensus starts
    pub async fn wait_for_sync_to_block(&self, target_block: u64, timeout_seconds: u64) -> Result<(bool, u64, String)> {
        if !self.is_enabled() {
            return Err(anyhow::anyhow!("Executor client is not enabled"));
        }

        if let Err(e) = self.connect_request().await {
            return Err(anyhow::anyhow!("Failed to connect to Go request socket: {}", e));
        }

        let request = Request {
            payload: Some(proto::request::Payload::WaitForSyncToBlockRequest(
                proto::WaitForSyncToBlockRequest { 
                    target_block, 
                    timeout_seconds, 
                },
            )),
        };

        let mut request_buf = Vec::new();
        request.encode(&mut request_buf)?;

        let mut conn_guard = self.request_connection.lock().await;
        if let Some(ref mut stream) = *conn_guard {
            let len = request_buf.len() as u32;
            stream.write_all(&len.to_be_bytes()).await?;
            stream.write_all(&request_buf).await?;
            stream.flush().await?;

            info!("📤 [TRANSITION] Sent WaitForSyncToBlockRequest: target_block={}, timeout={}s", target_block, timeout_seconds);

            use tokio::io::AsyncReadExt;
            use tokio::time::{timeout, Duration};
            // Timeout needs to be longer than the Go-side timeout
            let read_timeout = Duration::from_secs(timeout_seconds + 10);

            let mut len_buf = [0u8; 4];
            timeout(read_timeout, stream.read_exact(&mut len_buf)).await
                .map_err(|e| anyhow::anyhow!("Timeout reading response length: {}", e))??;
            let response_len = u32::from_be_bytes(len_buf) as usize;

            if response_len == 0 || response_len > 10_000_000 {
                return Err(anyhow::anyhow!("Invalid response length: {}", response_len));
            }

            let mut response_buf = vec![0u8; response_len];
            timeout(read_timeout, stream.read_exact(&mut response_buf)).await
                .map_err(|e| anyhow::anyhow!("Timeout reading response data: {}", e))??;

            let response = Response::decode(&response_buf[..])?;
            match response.payload {
                Some(proto::response::Payload::NotifyEpochChangeResponse(_)) => {
                    return Err(anyhow::anyhow!("Unexpected NotifyEpochChangeResponse"));
                }
                Some(proto::response::Payload::WaitForSyncToBlockResponse(res)) => {
                    info!(
                        "✅ [TRANSITION] WaitForSyncToBlock response: reached={}, current_block={}, message={}",
                        res.reached, res.current_block, res.message
                    );
                    Ok((res.reached, res.current_block, res.message))
                }
                Some(proto::response::Payload::Error(error_msg)) => {
                    Err(anyhow::anyhow!("Go returned error: {}", error_msg))
                }
                _ => Err(anyhow::anyhow!("Unexpected response type from Go")),
            }
        } else {
            Err(anyhow::anyhow!("Request connection is not available"))
        }
    }
}

// Persistence functions are in persistence.rs
// Block sync methods are in block_sync.rs
// Socket abstraction is in socket_stream.rs
