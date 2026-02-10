// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

//! Peer RPC Server — HTTP endpoints for peer queries.
//!
//! ## Endpoints
//!
//! - `GET /peer_info` - Returns current node's epoch and block info
//! - `GET /health` - Health check endpoint
//! - `GET /get_epoch_boundary_data?epoch=X` - Epoch boundary data
//! - `GET /get_blocks?from=X&to=Y` - Block data range
//! - `POST /submit_transaction` - Forward transactions to consensus

use anyhow::Result;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{error, info, warn};

use crate::node::executor_client::ExecutorClient;
use crate::node::tx_submitter::TransactionSubmitter;

use super::types::*;

/// Peer RPC Server for exposing node info over HTTP
pub struct PeerRpcServer {
    /// Node ID
    node_id: usize,
    /// Port to listen on
    port: u16,
    /// Network address for this node
    network_address: String,
    /// Executor client for querying Go Master
    executor_client: Arc<ExecutorClient>,
    /// Optional transaction submitter for forwarding transactions to consensus
    transaction_submitter: Option<Arc<dyn TransactionSubmitter>>,
}

impl PeerRpcServer {
    /// Create new Peer RPC Server
    pub fn new(
        node_id: usize,
        port: u16,
        network_address: String,
        executor_client: Arc<ExecutorClient>,
    ) -> Self {
        Self {
            node_id,
            port,
            network_address,
            executor_client,
            transaction_submitter: None,
        }
    }

    /// Start the Peer RPC Server
    pub async fn start(self) -> Result<()> {
        // Listen on all interfaces for WAN access
        let addr = format!("0.0.0.0:{}", self.port);
        let listener = TcpListener::bind(&addr).await?;
        info!(
            "🌐 [PEER RPC] Started on {} (node_id={}, network_address={})",
            addr, self.node_id, self.network_address
        );

        let executor_client = Arc::clone(&self.executor_client);
        let transaction_submitter = self.transaction_submitter.clone();
        let node_id = self.node_id;
        let network_address = self.network_address.clone();

        loop {
            let (mut stream, peer_addr) = match listener.accept().await {
                Ok((s, addr)) => (s, addr),
                Err(e) => {
                    error!("🌐 [PEER RPC] Failed to accept connection: {}", e);
                    continue;
                }
            };

            let executor = Arc::clone(&executor_client);
            let submitter = transaction_submitter.clone();
            let net_addr = network_address.clone();

            tokio::spawn(async move {
                // Read HTTP request with timeout
                let mut buffer = [0u8; 1024];
                let read_result = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    stream.read(&mut buffer),
                )
                .await;

                let n = match read_result {
                    Ok(Ok(n)) if n > 0 => n,
                    Ok(Ok(_)) => return, // Empty read
                    Ok(Err(e)) => {
                        warn!("🌐 [PEER RPC] Failed to read from {}: {}", peer_addr, e);
                        return;
                    }
                    Err(_) => {
                        warn!("🌐 [PEER RPC] Timeout reading from {}", peer_addr);
                        return;
                    }
                };

                let request = String::from_utf8_lossy(&buffer[..n]);

                // Route request
                if request.starts_with("GET /peer_info") {
                    Self::handle_peer_info(&mut stream, &executor, node_id, &net_addr).await;
                } else if request.starts_with("GET /get_epoch_boundary_data") {
                    Self::handle_get_epoch_boundary_data(&mut stream, &executor, &request).await;
                } else if request.starts_with("GET /get_blocks") {
                    Self::handle_get_blocks(&mut stream, &executor, node_id, &request).await;
                } else if request.starts_with("GET /health") {
                    Self::handle_health(&mut stream).await;
                } else if request.starts_with("POST /submit_transaction") {
                    Self::handle_submit_transaction(&mut stream, submitter.as_ref(), &request)
                        .await;
                } else {
                    // Return 404 for unknown routes
                    let response = "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\n\r\n{\"error\":\"Not Found\"}";
                    let _ = stream.write_all(response.as_bytes()).await;
                }
            });
        }
    }

    /// Handle /peer_info request
    async fn handle_peer_info(
        stream: &mut tokio::net::TcpStream,
        executor: &Arc<ExecutorClient>,
        node_id: usize,
        network_address: &str,
    ) {
        // Query epoch and block from Go Master
        let epoch = match executor.get_current_epoch().await {
            Ok(e) => e,
            Err(e) => {
                error!("🌐 [PEER RPC] Failed to get epoch: {}", e);
                let response = format!(
                    "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\n\r\n{{\"error\":\"Failed to get epoch: {}\"}}",
                    e.to_string().replace('"', "\\\"")
                );
                let _ = stream.write_all(response.as_bytes()).await;
                return;
            }
        };

        let last_block = match executor.get_last_block_number().await {
            Ok(b) => b,
            Err(e) => {
                error!("🌐 [PEER RPC] Failed to get last block: {}", e);
                let response = format!(
                    "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\n\r\n{{\"error\":\"Failed to get last block: {}\"}}",
                    e.to_string().replace('"', "\\\"")
                );
                let _ = stream.write_all(response.as_bytes()).await;
                return;
            }
        };

        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let info = PeerInfoResponse {
            node_id,
            epoch,
            last_block,
            network_address: network_address.to_string(),
            timestamp_ms,
        };

        let json = serde_json::to_string(&info).unwrap_or_else(|_| "{}".to_string());
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\n\r\n{}",
            json
        );

        if let Err(e) = stream.write_all(response.as_bytes()).await {
            error!("🌐 [PEER RPC] Failed to write response: {}", e);
        }

        info!(
            "🌐 [PEER RPC] Served /peer_info: epoch={}, last_block={}",
            epoch, last_block
        );
    }

    /// Handle /health request
    async fn handle_health(stream: &mut tokio::net::TcpStream) {
        let response =
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"status\":\"ok\"}";
        let _ = stream.write_all(response.as_bytes()).await;
    }

    /// Handle /get_epoch_boundary_data request
    /// URL format: GET /get_epoch_boundary_data?epoch=X
    /// Returns epoch boundary data for the specified epoch (validators, timestamp, boundary block)
    async fn handle_get_epoch_boundary_data(
        stream: &mut tokio::net::TcpStream,
        executor: &Arc<ExecutorClient>,
        request: &str,
    ) {
        // Parse epoch from query parameter
        let epoch = Self::parse_epoch_param(request);

        let Some(target_epoch) = epoch else {
            let response = EpochBoundaryDataResponse {
                epoch: 0,
                timestamp_ms: 0,
                boundary_block: 0,
                validators: vec![],
                error: Some("Missing or invalid 'epoch' parameter".to_string()),
            };
            let json = serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string());
            let http_response = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\n\r\n{}",
                json
            );
            let _ = stream.write_all(http_response.as_bytes()).await;
            return;
        };

        info!(
            "🌐 [PEER RPC] /get_epoch_boundary_data request: epoch={}",
            target_epoch
        );

        // Fetch from local Go Master
        match executor.get_epoch_boundary_data(target_epoch).await {
            Ok((epoch, timestamp_ms, boundary_block, validators)) => {
                // Convert ValidatorInfo to ValidatorInfoSimple for JSON transport
                let validators_simple: Vec<ValidatorInfoSimple> = validators
                    .iter()
                    .map(|v| ValidatorInfoSimple {
                        name: v.name.clone(),
                        address: v.address.clone(),
                        stake: v.stake.parse::<u64>().unwrap_or(0),
                        protocol_key: v.protocol_key.clone(),
                        network_key: v.network_key.clone(),
                        authority_key: v.authority_key.clone(),
                    })
                    .collect();

                let response = EpochBoundaryDataResponse {
                    epoch,
                    timestamp_ms,
                    boundary_block,
                    validators: validators_simple,
                    error: None,
                };

                let json = serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string());
                let http_response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\n\r\n{}",
                    json
                );

                if let Err(e) = stream.write_all(http_response.as_bytes()).await {
                    error!(
                        "🌐 [PEER RPC] Failed to write /get_epoch_boundary_data response: {}",
                        e
                    );
                }

                info!(
                    "🌐 [PEER RPC] Served /get_epoch_boundary_data: epoch={}, timestamp={}, boundary_block={}, validators={}",
                    epoch, timestamp_ms, boundary_block, validators.len()
                );
            }
            Err(e) => {
                warn!("🌐 [PEER RPC] Failed to get epoch boundary data: {}", e);
                let response = EpochBoundaryDataResponse {
                    epoch: target_epoch,
                    timestamp_ms: 0,
                    boundary_block: 0,
                    validators: vec![],
                    error: Some(format!("{}", e)),
                };

                let json = serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string());
                let http_response = format!(
                    "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\n\r\n{}",
                    json
                );

                if let Err(e) = stream.write_all(http_response.as_bytes()).await {
                    error!("🌐 [PEER RPC] Failed to write error response: {}", e);
                }
            }
        }
    }

    /// Parse epoch parameter from request query string
    fn parse_epoch_param(request: &str) -> Option<u64> {
        if let Some(query_start) = request.find('?') {
            let query_end = request[query_start..]
                .find(' ')
                .unwrap_or(request.len() - query_start);
            let query = &request[query_start + 1..query_start + query_end];

            for param in query.split('&') {
                let parts: Vec<&str> = param.split('=').collect();
                if parts.len() == 2 && parts[0] == "epoch" {
                    return parts[1].parse().ok();
                }
            }
        }
        None
    }

    /// Handle /get_blocks request
    /// URL format: GET /get_blocks?from=X&to=Y
    async fn handle_get_blocks(
        stream: &mut tokio::net::TcpStream,
        executor: &Arc<ExecutorClient>,
        node_id: usize,
        request: &str,
    ) {
        // Parse query parameters from request line
        let (from_block, to_block) = Self::parse_block_range(request);

        let (Some(from), Some(to)) = (from_block, to_block) else {
            let response = GetBlocksResponse {
                node_id,
                blocks: std::collections::HashMap::new(),
                count: 0,
                error: Some("Missing or invalid from/to parameters".to_string()),
            };
            let json = serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string());
            let http_response = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\n\r\n{}",
                json
            );
            let _ = stream.write_all(http_response.as_bytes()).await;
            return;
        };

        // Limit batch size to prevent DoS
        let max_batch = 100u64;
        let actual_to = std::cmp::min(to, from + max_batch - 1);

        info!(
            "🌐 [PEER RPC] /get_blocks request: from={}, to={} (actual_to={})",
            from, to, actual_to
        );

        // Fetch blocks from Go Master via executor_client
        match executor.get_blocks_range(from, actual_to).await {
            Ok(block_data_list) => {
                // Convert proto::BlockData to HashMap<u64, String> for response
                let mut blocks = std::collections::HashMap::new();
                for block in &block_data_list {
                    // Encode full block info as hex string for JSON transport
                    // Using extra_data which contains the serialized block
                    blocks.insert(block.block_number, hex::encode(&block.extra_data));
                }

                let count = blocks.len();
                info!("🌐 [PEER RPC] Returning {} blocks from Go Master", count);

                let response = GetBlocksResponse {
                    node_id,
                    blocks,
                    count,
                    error: None,
                };

                let json = serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string());
                let http_response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\n\r\n{}",
                    json
                );

                if let Err(e) = stream.write_all(http_response.as_bytes()).await {
                    error!("🌐 [PEER RPC] Failed to write /get_blocks response: {}", e);
                }
            }
            Err(e) => {
                warn!("🌐 [PEER RPC] Failed to fetch blocks from Go Master: {}", e);
                let response = GetBlocksResponse {
                    node_id,
                    blocks: std::collections::HashMap::new(),
                    count: 0,
                    error: Some(format!("Failed to fetch blocks: {}", e)),
                };

                let json = serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string());
                let http_response = format!(
                    "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\n\r\n{}",
                    json
                );

                if let Err(e) = stream.write_all(http_response.as_bytes()).await {
                    error!("🌐 [PEER RPC] Failed to write error response: {}", e);
                }
            }
        }
    }

    /// Parse from and to block numbers from request query string
    fn parse_block_range(request: &str) -> (Option<u64>, Option<u64>) {
        let mut from_block = None;
        let mut to_block = None;

        // Find query string in request line
        if let Some(query_start) = request.find('?') {
            let query_end = request[query_start..]
                .find(' ')
                .unwrap_or(request.len() - query_start);
            let query = &request[query_start + 1..query_start + query_end];

            for param in query.split('&') {
                let parts: Vec<&str> = param.split('=').collect();
                if parts.len() == 2 {
                    match parts[0] {
                        "from" => from_block = parts[1].parse().ok(),
                        "to" => to_block = parts[1].parse().ok(),
                        _ => {}
                    }
                }
            }
        }

        (from_block, to_block)
    }

    /// Handle /submit_transaction POST request
    /// This endpoint receives transactions from SyncOnly nodes and submits them to consensus
    async fn handle_submit_transaction(
        stream: &mut tokio::net::TcpStream,
        submitter: Option<&Arc<dyn TransactionSubmitter>>,
        request: &str,
    ) {
        // Check if submitter is available (only validators have it)
        let submitter = match submitter {
            Some(s) => s,
            None => {
                let response = SubmitTransactionResponse {
                    success: false,
                    count: 0,
                    error: Some(
                        "This node cannot accept forwarded transactions (not a validator)"
                            .to_string(),
                    ),
                };
                let json = serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string());
                let http_response = format!(
                    "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\n\r\n{}",
                    json
                );
                let _ = stream.write_all(http_response.as_bytes()).await;
                return;
            }
        };

        // Parse POST body - find content after double newline
        let body_start = request
            .find("\r\n\r\n")
            .map(|i| i + 4)
            .or_else(|| request.find("\n\n").map(|i| i + 2))
            .unwrap_or(0);

        let body = &request[body_start..];

        // Parse JSON request
        let submit_req: SubmitTransactionRequest = match serde_json::from_str(body.trim()) {
            Ok(req) => req,
            Err(e) => {
                let response = SubmitTransactionResponse {
                    success: false,
                    count: 0,
                    error: Some(format!("Invalid JSON: {}", e)),
                };
                let json = serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string());
                let http_response = format!(
                    "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\n\r\n{}",
                    json
                );
                let _ = stream.write_all(http_response.as_bytes()).await;
                return;
            }
        };

        // Decode hex to bytes
        let tx_bytes = match hex::decode(&submit_req.transactions_hex) {
            Ok(bytes) => bytes,
            Err(e) => {
                let response = SubmitTransactionResponse {
                    success: false,
                    count: 0,
                    error: Some(format!("Invalid hex: {}", e)),
                };
                let json = serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string());
                let http_response = format!(
                    "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\n\r\n{}",
                    json
                );
                let _ = stream.write_all(http_response.as_bytes()).await;
                return;
            }
        };

        info!(
            "📥 [TX FORWARD] Received {} bytes from SyncOnly node, submitting to consensus",
            tx_bytes.len()
        );

        // Submit transaction to consensus via TransactionSubmitter
        // tx_bytes contains Transactions protobuf message, need to split into individual txs
        match submitter.submit(vec![tx_bytes]).await {
            Ok((block_ref, indices, _status_rx)) => {
                info!(
                    "✅ [TX FORWARD] Successfully submitted {} tx(s) to block {:?}",
                    indices.len(),
                    block_ref
                );
                let response = SubmitTransactionResponse {
                    success: true,
                    count: indices.len(),
                    error: None,
                };
                let json = serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string());
                let http_response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\n\r\n{}",
                    json
                );
                let _ = stream.write_all(http_response.as_bytes()).await;
            }
            Err(e) => {
                error!("❌ [TX FORWARD] Failed to submit transaction: {}", e);
                let response = SubmitTransactionResponse {
                    success: false,
                    count: 0,
                    error: Some(format!("Failed to submit: {}", e)),
                };
                let json = serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string());
                let http_response = format!(
                    "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\n\r\n{}",
                    json
                );
                let _ = stream.write_all(http_response.as_bytes()).await;
            }
        }
    }
}
