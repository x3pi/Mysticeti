// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tracing::{error, info, warn};
use crate::tx_submitter::TransactionSubmitter;
use crate::node::ConsensusNode;
use tokio::sync::Mutex;

/// Unix Domain Socket server for transaction submission
/// Faster than HTTP for local IPC communication
pub struct TxSocketServer {
    socket_path: String,
    transaction_client: Arc<dyn TransactionSubmitter>,
    /// Optional node reference for readiness checking
    node: Option<Arc<Mutex<ConsensusNode>>>,
}

impl TxSocketServer {
    /// Create UDS server with node reference for readiness checking
    pub fn with_node(
        socket_path: String,
        transaction_client: Arc<dyn TransactionSubmitter>,
        node: Arc<Mutex<ConsensusNode>>,
    ) -> Self {
        Self {
            socket_path,
            transaction_client,
            node: Some(node),
        }
    }

    /// Start the UDS server
    pub async fn start(self) -> Result<()> {
        // Remove old socket file if exists
        if Path::new(&self.socket_path).exists() {
            std::fs::remove_file(&self.socket_path)?;
        }

        let listener = UnixListener::bind(&self.socket_path)?;
        info!("🔌 Transaction UDS server started on {}", self.socket_path);

        // Set socket permissions (read/write for owner and group)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o660);
            std::fs::set_permissions(&self.socket_path, perms)?;
        }

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let client = self.transaction_client.clone();
                    let node = self.node.clone();

                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_connection(stream, client, node).await {
                            error!("Error handling UDS connection: {}", e);
                        }
                    });
                }
                Err(e) => {
                    error!("Failed to accept UDS connection: {}", e);
                }
            }
        }
    }

    async fn handle_connection(
        mut stream: UnixStream,
        client: Arc<dyn TransactionSubmitter>,
        node: Option<Arc<Mutex<ConsensusNode>>>,
    ) -> Result<()> {
        // PERSISTENT CONNECTION: Xử lý multiple requests trên cùng một connection
        // Điều này cho phép Go client gửi nhiều batches qua cùng một connection
        // Tối ưu cho localhost với throughput cao
        loop {
            // Read length prefix (4 bytes, big-endian)
            let mut len_buf = [0u8; 4];
            let read_result = stream.read_exact(&mut len_buf).await;
            
            // Nếu connection đóng (EOF), return bình thường (không phải lỗi)
            if let Err(e) = read_result {
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    info!("🔌 [TX FLOW] UDS connection closed by client (EOF)");
                    return Ok(());
                }
                // Lỗi khác, log và return
                error!("❌ [TX FLOW] Failed to read length prefix from UDS: {}", e);
                return Err(e.into());
            }
            
            let data_len = u32::from_be_bytes(len_buf) as usize;

            // Validate length (max 10MB per transaction)
            const MAX_TX_SIZE: usize = 10 * 1024 * 1024;
            if data_len > MAX_TX_SIZE {
                let error_response = format!(
                    r#"{{"success":false,"error":"Transaction too large: {} bytes (max: {})"}}"#,
                    data_len, MAX_TX_SIZE
                );
                if let Err(e) = Self::send_response_string(&mut stream, &error_response).await {
                    error!("❌ [TX FLOW] Failed to send error response: {}", e);
                    return Err(e.into());
                }
                continue; // Tiếp tục xử lý request tiếp theo
            }

            // Read transaction data
            let mut tx_data = vec![0u8; data_len];
            if let Err(e) = stream.read_exact(&mut tx_data).await {
                error!("❌ [TX FLOW] Failed to read transaction data via UDS: expected {} bytes, error={}", data_len, e);
                // Nếu là EOF, connection đã đóng, return bình thường
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    info!("🔌 [TX FLOW] UDS connection closed by client while reading data");
                    return Ok(());
                }
                return Err(e.into());
            }
        
        // 🔍 HASH INTEGRITY CHECK: Calculate actual transaction hash from protobuf data
        use crate::tx_hash;
        let tx_hash_preview = tx_hash::calculate_transaction_hash_hex(&tx_data);
        let tx_hash_short = if tx_hash_preview.len() >= 16 {
            &tx_hash_preview[..16]
        } else {
            &tx_hash_preview
        };
        
        info!("📥 [TX FLOW] Received transaction data via UDS: size={} bytes, hash={}...", 
            data_len, tx_hash_short);
        info!("🔍 [TX HASH] Rust received from Go-sub: full_hash={}, size={} bytes", 
            tx_hash_preview, data_len);

        // Check if node is ready to accept transactions
        if let Some(ref node) = node {
            let node_guard = node.lock().await;
            let (is_ready, reason) = node_guard.is_ready_for_transactions().await;
            drop(node_guard);
            
            if !is_ready {
                warn!("🚫 Transaction rejected via UDS: node not ready - {}", reason);
                let error_response = format!(
                    r#"{{"success":false,"error":"Node not ready to accept transactions: {}"}}"#,
                    reason.replace('"', "\\\"")
                );
                if let Err(e) = Self::send_response_string(&mut stream, &error_response).await {
                    error!("❌ [TX FLOW] Failed to send error response: {}", e);
                    return Err(e.into());
                }
                continue; // Tiếp tục xử lý request tiếp theo
            }
        }

        // THỐNG NHẤT: Go LUÔN gửi pb.Transactions (nhiều transactions)
        // Rust CHỈ xử lý Transactions message, không xử lý single Transaction hoặc raw data
        use prost::Message;
        
        mod proto {
            include!(concat!(env!("OUT_DIR"), "/transaction.rs"));
        }
        use proto::{Transaction, Transactions};
        
        // Go LUÔN gửi Transactions message
        let transactions_to_submit = match Transactions::decode(&tx_data[..]) {
            Ok(transactions_msg) => {
                if transactions_msg.transactions.is_empty() {
                    warn!("⚠️  [TX FLOW] Empty Transactions message received from Go via UDS");
                    let error_response = r#"{"success":false,"error":"Empty Transactions message"}"#;
                    if let Err(e) = Self::send_response_string(&mut stream, error_response).await {
                        error!("❌ [TX FLOW] Failed to send error response: {}", e);
                        return Err(e.into());
                    }
                    continue; // Tiếp tục xử lý request tiếp theo
                }
                
                info!("📦 [TX FLOW] Received Transactions message from Go via UDS with {} transactions, splitting into individual transactions", 
                    transactions_msg.transactions.len());
                
                // Split Transactions message into individual Transaction messages
                // Mỗi transaction được encode riêng để submit vào consensus
                let mut individual_txs = Vec::new();
                for (idx, tx) in transactions_msg.transactions.iter().enumerate() {
                    // Encode each Transaction as individual protobuf message
                    let mut buf = Vec::new();
                    if let Err(e) = tx.encode(&mut buf) {
                        error!("❌ [TX FLOW] Failed to encode transaction[{}] from Go Transactions message via UDS: {}", idx, e);
                        continue;
                    }
                    individual_txs.push(buf);
                }
                
                if individual_txs.is_empty() {
                    error!("❌ [TX FLOW] No valid transactions after encoding from Go Transactions message via UDS");
                    let error_response = r#"{"success":false,"error":"No valid transactions after encoding"}"#;
                    if let Err(e) = Self::send_response_string(&mut stream, error_response).await {
                        error!("❌ [TX FLOW] Failed to send error response: {}", e);
                        return Err(e.into());
                    }
                    continue; // Tiếp tục xử lý request tiếp theo
                }
                
                info!("✅ [TX FLOW] Split Go Transactions message into {} individual transactions for consensus via UDS", individual_txs.len());
                individual_txs
            }
            Err(e) => {
                // Go LUÔN gửi Transactions, nếu không decode được thì là lỗi
                error!("❌ [TX FLOW] Failed to decode Transactions message from Go via UDS (expected pb.Transactions): {}", e);
                error!("❌ [TX FLOW] Data preview (first 100 bytes): {}", 
                    hex::encode(&tx_data[..tx_data.len().min(100)]));
                let error_response = format!(r#"{{"success":false,"error":"Invalid Transactions protobuf: {}"}}"#, 
                    e.to_string().replace('"', "\\\""));
                if let Err(send_err) = Self::send_response_string(&mut stream, &error_response).await {
                    error!("❌ [TX FLOW] Failed to send error response: {}", send_err);
                    return Err(send_err.into());
                }
                continue; // Tiếp tục xử lý request tiếp theo
            }
        };

        // 🔍 HASH INTEGRITY CHECK: Log chi tiết từng transaction trước khi submit
        info!("📤 [TX FLOW] Preparing to submit {} transaction(s) via UDS", transactions_to_submit.len());
        for (i, tx_data) in transactions_to_submit.iter().enumerate() {
            let tx_hash = tx_hash::calculate_transaction_hash_hex(tx_data);
            info!("🔍 [TX HASH] Rust preparing to submit TX[{}]: hash={}, size={} bytes", 
                i, tx_hash, tx_data.len());
            // Try to decode transaction to get from/to/nonce
            if let Ok(tx) = Transaction::decode(tx_data.as_slice()) {
                let from_addr = if tx.from_address.len() >= 10 {
                    format!("0x{}...", hex::encode(&tx.from_address[..10]))
                } else {
                    hex::encode(&tx.from_address)
                };
                let to_addr = if tx.to_address.len() >= 10 {
                    format!("0x{}...", hex::encode(&tx.to_address[..10]))
                } else {
                    hex::encode(&tx.to_address)
                };
                info!("   📝 TX[{}]: hash={}, from={}, to={}, nonce={}", 
                    i, tx_hash, from_addr, to_addr, hex::encode(&tx.nonce));
            } else {
                info!("   📝 TX[{}]: hash={}, size={} bytes (cannot decode protobuf)", 
                    i, tx_hash, tx_data.len());
            }
        }
        
        // Calculate hash for logging (use first transaction)
        let first_tx_hash = if !transactions_to_submit.is_empty() {
            tx_hash::calculate_transaction_hash_hex(&transactions_to_submit[0])
        } else {
            "unknown".to_string()
        };
        
        info!("📤 [TX FLOW] Submitting {} transaction(s) via UDS: first_hash={}", 
            transactions_to_submit.len(), first_tx_hash);

        // Submit transactions to consensus
        // Each transaction is now a single Transaction protobuf message (not Transactions message)
        match client.submit(transactions_to_submit.clone()).await {
            Ok((block_ref, indices, _)) => {
                info!("✅ [TX FLOW] Transaction(s) included in block via UDS: first_hash={}, block={:?}, indices={:?}, count={}", 
                    first_tx_hash, block_ref, indices, transactions_to_submit.len());
                // Log chi tiết từng transaction đã được submit
                for (i, tx_data) in transactions_to_submit.iter().enumerate() {
                    let tx_hash = tx_hash::calculate_transaction_hash_hex(tx_data);
                    let index = if i < indices.len() { indices[i] } else { 0 };
                    if let Ok(tx) = Transaction::decode(tx_data.as_slice()) {
                        let from_addr = if tx.from_address.len() >= 10 {
                            format!("0x{}...", hex::encode(&tx.from_address[..10]))
                        } else {
                            hex::encode(&tx.from_address)
                        };
                        info!("   ✅ TX[{}] included: hash={}, from={}, nonce={}, block_index={}", 
                            i, tx_hash, from_addr, hex::encode(&tx.nonce), index);
                    } else {
                        info!("   ✅ TX[{}] included: hash={}, block_index={}", i, tx_hash, index);
                    }
                }
                
                let success_response = format!(
                    r#"{{"success":true,"tx_hash":"{}","block_ref":"{:?}","indices":{:?},"count":{}}}"#,
                    first_tx_hash, block_ref, indices, transactions_to_submit.len()
                );
                if let Err(e) = Self::send_response_string(&mut stream, &success_response).await {
                    error!("❌ [TX FLOW] Failed to send success response: {}", e);
                    return Err(e.into());
                }
            }
            Err(e) => {
                error!("❌ [TX FLOW] Transaction submission failed via UDS: first_hash={}, count={}, error={}", 
                    first_tx_hash, transactions_to_submit.len(), e);
                let error_response = format!(
                    r#"{{"success":false,"error":"Transaction submission failed: {}"}}"#,
                    e.to_string().replace('"', "\\\"")
                );
                if let Err(send_err) = Self::send_response_string(&mut stream, &error_response).await {
                    error!("❌ [TX FLOW] Failed to send error response: {}", send_err);
                    return Err(send_err.into());
                }
            }
        }
        
        // Sau khi xử lý xong một request, tiếp tục loop để xử lý request tiếp theo
        // Connection sẽ được giữ mở cho đến khi client đóng (EOF)
        }
    }

    async fn send_response_string(stream: &mut UnixStream, response: &str) -> Result<()> {
        let response_bytes = response.as_bytes();
        let response_len = (response_bytes.len() as u32).to_be_bytes();
        
        // Write length prefix
        stream.write_all(&response_len).await?;
        // Write response data
        stream.write_all(response_bytes).await?;
        stream.flush().await?;
        
        Ok(())
    }
}

