// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use crate::consensus::tx_recycler::TxRecycler;
use crate::node::tx_submitter::TransactionSubmitter;
use crate::node::ConsensusNode;
use anyhow::Result;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, RwLock};
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
    /// Direct access to the pending transactions queue for lock-free queuing during transitions
    pending_transactions_queue: Option<Arc<Mutex<Vec<Vec<u8>>>>>,
    /// Storage path for persistence during lock-free queuing
    storage_path: Option<std::path::PathBuf>,
    /// Static peer RPC addresses for forwarding transactions (SyncOnly mode)
    peer_rpc_addresses: Vec<String>,
    /// Dynamic peer addresses from PeerDiscoveryService (takes precedence if set)
    peer_discovery_addresses: Option<Arc<RwLock<Vec<String>>>>,
    /// TX recycler for tracking submitted TXs and re-submitting stale ones
    tx_recycler: Option<Arc<TxRecycler>>,
}

impl TxSocketServer {
    /// Create UDS server with node reference for readiness checking
    pub fn with_node(
        socket_path: String,
        transaction_client: Arc<dyn TransactionSubmitter>,
        node: Arc<Mutex<ConsensusNode>>,
        is_transitioning: Arc<AtomicBool>,
        pending_transactions_queue: Arc<Mutex<Vec<Vec<u8>>>>,
        storage_path: std::path::PathBuf,
        peer_rpc_addresses: Vec<String>,
    ) -> Self {
        Self {
            socket_path,
            transaction_client,
            node: Some(node),
            is_transitioning: Some(is_transitioning),
            pending_transactions_queue: Some(pending_transactions_queue),
            storage_path: Some(storage_path),
            peer_rpc_addresses,
            peer_discovery_addresses: None,
            tx_recycler: None,
        }
    }

    /// Set dynamic peer discovery addresses (takes precedence over static config)
    pub fn with_peer_discovery(mut self, addresses: Arc<RwLock<Vec<String>>>) -> Self {
        self.peer_discovery_addresses = Some(addresses);
        self
    }

    /// Set TX recycler for tracking and re-submitting stale TXs
    pub fn with_tx_recycler(mut self, recycler: Arc<TxRecycler>) -> Self {
        self.tx_recycler = Some(recycler);
        self
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
                    let pending_transactions_queue = self.pending_transactions_queue.clone();
                    let storage_path = self.storage_path.clone();
                    let peer_rpc_addresses = self.peer_rpc_addresses.clone();
                    let peer_discovery_addresses = self.peer_discovery_addresses.clone();
                    let tx_recycler = self.tx_recycler.clone();

                    tokio::spawn(async move {
                        info!("🔌 [DEBUG] Spawned handler for UDS connection");
                        if let Err(e) = Self::handle_connection(
                            stream,
                            client,
                            node,
                            is_transitioning,
                            pending_transactions_queue,
                            storage_path,
                            peer_rpc_addresses,
                            peer_discovery_addresses,
                            tx_recycler,
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
        pending_transactions_queue: Option<Arc<Mutex<Vec<Vec<u8>>>>>,
        storage_path: Option<std::path::PathBuf>,
        peer_rpc_addresses: Vec<String>,
        _peer_discovery_addresses: Option<Arc<RwLock<Vec<String>>>>,
        tx_recycler: Option<Arc<TxRecycler>>,
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

            // Batch-level logging only (per-TX hash logging removed for performance)
            info!(
                "📥 [TX FLOW] Received transaction batch via UDS: size={} bytes",
                data_len
            );

            // THỐNG NHẤT: Go LUÔN gửi pb.Transactions (nhiều transactions)
            // Rust CHỈ xử lý Transactions message, không xử lý single Transaction hoặc raw data
            use prost::Message;

            #[allow(dead_code)]
            mod proto {
                include!(concat!(env!("OUT_DIR"), "/transaction.rs"));
            }
            use proto::Transactions;

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
            // FIX: Queue TXs instead of rejecting — Go-sub doesn't retry, rejection = permanent loss
            if let Some(ref transitioning) = is_transitioning {
                if transitioning.load(Ordering::SeqCst) {
                    warn!("⚡ [TX FLOW] Epoch transition in progress. Queueing {} transactions (LOCK-FREE).", transactions_to_submit.len());

                    // TRULY LOCK-FREE PATH: Use direct access to queue if available
                    if let (Some(ref queue), Some(ref path)) =
                        (&pending_transactions_queue, &storage_path)
                    {
                        for tx_data in &transactions_to_submit {
                            if let Err(e) =
                                crate::node::queue::queue_transaction(queue, path, tx_data.clone())
                                    .await
                            {
                                error!("❌ [TX FLOW] Failed to queue TX during transition (lock-free): {}", e);
                            }
                        }

                        let success_response = format!(
                            r#"{{"success":true,"queued":true,"message":"Queued {} TXs during epoch transition (lock-free)"}}"#,
                            transactions_to_submit.len()
                        );
                        if let Err(e) =
                            Self::send_response_string(&mut stream, &success_response).await
                        {
                            error!("❌ [TX FLOW] Failed to send queue response: {}", e);
                            return Err(e.into());
                        }
                        return Ok(());
                    } else if let Some(ref node_arc) = node {
                        // Fallback to locking node if direct queue access is not available
                        match tokio::time::timeout(
                            std::time::Duration::from_millis(100), // reduced timeout
                            node_arc.lock(),
                        )
                        .await
                        {
                            Ok(node_guard) => {
                                if let Err(e) = node_guard
                                    .queue_transactions_for_next_epoch(
                                        transactions_to_submit.clone(),
                                    )
                                    .await
                                {
                                    error!(
                                        "❌ [TX FLOW] Failed to queue TXs during transition: {}",
                                        e
                                    );
                                }
                                drop(node_guard);
                                let success_response = format!(
                                    r#"{{"success":true,"queued":true,"message":"Queued {} TXs during epoch transition"}}"#,
                                    transactions_to_submit.len()
                                );
                                if let Err(e) =
                                    Self::send_response_string(&mut stream, &success_response).await
                                {
                                    error!("❌ [TX FLOW] Failed to send queue response: {}", e);
                                    return Err(e.into());
                                }
                                return Ok(());
                            }
                            Err(_) => {
                                // Lock timeout during transition — add to pending queue directly
                                warn!("⏳ [TX FLOW] Lock timeout during transition. Storing {} TXs in memory for retry.", transactions_to_submit.len());
                                let error_response = r#"{"success":false,"error":"Node busy (epoch transition in progress), transactions will be retried"}"#;
                                if let Err(e) =
                                    Self::send_response_string(&mut stream, error_response).await
                                {
                                    error!("❌ [TX FLOW] Failed to send timeout response: {}", e);
                                    return Err(e.into());
                                }
                                continue;
                            }
                        }
                    }
                }
            }

            // Check if node is ready (lock-free fast path already passed)
            info!(
                "🔍 [TX FLOW] Checking transaction acceptance for {} TXs",
                transactions_to_submit.len()
            );
            if let Some(ref node) = node {
                // Lock-free check passed, now try to acquire lock
                // Use short timeout as safety net (should rarely trigger since we checked flag)
                // 🛠 FIX: Epoch transitions (wait_for_commit_processor wait) can block for 10s.
                // 30s prevents early rejection.
                let lock_result =
                    tokio::time::timeout(std::time::Duration::from_secs(30), node.lock()).await;

                match lock_result {
                    Ok(node_guard) => {
                        let (should_accept, should_queue, reason) =
                            node_guard.check_transaction_acceptance().await;

                        if should_queue {
                            // Queue transactions for next epoch
                            info!(
                                "📦 [TX FLOW] Queueing {} transactions for next epoch: {}",
                                transactions_to_submit.len(),
                                reason
                            );
                            if let Err(e) = node_guard
                                .queue_transactions_for_next_epoch(transactions_to_submit.clone())
                                .await
                            {
                                error!("❌ [TX FLOW] Failed to queue transactions: {}", e);
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

                            // Regular rejection
                            warn!(
                                "🚫 [TX FLOW] Rejecting {} transactions via UDS: {}",
                                transactions_to_submit.len(),
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
                        // Lock acquisition timeout - node is busy processing other transactions
                        warn!("⏳ [TX FLOW] Lock timeout (30s) - node busy. Asking client to retry {} transactions.", 
                        transactions_to_submit.len());

                        // Return a retry-able error that does NOT contain "epoch transition"
                        // to avoid Go channel workers treating this as permanent rejection
                        let error_response = r#"{"success":false,"error":"Node busy processing requests, please retry"}"#;
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

            // Submit transactions to consensus
            info!(
                "📤 [TX FLOW] Submitting {} transaction(s) to consensus via UDS",
                transactions_to_submit.len()
            );

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
                        if should_queue_final {
                            // Epoch transition started between initial check and submission - queue transaction instead
                            warn!("⚠️ [RACE CONDITION] Epoch transition started between initial check and submission - queueing transaction instead: {}", reason_final);
                            // Queue all transactions (node_guard is still held)
                            if let Err(e) = node_guard
                                .queue_transactions_for_next_epoch(transactions_to_submit.clone())
                                .await
                            {
                                error!("❌ [TX FLOW] Failed to queue transactions after race condition detection: {}", e);
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
                            continue; // Don't submit to consensus
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
                            continue; // Don't submit to consensus
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
                        continue; // Don't submit to consensus
                    }
                }
            } else {
                false // No node reference, continue with submission
            };

            if should_queue_final {
                return Ok(()); // Already handled above
            }

            // Submit transactions to consensus in sub-batches
            // Consensus limits have been increased to 10,000
            const MAX_BUNDLE_SIZE: usize = 10000;
            let total_tx_count = transactions_to_submit.len();

            let mut all_succeeded = true;
            let mut total_submitted = 0usize;
            let mut _last_block_ref = None;
            let mut last_error = String::new();

            for (chunk_idx, chunk) in transactions_to_submit.chunks(MAX_BUNDLE_SIZE).enumerate() {
                let chunk_vec: Vec<Vec<u8>> = chunk.to_vec();
                let chunk_len = chunk_vec.len();

                info!(
                    "🚀 [TX FLOW] Submitting sub-batch {}: {} TXs (total progress: {}/{})",
                    chunk_idx + 1,
                    chunk_len,
                    total_submitted,
                    total_tx_count
                );

                match client.submit(chunk_vec.clone()).await {
                    Ok((block_ref, _indices, status_receiver)) => {
                        total_submitted += chunk_len;
                        _last_block_ref = Some(format!("{:?}", block_ref));
                        info!(
                            "✅ [TX FLOW] Sub-batch {} included: {} TXs in block {:?} (progress: {}/{})",
                            chunk_idx + 1, chunk_len, block_ref, total_submitted, total_tx_count
                        );

                        // Track submitted TXs for recycling (re-submit if not committed)
                        if let Some(ref recycler) = tx_recycler {
                            recycler.track_submitted(&chunk_vec).await;
                        }

                        // Track submitted transactions for epoch transition recovery
                        if let Some(ref node_arc) = node.as_ref() {
                            let node_guard = node_arc.lock().await;
                            let mut epoch_pending =
                                node_guard.epoch_pending_transactions.lock().await;
                            for tx_data in &chunk_vec {
                                epoch_pending.push(tx_data.clone());
                            }
                        }

                        let _client_clone = client.clone();
                        let _chunk_clone = chunk_vec.clone();
                        tokio::spawn(async move {
                            match status_receiver.await {
                                Ok(consensus_core::BlockStatus::Sequenced(block)) => {
                                    info!(
                                        "✅ [TX STATUS] Block {:?} was sequenced and finalized.",
                                        block
                                    );
                                }
                                Ok(consensus_core::BlockStatus::GarbageCollected(gc_block)) => {
                                    warn!("♻️ [TX STATUS] Block {:?} was Garbage Collected. TxRecycler will handle re-submission if necessary.", gc_block);
                                }
                                Err(e) => {
                                    warn!("⚠️ [TX STATUS] Failed to receive block status: {}", e);
                                }
                            }
                        });
                    }
                    Err(e) => {
                        all_succeeded = false;
                        last_error = e.to_string();
                        error!(
                            "❌ [TX FLOW] Sub-batch {} submission failed: {} TXs, error={}",
                            chunk_idx + 1,
                            chunk_len,
                            e
                        );
                        // Don't break — try remaining sub-batches
                    }
                }
            }

            if all_succeeded {
                let success_response = format!(r#"{{"success":true,"count":{}}}"#, total_submitted);
                if let Err(e) = Self::send_response_string(&mut stream, &success_response).await {
                    error!("❌ [TX FLOW] Failed to send success response: {}", e);
                    return Err(e.into());
                }
            } else if total_submitted > 0 {
                // Partial success
                let response = format!(
                    r#"{{"success":true,"partial":true,"submitted":{},"total":{},"error":"{}"}}"#,
                    total_submitted,
                    total_tx_count,
                    last_error.replace('"', "\\\"")
                );
                if let Err(e) = Self::send_response_string(&mut stream, &response).await {
                    error!("❌ [TX FLOW] Failed to send partial response: {}", e);
                    return Err(e.into());
                }
            } else {
                let error_response = format!(
                    r#"{{"success":false,"error":"Transaction submission failed: {}"}}"#,
                    last_error.replace('"', "\\\"")
                );
                if let Err(e) = Self::send_response_string(&mut stream, &error_response).await {
                    error!("❌ [TX FLOW] Failed to send error response: {}", e);
                    return Err(e.into());
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
