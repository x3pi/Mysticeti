// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use tracing::{info, error, warn};
use crate::node::tx_submitter::TransactionSubmitter;
use crate::types::tx_hash::calculate_transaction_hash_hex;

/// Simple HTTP RPC server for submitting transactions
/// Supports both HTTP POST and length-prefixed binary protocols
pub struct RpcServer {
    transaction_client: Arc<dyn TransactionSubmitter>,
    port: u16,
    /// Optional node reference for readiness checking
    /// If provided, transactions are rejected when node is not ready (catching up, transitioning, etc.)
    node: Option<Arc<Mutex<crate::node::ConsensusNode>>>,
}

impl RpcServer {
    #[allow(dead_code)] // Kept for backward compatibility, but with_node is preferred
    pub fn new(transaction_client: Arc<dyn TransactionSubmitter>, port: u16) -> Self {
        Self {
            transaction_client,
            port,
            node: None,
        }
    }

    /// Create RPC server with node reference for readiness checking
    /// This allows the server to reject transactions when the node is not ready
    /// (e.g., catching up, transitioning epochs, or not fully initialized)
    pub fn with_node(
        transaction_client: Arc<dyn TransactionSubmitter>,
        port: u16,
        node: Arc<Mutex<crate::node::ConsensusNode>>,
    ) -> Self {
        Self {
            transaction_client,
            port,
            node: Some(node),
        }
    }

    pub async fn start(self) -> Result<()> {
        use tokio::sync::Semaphore;
        
        let addr = format!("127.0.0.1:{}", self.port);
        let listener = TcpListener::bind(&addr).await?;
        info!("RPC server started on {} (supports HTTP POST and length-prefixed binary)", addr);

        // Limit concurrent connections để tránh quá tải
        // Tăng từ 200 lên 500 cho production với thông lượng lớn
        let semaphore = Arc::new(Semaphore::new(500));

        loop {
            info!("🔌 [TX FLOW] Waiting for new connection on RPC server...");
            let (mut stream, peer_addr) = match listener.accept().await {
                Ok((s, addr)) => (s, addr),
                Err(e) => {
                    error!("❌ [TX FLOW] Failed to accept connection: {}", e);
                    continue;
                }
            };
            info!("🔌 [TX FLOW] New connection accepted from {:?} (local={:?})", peer_addr, stream.local_addr().ok());
            
            // Tối ưu TCP connection cho localhost với thông lượng lớn
            // Set TCP options trên stream để tối ưu cho localhost
            if let Err(e) = stream.set_nodelay(true) {
                warn!("Failed to set TCP_NODELAY: {}", e);
            }
            
            let client = self.transaction_client.clone();
            let node = self.node.clone();
            let permit = semaphore.clone().acquire_owned().await;

            tokio::spawn(async move {
                info!("📥 [TX FLOW] Spawned handler for connection from {:?}", peer_addr);
                // Release permit khi task hoàn thành
                let _permit = permit;
                
                // Try to detect protocol: length-prefixed binary or HTTP
                // Read first 4 bytes to check if it's a length prefix
                // Wrap với timeout ngắn hơn để tránh connection bị treo và phát hiện connection chết sớm
                info!("📥 [TX FLOW] Waiting to read length prefix (4 bytes) from {:?}...", peer_addr);
                let mut len_buf = [0u8; 4];
                let read_len_result = tokio::time::timeout(
                    std::time::Duration::from_secs(5), // Giảm từ 30s xuống 5s để phát hiện connection chết sớm
                    stream.read_exact(&mut len_buf)
                ).await;

                // SPECIAL TESTING MODE: If first 4 bytes are "TEST", treat as raw text transaction
                let is_testing_mode = len_buf == [b'T', b'E', b'S', b'T'];

                if is_testing_mode {
                    // TESTING MODE: Read the rest as raw transaction data
                    info!("🧪 [TX FLOW] Detected TESTING MODE from {:?}, reading raw transaction data...", peer_addr);
                    let mut tx_data = Vec::new();
                    let mut buf = [0u8; 1024];
                    loop {
                        match stream.read(&mut buf).await {
                            Ok(0) => break, // EOF
                            Ok(n) => tx_data.extend_from_slice(&buf[..n]),
                            Err(e) => {
                                error!("❌ [TX FLOW] Failed to read testing transaction data from {:?}: {}", peer_addr, e);
                                return;
                            }
                        }
                    }

                    info!("📥 [TX FLOW] Received testing transaction: {} bytes", tx_data.len());

                    // Process as raw transaction data (just use the data as-is)
                    let is_length_prefixed = false; // Mark as not length-prefixed for testing
                    if let Err(e) = Self::process_transaction_data(
                        &client,
                        &node,
                        &mut stream,
                        tx_data,
                        is_length_prefixed,
                    ).await {
                        error!("❌ [TX FLOW] Failed to process testing transaction: {}", e);
                        let _ = Self::send_binary_response(&mut stream, false, "Failed to process testing transaction").await;
                    } else {
                        info!("✅ [TX FLOW] Successfully processed testing transaction");
                        let _ = Self::send_binary_response(&mut stream, true, "Testing transaction submitted").await;
                    }
                    return;
                }

                match read_len_result {
                    Ok(Ok(_)) => {
                        let data_len = u32::from_be_bytes(len_buf) as usize;
                        info!("📥 [TX FLOW] Read length prefix: {} bytes from {:?}", data_len, peer_addr);
                        // Check if this looks like a length prefix (reasonable size: 1 byte to 10MB)
                        if data_len > 0 && data_len <= 10 * 1024 * 1024 {
                            // Length-prefixed binary protocol (used by Go txsender client)
                            info!("📥 [TX FLOW] Reading {} bytes of transaction data from {:?}...", data_len, peer_addr);

                            // Use the new codec module to read the frame with timeout
                            let tx_data_result = crate::network::codec::read_length_prefixed_frame_with_timeout(
                                &mut stream,
                                std::time::Duration::from_secs(10), // Timeout for data reading
                            ).await;

                            match tx_data_result {
                                Ok(tx_data) => {
                                    // Calculate actual transaction hash from protobuf data
                                    let tx_hash_preview = calculate_transaction_hash_hex(&tx_data);
                                    let tx_hash_short = if tx_hash_preview.len() >= 16 {
                                        &tx_hash_preview[..16]
                                    } else {
                                        &tx_hash_preview
                                    };

                                    info!("📥 [TX FLOW] Received length-prefixed transaction data via RPC: size={} bytes, hash={}...",
                                        tx_data.len(), tx_hash_short);

                                    // Process transaction data
                                    let is_length_prefixed = true; // Mark as length-prefixed protocol
                                    if let Err(e) = Self::process_transaction_data(
                                        &client,
                                        &node,
                                        &mut stream,
                                        tx_data.clone(),
                                        is_length_prefixed,
                                    ).await {
                                        error!("❌ [TX FLOW] Failed to process transaction (size={} bytes, hash_preview={}): {}",
                                            tx_data.len(), tx_hash_preview, e);
                                        // Send error response for length-prefixed protocol
                                        let _ = Self::send_binary_response(&mut stream, false, "Failed to process transaction").await;
                                    }
                                }
                                Err(e) => {
                                    error!("❌ [TX FLOW] Failed to read length-prefixed transaction data: {}", e);
                                    return;
                                }
                            }
                        } else {
                            // Not a valid length prefix, treat as HTTP
                            // Reconstruct the 4 bytes we read as part of HTTP request
                            let mut buffer = Vec::with_capacity(8192);
                            buffer.extend_from_slice(&len_buf);

                            // Read remaining data with timeout
                            let mut remaining = [0u8; 8188];
                            let read_remaining_result = tokio::time::timeout(
                                std::time::Duration::from_secs(5), // Giảm từ 30s xuống 5s
                                stream.read(&mut remaining)
                            ).await;

                            match read_remaining_result {
                                Ok(Ok(n)) => {
                                    buffer.extend_from_slice(&remaining[..n]);
                                    let request = String::from_utf8_lossy(&buffer);

                                    if request.starts_with("POST /submit") {
                                        // Extract transaction data from HTTP body
                                        let body_start = request.find("\r\n\r\n")
                                            .or_else(|| request.find("\n\n"))
                                            .map(|i| i + 4)
                                            .unwrap_or(0);

                                        let body = &request[body_start..];
                                        let tx_data = if body.starts_with("0x") || body.chars().all(|c| c.is_ascii_hexdigit()) {
                                            hex::decode(body.trim().trim_start_matches("0x"))
                                                .unwrap_or_else(|_| body.as_bytes().to_vec())
                                        } else {
                                            body.trim().as_bytes().to_vec()
                                        };

                                        info!("📥 [TX FLOW] Received HTTP POST transaction data via RPC: size={} bytes", tx_data.len());

                                        // Process transaction data (HTTP protocol)
                                        let is_length_prefixed = false;
                                        if let Err(e) = Self::process_transaction_data(
                                            &client,
                                            &node,
                                            &mut stream,
                                            tx_data,
                                            is_length_prefixed,
                                        ).await {
                                            error!("Failed to process transaction: {}", e);
                                        }
                                    } else {
                                        // Return 404 for other requests
                                        let response = "HTTP/1.1 404 Not Found\r\n\r\n";
                                        let _ = stream.write_all(response.as_bytes()).await;
                                    }
                                }
                                Ok(Err(e)) => {
                                    error!("❌ [TX FLOW] Failed to read HTTP request: {}", e);
                                    return;
                                }
                                Err(_) => {
                                    error!("❌ [TX FLOW] Timeout reading HTTP request (timeout after 5s)");
                                    return;
                                }
                            }
                        }
                    }
                    Ok(Err(e)) => {
                        error!("❌ [TX FLOW] Failed to read length prefix from {:?}: {} (connection may be closed by client)", peer_addr, e);
                        return;
                    }
                    Err(_) => {
                        error!("❌ [TX FLOW] Timeout reading length prefix from {:?} (timeout after 5s) - connection may be dead or client disconnected", peer_addr);
                        return;
                    }
                }
            });
        }
    }
    
    async fn process_transaction_data(
        client: &Arc<dyn TransactionSubmitter>,
        node: &Option<Arc<Mutex<crate::node::ConsensusNode>>>,
        stream: &mut tokio::net::TcpStream,
        tx_data: Vec<u8>,
        is_length_prefixed: bool,
    ) -> Result<()> {
        use prost::Message;
        
        #[allow(dead_code)]
        mod proto {
            include!(concat!(env!("OUT_DIR"), "/transaction.rs"));
        }
        use proto::{Transaction, Transactions};
        
        // THỐNG NHẤT: Hỗ trợ cả Transactions (nhiều transactions) và Transaction (single transaction)
        // Go-sub LUÔN gửi pb.Transactions (nhiều transactions)
        // Rust client có thể gửi Transactions hoặc Transaction protobuf
        let transactions_to_submit = match Transactions::decode(tx_data.as_slice()) {
            Ok(transactions_msg) => {
                // Format: pb.Transactions (nhiều transactions) - từ Go-sub
                if transactions_msg.transactions.is_empty() {
                    warn!("⚠️  [TX FLOW] Empty Transactions message received");
                    if is_length_prefixed {
                        Self::send_binary_response(stream, false, "Empty Transactions message").await?;
                    } else {
                        let response = r#"{"success":false,"error":"Empty Transactions message"}"#;
                        Self::send_response(stream, response, false).await?;
                    }
                    return Ok(());
                }
                
                info!("📦 [TX FLOW] Received Transactions message with {} transactions, splitting into individual transactions", 
                    transactions_msg.transactions.len());
                
                // Split Transactions message into individual Transaction messages
                // Mỗi transaction được encode riêng để submit vào consensus
                let mut individual_txs = Vec::new();
                for (idx, tx) in transactions_msg.transactions.iter().enumerate() {
                    // Encode each Transaction as individual protobuf message
                    let mut buf = Vec::new();
                    if let Err(e) = tx.encode(&mut buf) {
                        error!("❌ [TX FLOW] Failed to encode transaction[{}] from Transactions message: {}", idx, e);
                        continue;
                    }
                    individual_txs.push(buf);
                }
                
                if individual_txs.is_empty() {
                    error!("❌ [TX FLOW] No valid transactions after encoding from Transactions message");
                    if is_length_prefixed {
                        Self::send_binary_response(stream, false, "No valid transactions after encoding").await?;
                    } else {
                        let response = r#"{"success":false,"error":"No valid transactions after encoding"}"#;
                        Self::send_response(stream, response, false).await?;
                    }
                    return Ok(());
                }
                
                info!("✅ [TX FLOW] Split Transactions message into {} individual transactions for consensus", individual_txs.len());
                individual_txs
            }
            Err(_) => {
                // Không phải Transactions, thử decode như single Transaction
                match Transaction::decode(tx_data.as_slice()) {
                    Ok(tx) => {
                        // Format: pb.Transaction (single transaction) - từ Rust client
                        info!("📦 [TX FLOW] Received single Transaction message, encoding for consensus");
                        let mut buf = Vec::new();
                        if let Err(e) = tx.encode(&mut buf) {
                            error!("❌ [TX FLOW] Failed to encode single Transaction: {}", e);
                            if is_length_prefixed {
                                Self::send_binary_response(stream, false, &format!("Failed to encode Transaction: {}", e)).await?;
                            } else {
                                let response = format!(r#"{{"success":false,"error":"Failed to encode Transaction: {}"}}"#, 
                                    e.to_string().replace('"', "\\\""));
                                Self::send_response(stream, &response, false).await?;
                            }
                            return Ok(());
                        }
                        vec![buf]
                    }
                    Err(e) => {
                        // Không phải Transactions cũng không phải Transaction protobuf
                        // Có thể là raw bytes từ Rust client (backward compatibility)
                        error!("❌ [TX FLOW] Failed to decode as Transactions or Transaction protobuf: {}", e);
                        error!("❌ [TX FLOW] Data preview (first 100 bytes): {}", 
                            hex::encode(&tx_data[..tx_data.len().min(100)]));
                        warn!("⚠️  [TX FLOW] Raw bytes received (not protobuf), this format is deprecated. Please use Transactions or Transaction protobuf.");
                        
                        // Backward compatibility: Thử xử lý như raw transaction data
                        // Tạo một Transaction protobuf từ raw bytes (nếu có thể)
                        // Hoặc reject với error message rõ ràng
                        if is_length_prefixed {
                            Self::send_binary_response(stream, false, "Invalid protobuf format. Expected Transactions or Transaction protobuf.").await?;
                        } else {
                            let response = r#"{"success":false,"error":"Invalid protobuf format. Expected Transactions or Transaction protobuf. Please use protobuf encoding."}"#;
                            Self::send_response(stream, response, false).await?;
                        }
                        return Ok(());
                    }
                }
            }
        };
        
        // Log chi tiết từng transaction trước khi submit
        info!("📤 [TX FLOW] Preparing to submit {} transaction(s) via RPC", transactions_to_submit.len());
        for (i, tx_data) in transactions_to_submit.iter().enumerate() {
            let tx_hash = calculate_transaction_hash_hex(tx_data);
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
            calculate_transaction_hash_hex(&transactions_to_submit[0])
        } else {
            "unknown".to_string()
        };
        
        // Check if node is ready to accept transactions or should queue them
        if let Some(ref node) = node {
            let node_guard = node.lock().await;
            let (is_ready, should_queue, reason) = node_guard.check_transaction_acceptance().await;
            drop(node_guard);

            if !is_ready {
                if should_queue {
                    // Queue transactions for next epoch instead of rejecting
                    info!("📦 [TX FLOW] Queueing {} transaction(s) for next epoch: {} (first_hash={})",
                        transactions_to_submit.len(), reason, first_tx_hash);

                    // Queue all transactions
                    let node_guard = node.lock().await;
                    for (i, tx_data) in transactions_to_submit.iter().enumerate() {
                        if let Err(e) = node_guard.queue_transaction_for_next_epoch(tx_data.clone()).await {
                            error!("❌ [TX FLOW] Failed to queue transaction {}: {}", i, e);
                        } else {
                            let tx_hash = calculate_transaction_hash_hex(tx_data);
                            info!("📦 [TX FLOW] Queued TX[{}]: hash={}", i, tx_hash);
                        }
                    }

                    if is_length_prefixed {
                        Self::send_binary_response(stream, true, "Transactions queued for next epoch").await?;
                    } else {
                        let response = r#"{"success":true,"message":"Transactions queued for next epoch"}"#;
                        Self::send_response(stream, response, true).await?;
                    }
                    return Ok(());
                } else {
                    // Reject transactions
                    warn!("🚫 [TX FLOW] Transaction rejected: {} (first_hash={}, count={})",
                        reason, first_tx_hash, transactions_to_submit.len());
                    if is_length_prefixed {
                        Self::send_binary_response(stream, false, &reason).await?;
                    } else {
                        let response = format!(r#"{{"success":false,"error":"{}"}}"#,
                            reason.replace('"', "\\\""));
                        Self::send_response(stream, &response, false).await?;
                    }
                    return Ok(());
                }
            }
        }
        
        info!("📤 [TX FLOW] Submitting {} transaction(s) via RPC: first_hash={}", 
            transactions_to_submit.len(), first_tx_hash);

        match client.submit(transactions_to_submit.clone()).await {
            Ok((block_ref, indices, _)) => {
                info!("✅ [TX FLOW] Transaction(s) included in block: first_hash={}, block={:?}, indices={:?}, count={}",
                    first_tx_hash, block_ref, indices, transactions_to_submit.len());

                // NOTE: Hash tracking moved to commit processor to ensure only truly committed transactions are tracked
                // This prevents false positives where submitted-but-not-committed transactions get tracked

                // Log chi tiết từng transaction đã được submit
                for (i, tx_data) in transactions_to_submit.iter().enumerate() {
                    let tx_hash = calculate_transaction_hash_hex(tx_data);
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
                if is_length_prefixed {
                    // LOCALHOST OPTIMIZATION: Không gửi response cho localhost
                    // Go client không đọc response, giữ connection mở để gửi nhiều transactions liên tục
                    // Chỉ log để debug, không gửi response để tối ưu throughput
                    // Connection sẽ được giữ mở và tái sử dụng cho batch tiếp theo
                } else {
                    // Send HTTP JSON response
                    let response = format!(
                        r#"{{"success":true,"tx_hash":"{}","block_ref":"{:?}","indices":{:?},"count":{}}}"#,
                        first_tx_hash, block_ref, indices, transactions_to_submit.len()
                    );
                    Self::send_response(stream, &response, true).await?;
                }
            }
            Err(e) => {
                error!("❌ [TX FLOW] Transaction submission failed: first_hash={}, error={}", first_tx_hash, e);
                if is_length_prefixed {
                    // Send binary error response
                    if let Err(e) = Self::send_binary_response(stream, false, &format!("Transaction submission failed: {}", e)).await {
                        error!("❌ [TX FLOW] Failed to send binary error response: {}", e);
                    }
                } else {
                    // Send HTTP JSON error response
                    let response = format!(r#"{{"success":false,"error":"{}"}}"#, e);
                    Self::send_response(stream, &response, false).await?;
                }
            }
        }
        
        Ok(())
    }
    
    async fn send_response(
        stream: &mut tokio::net::TcpStream,
        json_body: &str,
        is_http: bool,
    ) -> Result<()> {
        if is_http {
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{}",
                json_body
            );
            stream.write_all(response.as_bytes()).await?;
        } else {
            // For length-prefixed binary, send JSON response with length prefix
            let json_bytes = json_body.as_bytes();
            let len_buf = (json_bytes.len() as u32).to_be_bytes();
            stream.write_all(&len_buf).await?;
            stream.write_all(json_bytes).await?;
        }
        Ok(())
    }
    
    /// Send binary response for length-prefixed protocol
    /// Format: [1 byte: success (0x01=OK, 0x00=ERROR)][4 bytes: message length][message bytes]
    async fn send_binary_response(
        stream: &mut tokio::net::TcpStream,
        success: bool,
        message: &str,
    ) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        
        // Kiểm tra stream có thể write không
        if let Err(e) = stream.writable().await {
            error!("❌ [TX FLOW] Stream is not writable, cannot send response: {}", e);
            return Err(e.into());
        }
        
        let success_byte = if success { 0x01u8 } else { 0x00u8 };
        let message_bytes = message.as_bytes();
        let message_len = message_bytes.len() as u32;
        
        // Write: [1 byte success][4 bytes length][message]
        // Write response với error handling chi tiết
        match stream.write_u8(success_byte).await {
            Ok(_) => {}
            Err(e) => {
                error!("❌ [TX FLOW] Failed to write success byte to stream: {}", e);
                return Err(e.into());
            }
        }
        
        match stream.write_u32(message_len).await {
            Ok(_) => {}
            Err(e) => {
                error!("❌ [TX FLOW] Failed to write message length to stream: {}", e);
                return Err(e.into());
            }
        }
        
        match stream.write_all(message_bytes).await {
            Ok(_) => {}
            Err(e) => {
                error!("❌ [TX FLOW] Failed to write message to stream: {}", e);
                return Err(e.into());
            }
        }
        
        match stream.flush().await {
            Ok(_) => {}
            Err(e) => {
                error!("❌ [TX FLOW] Failed to flush stream: {}", e);
                return Err(e.into());
            }
        }
        
        info!("📤 [TX FLOW] Sent binary response: success={}, message={}, message_len={}", success, message, message_len);
        
        Ok(())
    }
}

