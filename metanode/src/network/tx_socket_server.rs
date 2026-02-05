// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use crate::node::tx_submitter::TransactionSubmitter;
use crate::node::ConsensusNode;
use anyhow::Result;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

/// Unix Domain Socket server for transaction submission
/// Faster than HTTP for local IPC communication
pub struct TxSocketServer {
    socket_path: String,
    transaction_client: Arc<dyn TransactionSubmitter>,
    /// Optional node reference for readiness checking
    node: Option<Arc<Mutex<ConsensusNode>>>,
    /// Lock-free flag to check epoch transition status without acquiring node lock
    is_transitioning: Option<Arc<AtomicBool>>,
    /// Peer RPC addresses for forwarding transactions (SyncOnly mode)
    peer_rpc_addresses: Vec<String>,
}

impl TxSocketServer {
    /// Create UDS server with node reference for readiness checking
    pub fn with_node(
        socket_path: String,
        transaction_client: Arc<dyn TransactionSubmitter>,
        node: Arc<Mutex<ConsensusNode>>,
        is_transitioning: Arc<AtomicBool>,
        peer_rpc_addresses: Vec<String>,
    ) -> Self {
        Self {
            socket_path,
            transaction_client,
            node: Some(node),
            is_transitioning: Some(is_transitioning),
            peer_rpc_addresses,
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

        // DEBUG: Log that we're waiting for connections
        info!(
            "🔌 [DEBUG] UDS server waiting for connections on {}",
            self.socket_path
        );

        // Set socket permissions (read/write for owner and group)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o660);
            std::fs::set_permissions(&self.socket_path, perms)?;
        }

        loop {
            // DEBUG: Log before accepting
            info!("🔌 [DEBUG] UDS server waiting for connections...");

            match listener.accept().await {
                Ok((stream, addr)) => {
                    info!(
                        "🔌 [TX FLOW] ✅ ACCEPTED new UDS connection from client: {:?}",
                        addr
                    );

                    let client = self.transaction_client.clone();
                    let node = self.node.clone();
                    let is_transitioning = self.is_transitioning.clone();
                    let peer_addresses = self.peer_rpc_addresses.clone();

                    tokio::spawn(async move {
                        info!("🔌 [DEBUG] Spawned handler for UDS connection");
                        if let Err(e) = Self::handle_connection(
                            stream,
                            client,
                            node,
                            is_transitioning,
                            peer_addresses,
                        )
                        .await
                        {
                            error!("Error handling UDS connection: {}", e);
                        }
                    });
                }
                Err(e) => {
                    error!("❌ [UDS ERROR] Failed to accept UDS connection: {}", e);
                    // Continue loop instead of breaking
                }
            }
        }
    }

    async fn handle_connection(
        mut stream: UnixStream,
        client: Arc<dyn TransactionSubmitter>,
        node: Option<Arc<Mutex<ConsensusNode>>>,
        is_transitioning: Option<Arc<AtomicBool>>,
        peer_rpc_addresses: Vec<String>,
    ) -> Result<()> {
        // PERSISTENT CONNECTION: Xử lý multiple requests trên cùng một connection
        // Điều này cho phép Go client gửi nhiều batches qua cùng một connection
        // Tối ưu cho localhost với throughput cao
        loop {
            // Use the new codec module to read the length-prefixed frame
            let tx_data_result =
                crate::network::codec::read_length_prefixed_frame(&mut stream).await;

            let tx_data = match tx_data_result {
                Ok(data) => data,
                Err(e) => {
                    // Check if it's EOF (connection closed by client)
                    if let Some(io_err) = e.downcast_ref::<std::io::Error>() {
                        if io_err.kind() == std::io::ErrorKind::UnexpectedEof {
                            info!("🔌 [TX FLOW] UDS connection closed by client (EOF)");
                            return Ok(());
                        }
                    }
                    // For other errors, send error response and continue
                    error!(
                        "❌ [TX FLOW] Failed to read length-prefixed frame from UDS: {}",
                        e
                    );
                    let error_response = format!(
                        r#"{{"success":false,"error":"Failed to read frame: {}"}}"#,
                        e
                    );
                    if let Err(send_err) =
                        Self::send_response_string(&mut stream, &error_response).await
                    {
                        error!("❌ [TX FLOW] Failed to send error response: {}", send_err);
                        return Err(send_err.into());
                    }
                    continue; // Tiếp tục xử lý request tiếp theo
                }
            };

            let data_len = tx_data.len();

            // 🔍 HASH INTEGRITY CHECK: Calculate actual transaction hash from protobuf data
            use crate::types::tx_hash;
            let tx_hash_preview = tx_hash::calculate_transaction_hash_hex(&tx_data);
            let tx_hash_short = if tx_hash_preview.len() >= 16 {
                &tx_hash_preview[..16]
            } else {
                &tx_hash_preview
            };

            info!(
                "📥 [TX FLOW] Received transaction data via UDS: size={} bytes, hash={}...",
                data_len, tx_hash_short
            );
            info!(
                "🔍 [TX HASH] Rust received from Go-sub: full_hash={}, size={} bytes",
                tx_hash_preview, data_len
            );

            // DEBUG: Log raw bytes received
            info!(
                "🔍 [DEBUG] Raw data preview (first 50 bytes): {}",
                hex::encode(&tx_data[..tx_data.len().min(50)])
            );
            info!(
                "🔍 [DEBUG] Data starts with: 0x{:02x} 0x{:02x} 0x{:02x} 0x{:02x}",
                tx_data.get(0).unwrap_or(&0),
                tx_data.get(1).unwrap_or(&0),
                tx_data.get(2).unwrap_or(&0),
                tx_data.get(3).unwrap_or(&0)
            );

            // THỐNG NHẤT: Go LUÔN gửi pb.Transactions (nhiều transactions)
            // Rust CHỈ xử lý Transactions message, không xử lý single Transaction hoặc raw data
            use prost::Message;

            #[allow(dead_code)]
            mod proto {
                include!(concat!(env!("OUT_DIR"), "/transaction.rs"));
            }
            use proto::{Transaction, Transactions};

            // Go LUÔN gửi Transactions message
            let transactions_to_submit = match Transactions::decode(&tx_data[..]) {
                Ok(transactions_msg) => {
                    if transactions_msg.transactions.is_empty() {
                        warn!("⚠️  [TX FLOW] Empty Transactions message received from Go via UDS");
                        let error_response =
                            r#"{"success":false,"error":"Empty Transactions message"}"#;
                        if let Err(e) =
                            Self::send_response_string(&mut stream, error_response).await
                        {
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
                        let error_response =
                            r#"{"success":false,"error":"No valid transactions after encoding"}"#;
                        if let Err(e) =
                            Self::send_response_string(&mut stream, error_response).await
                        {
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
                    error!(
                        "❌ [TX FLOW] Data preview (first 100 bytes): {}",
                        hex::encode(&tx_data[..tx_data.len().min(100)])
                    );
                    let error_response = format!(
                        r#"{{"success":false,"error":"Invalid Transactions protobuf: {}"}}"#,
                        e.to_string().replace('"', "\\\"")
                    );
                    if let Err(send_err) =
                        Self::send_response_string(&mut stream, &error_response).await
                    {
                        error!("❌ [TX FLOW] Failed to send error response: {}", send_err);
                        return Err(send_err.into());
                    }
                    continue; // Tiếp tục xử lý request tiếp theo
                }
            };

            // LOCK-FREE CHECK: Fast path - check is_transitioning BEFORE touching the lock
            // This prevents blocking when epoch transition is in progress
            // The flag is set atomically at the START of transition, so we can check it without lock
            if let Some(ref transitioning) = is_transitioning {
                if transitioning.load(Ordering::SeqCst) {
                    warn!("⚡ [TX FLOW] Lock-free check: Epoch transition in progress. Returning retry immediately.");
                    let error_response = r#"{"success":false,"error":"Node busy (epoch transition in progress), please retry"}"#;
                    if let Err(e) = Self::send_response_string(&mut stream, error_response).await {
                        error!(
                            "❌ [TX FLOW] Failed to send transition-busy response: {}",
                            e
                        );
                        return Err(e.into());
                    }
                    continue; // Don't even attempt to acquire lock
                }
            }

            // Check if node is ready to accept transactions or should queue them
            // Only acquire lock after the lock-free check passed
            info!("🔍 [DEBUG] About to check initial transaction acceptance");
            if let Some(ref node) = node {
                // Lock-free check passed, now try to acquire lock
                // Use short timeout as safety net (should rarely trigger since we checked flag)
                let lock_result =
                    tokio::time::timeout(std::time::Duration::from_secs(1), node.lock()).await;

                match lock_result {
                    Ok(node_guard) => {
                        let (should_accept, should_queue, reason) =
                            node_guard.check_transaction_acceptance().await;
                        info!("🔍 [DEBUG] Initial check result: should_accept={}, should_queue={}, reason={}", should_accept, should_queue, reason);

                        if should_queue {
                            // Queue transactions for next epoch
                            info!(
                                "📦 [TX FLOW] Queueing {} transactions for next epoch: {}",
                                transactions_to_submit.len(),
                                reason
                            );
                            for tx_data in &transactions_to_submit {
                                let tx_hash =
                                    crate::types::tx_hash::calculate_transaction_hash_hex(tx_data);
                                info!(
                                    "📦 [TX FLOW] Queueing transaction: hash={}, reason={}",
                                    tx_hash, reason
                                );
                                if let Err(e) = node_guard
                                    .queue_transaction_for_next_epoch(tx_data.clone())
                                    .await
                                {
                                    error!("❌ [TX FLOW] Failed to queue transaction: hash={}, error={}", tx_hash, e);
                                }
                            }
                            drop(node_guard);

                            // Send success response (transaction is queued, will be processed in next epoch)
                            let success_response = format!(
                                r#"{{"success":true,"queued":true,"message":"Transaction queued for next epoch: {}"}}"#,
                                reason.replace('"', "\\\"")
                            );
                            if let Err(e) =
                                Self::send_response_string(&mut stream, &success_response).await
                            {
                                error!("❌ [TX FLOW] Failed to send queue response: {}", e);
                                return Err(e.into());
                            }
                            continue; // Tiếp tục xử lý request tiếp theo
                        }

                        if !should_accept {
                            // Check if this is SyncOnly mode (authority.is_none())
                            // In SyncOnly mode, forward TX to validators instead of rejecting
                            let is_sync_only = reason.contains("Node is still initializing");

                            if is_sync_only && !peer_rpc_addresses.is_empty() {
                                // SyncOnly mode: Forward transactions to validators
                                for tx_data in &transactions_to_submit {
                                    let tx_hash =
                                        crate::types::tx_hash::calculate_transaction_hash_hex(
                                            tx_data,
                                        );
                                    info!(
                                        "🔄 [TX FORWARD] SyncOnly node forwarding transaction: hash={}, {} validator addresses available",
                                        tx_hash, peer_rpc_addresses.len()
                                    );
                                }
                                drop(node_guard);

                                // Collect all TX data into single bytes (Transactions protobuf)
                                // Forward to validators using peer_rpc
                                use crate::network::peer_rpc::forward_transaction_to_validators;

                                // Forward each transaction individually
                                let mut forward_success = true;
                                let mut forward_error = String::new();

                                for tx_data in &transactions_to_submit {
                                    match forward_transaction_to_validators(
                                        &peer_rpc_addresses,
                                        tx_data,
                                    )
                                    .await
                                    {
                                        Ok(resp) if resp.success => {
                                            let tx_hash = crate::types::tx_hash::calculate_transaction_hash_hex(tx_data);
                                            info!("✅ [TX FORWARD] Successfully forwarded TX {} to validator", tx_hash);
                                        }
                                        Ok(resp) => {
                                            forward_success = false;
                                            forward_error = resp
                                                .error
                                                .unwrap_or_else(|| "Unknown error".to_string());
                                            break;
                                        }
                                        Err(e) => {
                                            forward_success = false;
                                            forward_error = e.to_string();
                                            break;
                                        }
                                    }
                                }

                                if forward_success {
                                    let success_response = r#"{"success":true,"forwarded":true,"message":"Transaction forwarded to validator"}"#;
                                    if let Err(e) =
                                        Self::send_response_string(&mut stream, success_response)
                                            .await
                                    {
                                        error!("❌ [TX FLOW] Failed to send forward success response: {}", e);
                                        return Err(e.into());
                                    }
                                } else {
                                    let error_response = format!(
                                        r#"{{"success":false,"error":"Forward failed: {}"}}"#,
                                        forward_error.replace('"', "\\\"")
                                    );
                                    if let Err(e) =
                                        Self::send_response_string(&mut stream, &error_response)
                                            .await
                                    {
                                        error!("❌ [TX FLOW] Failed to send forward error response: {}", e);
                                        return Err(e.into());
                                    }
                                }
                                continue; // Continue to next request
                            }

                            // Regular rejection (not SyncOnly or no peer addresses)
                            for tx_data in &transactions_to_submit {
                                let tx_hash =
                                    crate::types::tx_hash::calculate_transaction_hash_hex(tx_data);
                                warn!(
                                    "🚫 [TX FLOW] Rejecting transaction: hash={}, reason={}",
                                    tx_hash, reason
                                );
                            }
                            warn!(
                                "🚫 Transaction rejected via UDS: node not ready - {}",
                                reason
                            );
                            drop(node_guard);
                            let error_response = format!(
                                r#"{{"success":false,"error":"Node not ready to accept transactions: {}"}}"#,
                                reason.replace('"', "\\\"")
                            );
                            if let Err(e) =
                                Self::send_response_string(&mut stream, &error_response).await
                            {
                                error!("❌ [TX FLOW] Failed to send error response: {}", e);
                                return Err(e.into());
                            }
                            continue; // Tiếp tục xử lý request tiếp theo
                        }

                        drop(node_guard);
                    }
                    Err(_) => {
                        // Lock acquisition timeout - epoch transition is likely in progress
                        // Queue transactions instead of blocking indefinitely
                        warn!("⏳ [TX FLOW] Lock timeout (1s) - epoch transition likely in progress. Queueing {} transactions.", 
                        transactions_to_submit.len());

                        // We couldn't get the lock, so we can't call queue_transaction_for_next_epoch
                        // Return an error asking client to retry
                        let error_response = r#"{"success":false,"error":"Node busy (epoch transition in progress), please retry"}"#;
                        if let Err(e) =
                            Self::send_response_string(&mut stream, error_response).await
                        {
                            error!("❌ [TX FLOW] Failed to send timeout response: {}", e);
                            return Err(e.into());
                        }
                        continue; // Tiếp tục xử lý request tiếp theo
                    }
                }
            }

            // 🔍 HASH INTEGRITY CHECK: Log chi tiết từng transaction trước khi submit
            info!(
                "📤 [TX FLOW] Preparing to submit {} transaction(s) via UDS",
                transactions_to_submit.len()
            );
            for (i, tx_data) in transactions_to_submit.iter().enumerate() {
                let tx_hash = tx_hash::calculate_transaction_hash_hex(tx_data);
                info!(
                    "🔍 [TX HASH] Rust preparing to submit TX[{}]: hash={}, size={} bytes",
                    i,
                    tx_hash,
                    tx_data.len()
                );
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
                    info!(
                        "   📝 TX[{}]: hash={}, from={}, to={}, nonce={}",
                        i,
                        tx_hash,
                        from_addr,
                        to_addr,
                        hex::encode(&tx.nonce)
                    );
                } else {
                    info!(
                        "   📝 TX[{}]: hash={}, size={} bytes (cannot decode protobuf)",
                        i,
                        tx_hash,
                        tx_data.len()
                    );
                }
            }

            // Calculate hash for logging (use first transaction)
            let first_tx_hash = if !transactions_to_submit.is_empty() {
                tx_hash::calculate_transaction_hash_hex(&transactions_to_submit[0])
            } else {
                "unknown".to_string()
            };

            // CRITICAL: Double-check transaction acceptance RIGHT BEFORE submitting to consensus
            // This prevents race condition where epoch transition starts between initial check and submission
            // Also use timeout to prevent blocking during epoch transition
            let should_queue_final = if let Some(ref node) = node {
                let lock_result =
                    tokio::time::timeout(std::time::Duration::from_secs(1), node.lock()).await;

                match lock_result {
                    Ok(node_guard) => {
                        let (should_accept_final, should_queue_final, reason_final) =
                            node_guard.check_transaction_acceptance().await;

                        info!("🔍 [DEBUG] Final check result: should_accept_final={}, should_queue_final={}, reason_final={}", should_accept_final, should_queue_final, reason_final);
                        if should_queue_final {
                            // Epoch transition started between initial check and submission - queue transaction instead
                            warn!("⚠️ [RACE CONDITION] Epoch transition started between initial check and submission - queueing transaction instead: {}", reason_final);
                            // Queue all transactions (node_guard is still held)
                            for tx_data in &transactions_to_submit {
                                if let Err(e) = node_guard
                                    .queue_transaction_for_next_epoch(tx_data.clone())
                                    .await
                                {
                                    error!("❌ [TX FLOW] Failed to queue transaction after race condition detection: {}", e);
                                }
                            }
                            drop(node_guard);
                            // Send success response (transaction is queued)
                            let success_response = format!(
                                r#"{{"success":true,"queued":true,"message":"Transaction queued due to epoch transition: {}"}}"#,
                                reason_final.replace('"', "\\\"")
                            );
                            if let Err(e) =
                                Self::send_response_string(&mut stream, &success_response).await
                            {
                                error!("❌ [TX FLOW] Failed to send queue response: {}", e);
                                return Err(e.into());
                            }
                            return Ok(()); // Don't submit to consensus
                        }

                        if !should_accept_final {
                            // Node is not ready - reject transaction
                            warn!("🚫 [RACE CONDITION] Node became not ready between initial check and submission - rejecting: {}", reason_final);
                            drop(node_guard);
                            let error_response = format!(
                                r#"{{"success":false,"error":"Node not ready: {}"}}"#,
                                reason_final.replace('"', "\\\"")
                            );
                            if let Err(e) =
                                Self::send_response_string(&mut stream, &error_response).await
                            {
                                error!("❌ [TX FLOW] Failed to send error response: {}", e);
                                return Err(e.into());
                            }
                            return Ok(()); // Don't submit to consensus
                        }

                        drop(node_guard);
                        false // Continue with submission
                    }
                    Err(_) => {
                        // Lock acquisition timeout on final check
                        warn!("⏳ [TX FLOW] Final lock timeout (1s) - epoch transition likely in progress");
                        let error_response = r#"{"success":false,"error":"Node busy (epoch transition in progress), please retry"}"#;
                        if let Err(e) =
                            Self::send_response_string(&mut stream, error_response).await
                        {
                            error!("❌ [TX FLOW] Failed to send timeout response: {}", e);
                            return Err(e.into());
                        }
                        return Ok(()); // Don't submit to consensus
                    }
                }
            } else {
                false // No node reference, continue with submission
            };

            info!(
                "🔍 [DEBUG] Final check: should_queue_final={}",
                should_queue_final
            );
            if should_queue_final {
                return Ok(()); // Already handled above
            }

            info!(
                "📤 [TX FLOW] Submitting {} transaction(s) via UDS: first_hash={}",
                transactions_to_submit.len(),
                first_tx_hash
            );

            // Submit transactions to consensus
            // Each transaction is now a single Transaction protobuf message (not Transactions message)
            info!(
                "🚀 [DEBUG] About to call client.submit() with {} transactions",
                transactions_to_submit.len()
            );
            match client.submit(transactions_to_submit.clone()).await {
                Ok((block_ref, indices, _)) => {
                    info!("✅ [TX FLOW] Transaction(s) included in block via UDS: first_hash={}, block={:?}, indices={:?}, count={}",
                    first_tx_hash, block_ref, indices, transactions_to_submit.len());

                    // Track submitted transactions for epoch transition recovery
                    if let Some(ref node_arc) = node.as_ref() {
                        let node_guard = node_arc.lock().await;
                        let mut epoch_pending = node_guard.epoch_pending_transactions.lock().await;
                        for tx_data in &transactions_to_submit {
                            epoch_pending.push(tx_data.clone());
                        }
                    }
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
                            info!(
                                "   ✅ TX[{}] included: hash={}, from={}, nonce={}, block_index={}",
                                i,
                                tx_hash,
                                from_addr,
                                hex::encode(&tx.nonce),
                                index
                            );
                        } else {
                            info!(
                                "   ✅ TX[{}] included: hash={}, block_index={}",
                                i, tx_hash, index
                            );
                        }
                    }

                    let success_response = format!(
                        r#"{{"success":true,"tx_hash":"{}","block_ref":"{:?}","indices":{:?},"count":{}}}"#,
                        first_tx_hash,
                        block_ref,
                        indices,
                        transactions_to_submit.len()
                    );
                    if let Err(e) = Self::send_response_string(&mut stream, &success_response).await
                    {
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
                    if let Err(send_err) =
                        Self::send_response_string(&mut stream, &error_response).await
                    {
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
