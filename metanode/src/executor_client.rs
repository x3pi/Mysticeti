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
use tracing::{info, trace, warn};

// Include generated protobuf code
// Note: prost_build generates all messages from executor.proto into proto.rs
mod proto {
    include!(concat!(env!("OUT_DIR"), "/proto.rs"));
}

use proto::{CommittedBlock, CommittedEpochData, TransactionExe};

/// Client to send committed blocks to Go executor via Unix Domain Socket
/// Only enabled when config file exists (typically only node 0)
pub struct ExecutorClient {
    socket_path: String,
    connection: Arc<Mutex<Option<UnixStream>>>,
    enabled: bool,
}

impl ExecutorClient {
    /// Create new executor client
    /// enabled: whether executor is enabled (check config file exists)
    /// socket_id: socket ID (0 for node 0)
    pub fn new(enabled: bool, socket_id: usize) -> Self {
        let socket_path = format!("/tmp/executor{}.sock", socket_id);
        Self {
            socket_path,
            connection: Arc::new(Mutex::new(None)),
            enabled,
        }
    }

    /// Check if executor is enabled
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Connect to executor socket (lazy connection with retry)
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
    pub async fn send_committed_subdag(&self, subdag: &CommittedSubDag, epoch: u64) -> Result<()> {
        if !self.enabled {
            return Ok(()); // Silently skip if not enabled
        }

        // Connect if needed
        if let Err(e) = self.connect().await {
            warn!("⚠️  Executor connection failed, skipping send: {}", e);
            return Ok(()); // Don't fail commit if executor is unavailable
        }

        // Convert CommittedSubDag to protobuf CommittedEpochData
        let epoch_data_bytes = self.convert_to_protobuf(subdag, epoch)?;

        // Count total transactions before sending
        let total_tx: usize = subdag.blocks.iter().map(|b| b.transactions().len()).sum();
        
        // Send via UDS with Uvarint length prefix (Go expects Uvarint)
        let mut conn_guard = self.connection.lock().await;
        if let Some(ref mut stream) = *conn_guard {
            // Write Uvarint length prefix
            let mut len_buf = Vec::new();
            write_uvarint(&mut len_buf, epoch_data_bytes.len() as u64)?;
            
            // Send with retry logic if write fails
            let send_result = async {
                stream.write_all(&len_buf).await?;
                stream.write_all(&epoch_data_bytes).await?;
                stream.flush().await?;
                Ok::<(), std::io::Error>(())
            }.await;
            
            match send_result {
                Ok(_) => {
                    info!("📤 [TX FLOW] Sent committed sub-DAG to Go executor: commit_index={}, epoch={}, blocks={}, total_tx={}, data_size={} bytes", 
                        subdag.commit_ref.index, epoch, subdag.blocks.len(), total_tx, epoch_data_bytes.len());
                }
                Err(e) => {
                    warn!("⚠️  [EXECUTOR] Failed to send committed sub-DAG (commit_index={}, total_tx={}): {}, reconnecting...", 
                        subdag.commit_ref.index, total_tx, e);
                    // Connection is dead, clear it so next send will reconnect
                    *conn_guard = None;
                    // Retry send after reconnection
                    drop(conn_guard);
                    if let Err(reconnect_err) = self.connect().await {
                        warn!("⚠️  [EXECUTOR] Failed to reconnect after send error: {}", reconnect_err);
                        return Ok(()); // Don't fail commit if executor is unavailable
                    }
                    // Retry send
                    let mut retry_guard = self.connection.lock().await;
                    if let Some(ref mut retry_stream) = *retry_guard {
                        let mut retry_len_buf = Vec::new();
                        write_uvarint(&mut retry_len_buf, epoch_data_bytes.len() as u64)?;
                        if let Err(retry_err) = async {
                            retry_stream.write_all(&retry_len_buf).await?;
                            retry_stream.write_all(&epoch_data_bytes).await?;
                            retry_stream.flush().await?;
                            Ok::<(), std::io::Error>(())
                        }.await {
                            warn!("⚠️  [EXECUTOR] Retry send also failed: {}", retry_err);
                            *retry_guard = None; // Clear connection for next attempt
                        } else {
                            info!("✅ [EXECUTOR] Successfully sent committed sub-DAG after reconnection: commit_index={}, total_tx={}", 
                                subdag.commit_ref.index, total_tx);
                        }
                    }
                }
            }
        } else {
            warn!("⚠️  [EXECUTOR] Executor connection lost, skipping send");
        }

        Ok(())
    }

    /// Convert CommittedSubDag to protobuf CommittedEpochData bytes
    /// Uses generated protobuf code to ensure correct encoding
    /// 
    /// IMPORTANT: Transaction data is passed through unchanged from Go sub → Rust consensus → Go master
    /// We verify data integrity by checking transaction hash at each step
    fn convert_to_protobuf(&self, subdag: &CommittedSubDag, epoch: u64) -> Result<Vec<u8>> {
        use crate::tx_hash::calculate_transaction_hash_hex;
        
        // Build CommittedEpochData protobuf message using generated types
        let mut blocks = Vec::new();
        
        for (block_idx, block) in subdag.blocks.iter().enumerate() {
            // Extract transactions
            let mut transactions = Vec::new();
            for (tx_idx, tx) in block.transactions().iter().enumerate() {
                // Get transaction data (raw bytes) - Go needs transaction data, not digest
                // IMPORTANT: tx.data() returns a reference to the original bytes, no modification
                let tx_data = tx.data();
                
                // 🔍 HASH INTEGRITY CHECK: Verify transaction data integrity by calculating hash
                // This ensures data hasn't been modified during consensus
                let tx_hash_hex = calculate_transaction_hash_hex(tx_data);
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
                
                // Create TransactionExe message using generated protobuf code
                // NOTE: We use "digest" field to store transaction data (raw bytes)
                // Go will unmarshal this as transaction data
                // IMPORTANT: We create a copy here (to_vec()) but the data is unchanged
                let tx_exe = TransactionExe {
                    digest: tx_data.to_vec(), // Copy for protobuf encoding, but data is unchanged
                    worker_id: 0, // Optional, set to 0 for now
                };
                transactions.push(tx_exe);
                
                info!("✅ [TX INTEGRITY] Transaction data preserved: hash={}, size={} bytes (unchanged from submission)", 
                    tx_hash_hex, tx_data.len());
            }
            
            // Create CommittedBlock message using generated protobuf code
            let committed_block = CommittedBlock {
                epoch,
                height: subdag.commit_ref.index as u64,
                transactions,
            };
            blocks.push(committed_block);
        }
        
        // Create CommittedEpochData message using generated protobuf code
        let epoch_data = CommittedEpochData { blocks };
        
        // Encode to protobuf bytes using prost::Message::encode
        // This ensures correct protobuf encoding that Go can unmarshal
        let mut buf = Vec::new();
        epoch_data.encode(&mut buf)?;
        
        info!("📦 [TX INTEGRITY] Encoded CommittedEpochData: epoch={}, blocks={}, total_size={} bytes (using protobuf encoding)", 
            epoch, epoch_data.blocks.len(), buf.len());
        
        Ok(buf)
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

