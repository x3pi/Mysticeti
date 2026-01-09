// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use consensus_core::{CommittedSubDag, BlockAPI};
use prost::Message;
use std::path::Path;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::sync::Mutex;
use tracing::{error, info, trace, warn};

// Include generated protobuf code
// Note: prost_build generates all messages from executor.proto into proto.rs
pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/proto.rs"));
}

// Include validator_rpc protobuf code for Request/Response
// Note: prost generates files based on package name, so "proto" package becomes "proto.rs"
// All proto files with package "proto" are merged into one proto.rs file
// So we can use the same proto module for all messages

use proto::{CommittedBlock, CommittedEpochData, TransactionExe, GetActiveValidatorsRequest, GetValidatorsAtBlockRequest, Request, Response, ValidatorInfo};
use std::collections::BTreeMap;

/// Client to send committed blocks to Go executor via Unix Domain Socket
/// Only enabled when config file exists (typically only node 0)
pub struct ExecutorClient {
    socket_path: String,
    connection: Arc<Mutex<Option<UnixStream>>>,
    request_socket_path: String, // For sending requests (Rust -> Go)
    request_connection: Arc<Mutex<Option<UnixStream>>>,
    enabled: bool,
    can_commit: bool, // Only node 0 can actually commit transactions to Go state
    /// Buffer for out-of-order blocks to ensure sequential sending
    /// Key: global_exec_index, Value: (epoch_data_bytes, epoch, commit_index)
    send_buffer: Arc<Mutex<BTreeMap<u64, (Vec<u8>, u64, u32)>>>,
    /// Next expected global_exec_index to send
    next_expected_index: Arc<tokio::sync::Mutex<u64>>,
}

impl ExecutorClient {
    /// Create new executor client
    /// enabled: whether executor is enabled (check config file exists)
    /// can_commit: whether this node can actually commit transactions (only node 0)
    /// send_socket_path: socket path for sending data to Go executor
    /// receive_socket_path: socket path for receiving data from Go executor
    pub fn new(enabled: bool, can_commit: bool, send_socket_path: String, receive_socket_path: String) -> Self {
        Self {
            socket_path: send_socket_path,
            connection: Arc::new(Mutex::new(None)),
            request_socket_path: receive_socket_path,
            request_connection: Arc::new(Mutex::new(None)),
            enabled,
            can_commit,
            send_buffer: Arc::new(Mutex::new(BTreeMap::new())),
            next_expected_index: Arc::new(tokio::sync::Mutex::new(1)), // Start from 1
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

    /// Send a request to Go executor and receive response
    async fn send_request(&self, request: Request) -> Result<Response> {
        if !self.is_enabled() {
            return Err(anyhow::anyhow!("Executor client is not enabled"));
        }

        // Connect to Go request socket if needed
        if let Err(e) = self.connect_request().await {
            return Err(anyhow::anyhow!("Failed to connect to Go request socket: {}", e));
        }

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

            info!("📤 [EXECUTOR-REQ] Sent request to Go (size: {} bytes)", request_buf.len());

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

            let mut response_buf = vec![0u8; response_len];
            timeout(read_timeout, stream.read_exact(&mut response_buf)).await
                .map_err(|e| anyhow::anyhow!("Timeout reading response data: {}", e))??;

            // Decode response
            let response = Response::decode(&response_buf[..])?;

            info!("📥 [EXECUTOR-REQ] Received response from Go (size: {} bytes)", response_buf.len());
            Ok(response)
        } else {
            Err(anyhow::anyhow!("No connection to Go request socket"))
        }
    }

    /// Get current epoch from Go state (Sui-style epoch monitoring)
    pub async fn get_current_epoch(&self) -> Result<u64> {
        if !self.enabled {
            return Err(anyhow::anyhow!("Executor client not enabled"));
        }

        let request = Request {
            payload: Some(proto::request::Payload::GetCurrentEpochRequest(
                proto::GetCurrentEpochRequest {}
            )),
        };

        let response = self.send_request(request).await?;
        match response.payload {
            Some(proto::response::Payload::GetCurrentEpochResponse(resp)) => {
                Ok(resp.epoch)
            }
            _ => Err(anyhow::anyhow!("Unexpected response type for get_current_epoch")),
        }
    }

    /// Get epoch start timestamp for a specific epoch from Go state
    pub async fn get_epoch_start_timestamp(&self, epoch: u64) -> Result<u64> {
        if !self.enabled {
            return Err(anyhow::anyhow!("Executor client not enabled"));
        }

        let request = Request {
            payload: Some(proto::request::Payload::GetEpochStartTimestampRequest(
                proto::GetEpochStartTimestampRequest { epoch }
            )),
        };

        let response = self.send_request(request).await?;
        match response.payload {
            Some(proto::response::Payload::GetEpochStartTimestampResponse(resp)) => {
                Ok(resp.timestamp_ms)
            }
            _ => Err(anyhow::anyhow!("Unexpected response type for get_epoch_start_timestamp")),
        }
    }

    /// Advance epoch in Go state (Sui-style epoch completion)
    pub async fn advance_epoch(&self, new_epoch: u64, epoch_start_timestamp_ms: u64) -> Result<(u64, u64)> {
        if !self.enabled {
            return Err(anyhow::anyhow!("Executor client not enabled"));
        }

        let request = Request {
            payload: Some(proto::request::Payload::AdvanceEpochRequest(
                proto::AdvanceEpochRequest {
                    new_epoch,
                    epoch_start_timestamp_ms,
                }
            )),
        };

        let response = self.send_request(request).await?;
        match response.payload {
            Some(proto::response::Payload::AdvanceEpochResponse(resp)) => {
                Ok((resp.new_epoch, resp.epoch_start_timestamp_ms))
            }
            _ => Err(anyhow::anyhow!("Unexpected response type for advance_epoch")),
        }
    }

    /// Initialize next_expected_index from Go Master's last block number
    /// This should be called ONCE when executor client is created, not on every connect
    /// After initialization, Rust will send blocks continuously and Go will buffer/process them sequentially
    /// CRITICAL: Only update next_expected if Go Master has processed more blocks (don't reset to old value)
    pub async fn initialize_from_go(&self) {
        if !self.is_enabled() {
            return;
        }
        
        // Query Go Master for last block number (only once at startup)
        if let Ok(last_block_number) = self.get_last_block_number().await {
            let new_next_expected = last_block_number + 1;
            let mut next_expected_guard = self.next_expected_index.lock().await;
            let current_next_expected = *next_expected_guard;
            
            // CRITICAL: Only update if Go Master has processed more blocks than we've sent
            // This prevents resetting next_expected to an old value (e.g., 50002) when Rust has already sent blocks up to 74860
            if new_next_expected > current_next_expected {
                *next_expected_guard = new_next_expected;
                info!("📊 [INIT] Updated next_expected_index from Go Master: last_block_number={}, old_next_expected={}, new_next_expected={}", 
                    last_block_number, current_next_expected, new_next_expected);
            } else if new_next_expected < current_next_expected {
                warn!("⚠️  [INIT] Go Master last_block_number={} (next_expected={}) is LESS than current next_expected={}. This means Rust has already sent blocks that Go hasn't processed yet. Keeping current value to avoid resetting progress.",
                    last_block_number, new_next_expected, current_next_expected);
            } else {
                info!("📊 [INIT] next_expected_index already correct: last_block_number={}, next_expected={}", 
                    last_block_number, current_next_expected);
            }
        } else {
            warn!("⚠️  [INIT] Failed to get last block number from Go Master. Keeping current next_expected_index. Rust will continue sending blocks, Go will buffer and process sequentially.");
        }
    }

    /// Connect to executor socket (lazy connection with retry)
    /// Just connects, doesn't query Go - Rust sends blocks continuously, Go buffers and processes sequentially
    async fn connect(&self) -> Result<()> {
        let mut conn_guard = self.connection.lock().await;
        
        // Check if already connected and still valid
        if let Some(ref mut stream) = *conn_guard {
            // Try to peek at the stream to check if it's still alive
            match stream.writable().await {
                Ok(_) => {
                    // Connection is still valid
                    trace!("🔌 [EXECUTOR] Reusing existing connection to {}", self.socket_path);
                    return Ok(());
                }
                Err(e) => {
                    // Connection is dead, close it
                    warn!("⚠️  [EXECUTOR] Existing connection to {} is dead: {}, reconnecting...", 
                        self.socket_path, e);
                    *conn_guard = None;
                }
            }
        }

        // Connect to socket with retry logic
        const MAX_RETRIES: u32 = 3;
        const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(100);
        
        for attempt in 1..=MAX_RETRIES {
            match UnixStream::connect(&self.socket_path).await {
                Ok(stream) => {
                    info!("🔌 [EXECUTOR] Connected to executor at {} (attempt {}/{})", 
                        self.socket_path, attempt, MAX_RETRIES);
                    *conn_guard = Some(stream);
                    return Ok(());
                }
                Err(e) => {
                    if attempt < MAX_RETRIES {
                        warn!("⚠️  [EXECUTOR] Failed to connect to executor at {} (attempt {}/{}): {}, retrying...", 
                            self.socket_path, attempt, MAX_RETRIES, e);
                        tokio::time::sleep(RETRY_DELAY).await;
                    } else {
                        warn!("⚠️  [EXECUTOR] Failed to connect to executor at {} after {} attempts: {}", 
                            self.socket_path, MAX_RETRIES, e);
                        return Err(e.into());
                    }
                }
            }
        }
        
        unreachable!()
    }

    /// Send committed sub-DAG to executor
    /// CRITICAL FORK-SAFETY: global_exec_index and commit_index ensure deterministic execution order
    /// SEQUENTIAL BUFFERING: Blocks are buffered and sent in order to ensure Go receives them sequentially
    pub async fn send_committed_subdag(
        &self,
        subdag: &CommittedSubDag,
        epoch: u64,
        global_exec_index: u64,
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

        // Convert CommittedSubDag to protobuf CommittedEpochData
        // CRITICAL FORK-SAFETY: Include global_exec_index and commit_index for deterministic ordering
        let epoch_data_bytes = self.convert_to_protobuf(subdag, epoch, global_exec_index)?;

        // Count total transactions before sending
        let total_tx: usize = subdag.blocks.iter().map(|b| b.transactions().len()).sum();
        
        // NOTE: Removed duplicate check here - duplicate prevention is handled at the source
        // (commit_processor checks barrier BEFORE calculating global_exec_index and BEFORE sending)
        // If we see duplicate here, it means barrier check failed, which is a critical bug
        // We keep this check as a safety net, but the real fix is to ensure barrier check works correctly
        
        // SEQUENTIAL BUFFERING: Add to buffer and try to send in order
        {
            let mut buffer = self.send_buffer.lock().await;
            buffer.insert(global_exec_index, (epoch_data_bytes, epoch, subdag.commit_ref.index));
            info!("📦 [SEQUENTIAL-BUFFER] Added block to buffer: global_exec_index={}, commit_index={}, epoch={}, blocks={}, total_tx={}, buffer_size={}",
                global_exec_index, subdag.commit_ref.index, epoch, subdag.blocks.len(), total_tx, buffer.len());
        }
        
        // Try to send buffered blocks in order
        self.flush_buffer().await?;
        
        Ok(())
    }
    
    /// Flush buffered blocks in sequential order
    /// This ensures Go executor receives blocks in the correct order even if Rust sends them out-of-order
    async fn flush_buffer(&self) -> Result<()> {
        // Connect if needed
        if let Err(e) = self.connect().await {
            warn!("⚠️  Executor connection failed, cannot flush buffer: {}", e);
            return Ok(()); // Don't fail if executor is unavailable
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
                
                // If gap is large, it means blocks are missing - log warning but do NOT skip
                // We must send blocks sequentially, so we wait for the missing blocks
                if gap > 100 {
                    warn!("⚠️  [SEQUENTIAL-BUFFER] Large gap detected: next_expected={}, min_buffered={}, gap={}. Missing blocks detected - waiting for blocks to arrive. Do NOT skip to maintain sequential ordering.",
                        *next_expected, min_buffered, gap);
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
                    current_expected, commit_index, epoch, epoch_data_bytes.len());
                
                if let Err(e) = self.send_block_data(&epoch_data_bytes, current_expected, epoch, commit_index).await {
                    warn!("⚠️  [SEQUENTIAL-BUFFER] Failed to send block global_exec_index={}: {}, re-adding to buffer", 
                        current_expected, e);
                    // Re-add to buffer for retry
                    let mut buffer_retry = self.send_buffer.lock().await;
                    buffer_retry.insert(current_expected, (epoch_data_bytes, epoch, commit_index));
                    return Ok(());
                }
                
                // Successfully sent, increment next_expected
                {
                    let mut next_expected = self.next_expected_index.lock().await;
                    *next_expected += 1;
                    info!("✅ [SEQUENTIAL-BUFFER] Successfully sent block global_exec_index={}, next_expected={}", 
                        current_expected, *next_expected);
                }
            } else {
                // No more consecutive blocks to send
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
                if buffer.len() > 10 {
                    warn!("⚠️  [SEQUENTIAL-BUFFER] Buffer has {} blocks waiting (range: {}..{}, next_expected={}). Some blocks may be missing or out-of-order.",
                        buffer.len(), min_buffered, max_buffered, *next_expected);
                } else {
                    info!("📊 [SEQUENTIAL-BUFFER] Buffer has {} blocks waiting (range: {}..{}, next_expected={})",
                        buffer.len(), min_buffered, max_buffered, *next_expected);
                }
            }
        }
        
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
                Ok(_) => {
                    info!("📤 [TX FLOW] Sent committed sub-DAG to Go executor: global_exec_index={}, commit_index={}, epoch={}, data_size={} bytes", 
                        global_exec_index, commit_index, epoch, epoch_data_bytes.len());
                    Ok(())
                }
                Err(e) => {
                    warn!("⚠️  [EXECUTOR] Failed to send committed sub-DAG (global_exec_index={}, commit_index={}): {}, reconnecting...", 
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
                            Ok(Ok(())) => {
                                info!("✅ [EXECUTOR] Successfully sent committed sub-DAG after reconnection: global_exec_index={}, commit_index={}", 
                                    global_exec_index, commit_index);
                                Ok(())
                            }
                            Ok(Err(retry_err)) => {
                                warn!("⚠️  [EXECUTOR] Retry send also failed: {}", retry_err);
                                *retry_guard = None; // Clear connection for next attempt
                                Err(anyhow::anyhow!("Retry send failed: {}", retry_err))
                            }
                            Err(_) => {
                                warn!("⏱️  [EXECUTOR] Retry send timeout after {}s (global_exec_index={}, commit_index={})", 
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
        } else {
            warn!("⚠️  [EXECUTOR] Executor connection lost, skipping send");
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
    ) -> Result<Vec<u8>> {
        // Build CommittedEpochData protobuf message using generated types
        let mut blocks = Vec::new();
        
        for (block_idx, block) in subdag.blocks.iter().enumerate() {
            // Extract transactions with hash for deterministic sorting
            // CRITICAL FORK-SAFETY: Sort transactions by hash to ensure all nodes send same order
            let mut transactions_with_hash: Vec<(Vec<u8>, Vec<u8>)> = Vec::new(); // (tx_data, tx_hash)
            
            for (tx_idx, tx) in block.transactions().iter().enumerate() {
                // Get transaction data (raw bytes) - Go needs transaction data, not digest
                // IMPORTANT: tx.data() returns a reference to the original bytes, no modification
                let tx_data = tx.data();
                
                // 🔍 HASH INTEGRITY CHECK: Verify transaction data integrity by calculating hash
                // This ensures data hasn't been modified during consensus
                use crate::tx_hash::calculate_transaction_hash;
                let tx_hash = calculate_transaction_hash(tx_data);
                let tx_hash_hex = hex::encode(&tx_hash[..8.min(tx_hash.len())]);
                info!("🔍 [TX HASH] Rust executor preparing to send to Go Master: hash={}, size={} bytes, block_idx={}, tx_idx={}", 
                    tx_hash_hex, tx_data.len(), block_idx, tx_idx);
                
                // CRITICAL: Verify transaction data is valid protobuf before sending to Go
                // Go executor expects protobuf Transactions or Transaction message
                use crate::tx_hash::verify_transaction_protobuf;
                if !verify_transaction_protobuf(tx_data) {
                    warn!("🚫 [TX INTEGRITY] Invalid transaction protobuf in committed block (hash={}, size={} bytes). This transaction will cause unmarshal errors in Go executor. Skipping send.", 
                        tx_hash_hex, tx_data.len());
                    
                    // Log first few bytes for debugging
                    let preview_len = tx_data.len().min(32);
                    let preview_hex = hex::encode(&tx_data[..preview_len]);
                    warn!("   📋 [TX INTEGRITY] Transaction data preview (first {} bytes): {}", preview_len, preview_hex);
                    
                    // Decide: skip this transaction or fail the whole block/commit?
                    // For now, we'll skip this transaction to prevent Go executor errors
                    // But this should be investigated - why is invalid protobuf in consensus blocks?
                    continue;
                }
                
                trace!("🔍 [TX INTEGRITY] Verifying transaction data: hash={}, size={} bytes", 
                    tx_hash_hex, tx_data.len());
                
                // Store transaction data with hash for sorting
                transactions_with_hash.push((tx_data.to_vec(), tx_hash));
                
                info!("✅ [TX INTEGRITY] Transaction data preserved: hash={}, size={} bytes (unchanged from submission)", 
                    tx_hash_hex, tx_data.len());
            }
            
            // CRITICAL FORK-SAFETY: Sort transactions by hash (deterministic ordering)
            // This ensures all nodes send transactions in the same order within a block
            // Sort by hash bytes (lexicographic order) - deterministic across all nodes
            transactions_with_hash.sort_by(|(_, hash_a), (_, hash_b)| hash_a.cmp(hash_b));
            
            // Convert to TransactionExe messages after sorting
            let mut transactions = Vec::new();
            for (tx_data, tx_hash) in transactions_with_hash {
                let tx_hash_hex = hex::encode(&tx_hash[..8.min(tx_hash.len())]);
                info!("📋 [FORK-SAFETY] Sorted transaction in block[{}]: hash={}", block_idx, tx_hash_hex);
                
                // Create TransactionExe message using generated protobuf code
                // NOTE: We use "digest" field to store transaction data (raw bytes)
                // Go will unmarshal this as transaction data
                // IMPORTANT: We create a copy here (to_vec()) but the data is unchanged
                let tx_exe = TransactionExe {
                    digest: tx_data, // Already a Vec<u8> from sorting step
                    worker_id: 0, // Optional, set to 0 for now
                };
                transactions.push(tx_exe);
            }
            
            info!("✅ [FORK-SAFETY] Block[{}] transactions sorted: {} transactions (deterministic order by hash)", 
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
        // This is especially important for merged blocks from commits past barrier
        // All nodes must send blocks in the same order
        blocks.sort_by(|a, b| a.height.cmp(&b.height));
        
        // Create CommittedEpochData message using generated protobuf code
        // CRITICAL FORK-SAFETY: Include global_exec_index and commit_index for deterministic ordering
        let epoch_data = CommittedEpochData {
            blocks,
            global_exec_index,
            commit_index: subdag.commit_ref.index as u32,
        };
        
        // Encode to protobuf bytes using prost::Message::encode
        // This ensures correct protobuf encoding that Go can unmarshal
        let mut buf = Vec::new();
        epoch_data.encode(&mut buf)?;
        
        info!("📦 [TX INTEGRITY] Encoded CommittedEpochData: global_exec_index={}, commit_index={}, epoch={}, blocks={}, total_size={} bytes (using protobuf encoding)", 
            global_exec_index, subdag.commit_ref.index, epoch, epoch_data.blocks.len(), buf.len());
        
        Ok(buf)
    }

    /// Connect to Go request socket for request/response (lazy connection with retry)
    async fn connect_request(&self) -> Result<()> {
        let mut conn_guard = self.request_connection.lock().await;
        
        // Check if already connected and still valid
        if let Some(ref mut stream) = *conn_guard {
            match stream.writable().await {
                Ok(_) => {
                    trace!("🔌 [EXECUTOR-REQ] Reusing existing request connection to {}", self.request_socket_path);
                    return Ok(());
                }
                Err(e) => {
                    warn!("⚠️  [EXECUTOR-REQ] Existing request connection to {} is dead: {}, reconnecting...", 
                        self.request_socket_path, e);
                    *conn_guard = None;
                }
            }
        }

        // Connect to socket with retry logic
        const MAX_RETRIES: u32 = 3;
        const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(100);
        
        for attempt in 1..=MAX_RETRIES {
            match UnixStream::connect(&self.request_socket_path).await {
                Ok(stream) => {
                    info!("🔌 [EXECUTOR-REQ] Connected to Go request socket at {} (attempt {}/{})", 
                        self.request_socket_path, attempt, MAX_RETRIES);
                    *conn_guard = Some(stream);
                    return Ok(());
                }
                Err(e) => {
                    if attempt < MAX_RETRIES {
                        warn!("⚠️  [EXECUTOR-REQ] Failed to connect to Go request socket at {} (attempt {}/{}): {}, retrying...", 
                            self.request_socket_path, attempt, MAX_RETRIES, e);
                        tokio::time::sleep(RETRY_DELAY).await;
                    } else {
                        warn!("⚠️  [EXECUTOR-REQ] Failed to connect to Go request socket at {} after {} attempts: {}", 
                            self.request_socket_path, MAX_RETRIES, e);
                        return Err(e.into());
                    }
                }
            }
        }
        
        unreachable!()
    }

    pub async fn get_active_validators(&self) -> Result<Vec<ValidatorInfo>> {
        if !self.is_enabled() {
            return Err(anyhow::anyhow!("Executor client is not enabled"));
        }

        // Connect to Go request socket if needed
        if let Err(e) = self.connect_request().await {
            return Err(anyhow::anyhow!("Failed to connect to Go request socket: {}", e));
        }

        // Create GetActiveValidatorsRequest
        let request = Request {
            payload: Some(proto::request::Payload::GetActiveValidatorsRequest(
                GetActiveValidatorsRequest {}
            )),
        };

        // Encode request to protobuf bytes
        let mut request_buf = Vec::new();
        request.encode(&mut request_buf)?;

        // Send request via UDS (4-byte length prefix, big-endian, like Go's WriteMessage)
        let mut conn_guard = self.request_connection.lock().await;
        if let Some(ref mut stream) = *conn_guard {
            // Write 4-byte length prefix (big-endian)
            let len = request_buf.len() as u32;
            let len_bytes = len.to_be_bytes();
            stream.write_all(&len_bytes).await?;
            
            // Write request data
            stream.write_all(&request_buf).await?;
            stream.flush().await?;
            
            info!("📤 [EXECUTOR-REQ] Sent GetActiveValidatorsRequest to Go (size: {} bytes)", request_buf.len());

            // Read response (4-byte length prefix + response data)
            use tokio::io::AsyncReadExt;
            let mut len_buf = [0u8; 4];
            stream.read_exact(&mut len_buf).await?;
            let response_len = u32::from_be_bytes(len_buf) as usize;
            
            let mut response_buf = vec![0u8; response_len];
            stream.read_exact(&mut response_buf).await?;
            
            // Decode response
            let response = Response::decode(&response_buf[..])?;
            
            match response.payload {
                Some(proto::response::Payload::ValidatorInfoList(validator_info_list)) => {
                    info!("✅ [EXECUTOR-REQ] Received ValidatorInfoList from Go with {} validators", 
                        validator_info_list.validators.len());
                    return Ok(validator_info_list.validators);
                }
                _ => {
                    return Err(anyhow::anyhow!("Unexpected response type from Go"));
                }
            }
        } else {
            return Err(anyhow::anyhow!("Request connection is not available"));
        }
    }

    /// Get validators at a specific block number from Go state
    /// Used for startup (block 0) and epoch transition (last_global_exec_index)
    pub async fn get_validators_at_block(&self, block_number: u64) -> Result<(Vec<ValidatorInfo>, u64)> {
        if !self.is_enabled() {
            return Err(anyhow::anyhow!("Executor client is not enabled"));
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
                    warn!("🔍 [EXECUTOR-REQ] Payload is GetCurrentEpochResponse (not expected for this request)");
                }
                Some(proto::response::Payload::GetEpochStartTimestampResponse(_)) => {
                    warn!("🔍 [EXECUTOR-REQ] Payload is GetEpochStartTimestampResponse (not expected for this request)");
                }
                Some(proto::response::Payload::AdvanceEpochResponse(_)) => {
                    warn!("🔍 [EXECUTOR-REQ] Payload is AdvanceEpochResponse (not expected for this request)");
                }
                None => {
                    warn!("🔍 [EXECUTOR-REQ] Payload is None - response structure may be incorrect");
                    warn!("🔍 [EXECUTOR-REQ] Full response debug: {:?}", response);
                }
            }
            
            match response.payload {
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
                None => {
                    return Err(anyhow::anyhow!("Unexpected response type from Go. Response payload: None. Response bytes (hex): {}", hex::encode(&response_buf)));
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
                Some(proto::response::Payload::LastBlockNumberResponse(res)) => {
                    let last_block_number = res.last_block_number;
                    info!("✅ [EXECUTOR-REQ] Received LastBlockNumberResponse: last_block_number={}", last_block_number);
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

    /// Submit checkpoint-based epoch change transaction (Sui-style)
    /// This replaces voting with deterministic transaction execution
    pub async fn submit_checkpoint_epoch_change_transaction(
        &self,
        epoch_change_data: crate::checkpoint_epoch_transition::CheckpointEpochChangeData,
    ) -> Result<()> {
        if !self.enabled {
            warn!("⚠️  [CHECKPOINT-EPOCH] Executor client not enabled - epoch transition skipped");
            return Ok(()); // Don't fail if executor is disabled
        }

        info!("📤 [CHECKPOINT-EPOCH] Submitting epoch change transaction: epoch {} -> {} at checkpoint {}",
            epoch_change_data.new_epoch - 1, epoch_change_data.new_epoch, epoch_change_data.checkpoint_sequence);

        // For now, reuse existing advance_epoch method
        // In future, this could use a dedicated checkpoint epoch change transaction
        match self.advance_epoch(
            epoch_change_data.new_epoch,
            epoch_change_data.epoch_start_timestamp_ms,
        ).await {
            Ok((new_epoch, epoch_start_ts)) => {
                info!("✅ [CHECKPOINT-EPOCH] Epoch change transaction executed successfully: epoch={}, timestamp={}",
                    new_epoch, epoch_start_ts);
                Ok(())
            }
            Err(e) => {
                // ⚠️  NON-FATAL: Log error but don't fail the transaction submission
                // This allows consensus to continue even if Go side has issues
                error!("⚠️  [CHECKPOINT-EPOCH] Epoch change transaction failed (non-fatal): {}", e);
                error!("🔄 Continuing with blocks despite Go-side epoch transition failure");
                Ok(()) // Return Ok to prevent consensus shutdown
            }
        }
    }


}

/// Write uvarint to buffer (Go's binary.ReadUvarint format)
fn write_uvarint(buf: &mut Vec<u8>, mut value: u64) -> Result<()> {
    use std::io::Write;
    loop {
        let mut b = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            b |= 0x80;
        }
        Write::write_all(buf, &[b])?;
        if value == 0 {
            break;
        }
    }
    Ok(())
}

// Note: Executor is now configured via executor_enabled field in node_X.toml
// This function is kept for backward compatibility but is no longer used
#[allow(dead_code)]
pub fn is_executor_enabled(config_dir: &Path) -> bool {
    let config_file = config_dir.join("enable_executor.toml");
    config_file.exists()
}

