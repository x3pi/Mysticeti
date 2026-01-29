// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

//! Peer RPC Server for WAN-based block synchronization
//!
//! This module provides HTTP endpoints for peer nodes to query epoch/block info
//! over network (WAN), enabling nodes on different servers to synchronize.
//!
//! ## Endpoints
//!
//! - `GET /peer_info` - Returns current node's epoch and block info
//! - `GET /health` - Health check endpoint
//!
//! ## Example Response
//!
//! ```json
//! {
//!   "node_id": 4,
//!   "epoch": 1,
//!   "last_block": 4500,
//!   "network_address": "127.0.0.1:9004"
//! }
//! ```

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{error, info, warn};

use crate::node::executor_client::ExecutorClient;

/// Response for /peer_info endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfoResponse {
    /// Node identifier
    pub node_id: usize,
    /// Current epoch number
    pub epoch: u64,
    /// Last executed block number
    pub last_block: u64,
    /// Network address of this node
    pub network_address: String,
    /// Timestamp of response (Unix ms)
    pub timestamp_ms: u64,
}

/// Request for /get_blocks endpoint
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetBlocksRequest {
    /// Start block number (inclusive)
    pub from_block: u64,
    /// End block number (inclusive)  
    pub to_block: u64,
}

/// Response for /get_blocks endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetBlocksResponse {
    /// Node ID
    pub node_id: usize,
    /// Blocks data (block_number -> hex-encoded block data)
    pub blocks: std::collections::HashMap<u64, String>,
    /// Number of blocks returned
    pub count: usize,
    /// Error message if any
    pub error: Option<String>,
}

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
                } else if request.starts_with("GET /get_blocks") {
                    Self::handle_get_blocks(&mut stream, &executor, node_id, &request).await;
                } else if request.starts_with("GET /health") {
                    Self::handle_health(&mut stream).await;
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

    /// Handle /get_blocks request
    /// URL format: GET /get_blocks?from=X&to=Y
    async fn handle_get_blocks(
        stream: &mut tokio::net::TcpStream,
        _executor: &Arc<ExecutorClient>,
        node_id: usize,
        request: &str,
    ) {
        // Parse query parameters from request line
        let (from_block, to_block) = Self::parse_block_range(request);

        if from_block.is_none() || to_block.is_none() {
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
        }

        let from = from_block.unwrap();
        let to = to_block.unwrap();

        // Limit batch size to prevent DoS
        let max_batch = 100u64;
        let actual_to = std::cmp::min(to, from + max_batch - 1);

        info!(
            "🌐 [PEER RPC] /get_blocks request: from={}, to={} (actual_to={})",
            from, to, actual_to
        );

        // For now, return empty blocks - actual block fetching requires Go integration
        // TODO: Implement block fetching from local storage or Go Master
        let blocks = std::collections::HashMap::<u64, String>::new();

        let response = GetBlocksResponse {
            node_id,
            blocks,
            count: 0,
            error: Some("Block fetching not yet implemented - use DAG sync".to_string()),
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
}

/// Query peer info from a remote node via HTTP
pub async fn query_peer_info(peer_address: &str) -> Result<PeerInfoResponse> {
    use tokio::net::TcpStream;

    // Connect with timeout
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        TcpStream::connect(peer_address),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Connection timeout to {}", peer_address))?
    .map_err(|e| anyhow::anyhow!("Failed to connect to {}: {}", peer_address, e))?;

    // Send HTTP GET request
    let request = format!(
        "GET /peer_info HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        peer_address
    );
    stream.write_all(request.as_bytes()).await?;

    // Read response with timeout
    let mut buffer = Vec::new();
    let mut temp = [0u8; 4096];
    let read_result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match stream.read(&mut temp).await {
                Ok(0) => break,
                Ok(n) => buffer.extend_from_slice(&temp[..n]),
                Err(e) => return Err(e),
            }
        }
        Ok(())
    })
    .await;

    match read_result {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => return Err(anyhow::anyhow!("Failed to read response: {}", e)),
        Err(_) => {
            return Err(anyhow::anyhow!(
                "Timeout reading response from {}",
                peer_address
            ))
        }
    }

    // Parse HTTP response
    let response_str = String::from_utf8_lossy(&buffer);

    // Find JSON body (after empty line)
    let body_start = response_str
        .find("\r\n\r\n")
        .map(|i| i + 4)
        .or_else(|| response_str.find("\n\n").map(|i| i + 2))
        .unwrap_or(0);

    let body = &response_str[body_start..];

    // Parse JSON
    let info: PeerInfoResponse = serde_json::from_str(body.trim()).map_err(|e| {
        anyhow::anyhow!(
            "Failed to parse peer info JSON: {} (body: {})",
            e,
            body.trim()
        )
    })?;

    Ok(info)
}

/// Query multiple peers and return the best one (highest epoch/block)
pub async fn query_peer_epochs_network(peer_addresses: &[String]) -> Result<(u64, u64, String)> {
    info!(
        "🌐 [PEER RPC] Querying {} peer(s) over network for epoch discovery...",
        peer_addresses.len()
    );

    let mut best_epoch = 0u64;
    let mut best_block = 0u64;
    let mut best_address = String::new();

    for peer_addr in peer_addresses {
        match query_peer_info(peer_addr).await {
            Ok(info) => {
                info!(
                    "🌐 [PEER RPC] Peer ({}): epoch={}, block={}",
                    peer_addr, info.epoch, info.last_block
                );

                // Use this peer if it has higher epoch or higher block
                if best_address.is_empty()
                    || info.epoch > best_epoch
                    || (info.epoch == best_epoch && info.last_block > best_block)
                {
                    best_epoch = info.epoch;
                    best_block = info.last_block;
                    best_address = peer_addr.clone();
                    info!(
                        "🌐 [PEER RPC] New best peer: epoch={} block={} from {}",
                        best_epoch, best_block, peer_addr
                    );
                }
            }
            Err(e) => {
                warn!("🌐 [PEER RPC] Failed to query peer ({}): {}", peer_addr, e);
            }
        }
    }

    if best_address.is_empty() {
        return Err(anyhow::anyhow!("No reachable peers found"));
    }

    info!(
        "🌐 [PEER RPC] Best peer found: epoch={} block={} from {}",
        best_epoch, best_block, best_address
    );

    Ok((best_epoch, best_block, best_address))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_peer_info_response_serialization() {
        let info = PeerInfoResponse {
            node_id: 4,
            epoch: 1,
            last_block: 4500,
            network_address: "127.0.0.1:9004".to_string(),
            timestamp_ms: 1234567890000,
        };

        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"node_id\":4"));
        assert!(json.contains("\"epoch\":1"));
        assert!(json.contains("\"last_block\":4500"));
    }
}
