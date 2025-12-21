// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use tracing::{info, error, warn};
use fastcrypto::hash::{HashFunction, Blake2b256};
use crate::tx_submitter::TransactionSubmitter;

/// Simple HTTP RPC server for submitting transactions
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
        let addr = format!("127.0.0.1:{}", self.port);
        let listener = TcpListener::bind(&addr).await?;
        info!("RPC server started on {}", addr);

        loop {
            let (mut stream, _) = listener.accept().await?;
            let client = self.transaction_client.clone();
            let node = self.node.clone();

            tokio::spawn(async move {
                let mut buffer = [0; 8192];
                match stream.read(&mut buffer).await {
                    Ok(0) => return,
                    Ok(n) => {
                        let request = String::from_utf8_lossy(&buffer[..n]);
                        
                        // Simple HTTP POST handler
                        if request.starts_with("POST /submit") {
                            // Extract transaction data from request body
                            let body_start = request.find("\r\n\r\n")
                                .or_else(|| request.find("\n\n"))
                                .map(|i| i + 4)
                                .unwrap_or(0);
                            
                            let body = &request[body_start..];
                            let tx_data = if body.starts_with("0x") || body.chars().all(|c| c.is_ascii_hexdigit()) {
                                // Hex encoded
                                hex::decode(body.trim().trim_start_matches("0x"))
                                    .unwrap_or_else(|_| body.as_bytes().to_vec())
                            } else {
                                // Text
                                body.trim().as_bytes().to_vec()
                            };

                            // Calculate transaction hash for tracking
                            let tx_hash = Blake2b256::digest(&tx_data).to_vec();
                            let tx_hash_hex = hex::encode(&tx_hash[..8]); // Use first 8 bytes as short hash
                            
                            // Check if node is ready to accept transactions
                            if let Some(ref node) = node {
                                let node_guard = node.lock().await;
                                let (is_ready, reason) = node_guard.is_ready_for_transactions().await;
                                drop(node_guard);
                                if !is_ready {
                                    warn!("🚫 Transaction rejected: node not ready - {}", reason);
                                    let response = format!(
                                        "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\n\r\n{{\"success\":false,\"error\":\"Node not ready to accept transactions: {}\"}}",
                                        reason
                                    );
                                    let _ = stream.write_all(response.as_bytes()).await;
                                    return;
                                }
                            }
                            
                            info!("📤 Transaction submitted via RPC: hash={}, size={} bytes", tx_hash_hex, tx_data.len());

                            match client.submit(vec![tx_data]).await {
                                Ok((block_ref, indices, _)) => {
                                    info!("✅ Transaction included in block: hash={}, block={:?}, indices={:?}", 
                                        tx_hash_hex, block_ref, indices);
                                    let response = format!(
                                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{{\"success\":true,\"tx_hash\":\"{}\",\"block_ref\":\"{:?}\",\"indices\":{:?}}}",
                                        tx_hash_hex, block_ref, indices
                                    );
                                    let _ = stream.write_all(response.as_bytes()).await;
                                }
                                Err(e) => {
                                    let response = format!(
                                        "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\n\r\n{{\"success\":false,\"error\":\"{}\"}}",
                                        e
                                    );
                                    let _ = stream.write_all(response.as_bytes()).await;
                                }
                            }
                        } else {
                            // Return 404 for other requests
                            let response = "HTTP/1.1 404 Not Found\r\n\r\n";
                            let _ = stream.write_all(response.as_bytes()).await;
                        }
                    }
                    Err(e) => {
                        error!("Failed to read from stream: {}", e);
                    }
                }
            });
        }
    }
}

