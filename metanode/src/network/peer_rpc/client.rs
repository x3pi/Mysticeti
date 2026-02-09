// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

//! Peer RPC client functions — query remote peers over TCP/HTTP.
//!
//! Used for epoch discovery, fetching boundary data from peers, and
//! forwarding transactions from SyncOnly nodes to validators.

use anyhow::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{info, warn};

use super::types::*;

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

/// Query epoch boundary data from a remote peer via HTTP
/// This is used by late-joining validators to get epoch boundary data from peers
/// who have already witnessed the epoch transition
pub async fn query_peer_epoch_boundary_data(
    peer_address: &str,
    epoch: u64,
) -> Result<EpochBoundaryDataResponse> {
    use tokio::net::TcpStream;

    info!(
        "🌐 [PEER RPC] Querying epoch boundary data for epoch {} from {}",
        epoch, peer_address
    );

    // Connect with timeout
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        TcpStream::connect(peer_address),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Connection timeout to {}", peer_address))?
    .map_err(|e| anyhow::anyhow!("Failed to connect to {}: {}", peer_address, e))?;

    // Send HTTP GET request
    let request = format!(
        "GET /get_epoch_boundary_data?epoch={} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        epoch, peer_address
    );
    stream.write_all(request.as_bytes()).await?;

    // Read response with timeout
    let mut buffer = Vec::new();
    let mut temp = [0u8; 16384]; // Larger buffer for validator data
    let read_result = tokio::time::timeout(std::time::Duration::from_secs(15), async {
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
    let response: EpochBoundaryDataResponse = serde_json::from_str(body.trim()).map_err(|e| {
        anyhow::anyhow!(
            "Failed to parse epoch boundary data JSON: {} (body: {})",
            e,
            body.trim()
        )
    })?;

    // Check for error in response
    if let Some(error) = &response.error {
        return Err(anyhow::anyhow!("Peer returned error: {}", error));
    }

    info!(
        "🌐 [PEER RPC] Received epoch boundary data from {}: epoch={}, timestamp={}, boundary_block={}, validators={}",
        peer_address, response.epoch, response.timestamp_ms, response.boundary_block, response.validators.len()
    );

    Ok(response)
}

/// Forward transaction to validator nodes via HTTP POST
/// This is used by SyncOnly nodes to forward transactions to validators for consensus
/// Uses round-robin retry logic for fault tolerance
pub async fn forward_transaction_to_validators(
    peer_addresses: &[String],
    tx_data: &[u8],
) -> Result<SubmitTransactionResponse> {
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpStream;

    if peer_addresses.is_empty() {
        return Err(anyhow::anyhow!(
            "No peer addresses configured for forwarding"
        ));
    }

    let tx_hex = hex::encode(tx_data);
    let request_body = serde_json::to_string(&SubmitTransactionRequest {
        transactions_hex: tx_hex,
    })?;

    // Round-robin through peers until one succeeds
    for peer_addr in peer_addresses {
        info!(
            "🔄 [TX FORWARD] Attempting to forward transaction to validator: {}",
            peer_addr
        );

        // Connect with timeout
        let stream_result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            TcpStream::connect(peer_addr),
        )
        .await;

        let mut stream = match stream_result {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                warn!("🔄 [TX FORWARD] Failed to connect to {}: {}", peer_addr, e);
                continue;
            }
            Err(_) => {
                warn!("🔄 [TX FORWARD] Timeout connecting to {}", peer_addr);
                continue;
            }
        };

        // Build HTTP POST request
        let http_request = format!(
            "POST /submit_transaction HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            peer_addr,
            request_body.len(),
            request_body
        );

        // Send request
        if let Err(e) = stream.write_all(http_request.as_bytes()).await {
            warn!(
                "🔄 [TX FORWARD] Failed to send request to {}: {}",
                peer_addr, e
            );
            continue;
        }

        // Read response with timeout
        let mut buffer = [0u8; 4096];
        let read_result =
            tokio::time::timeout(std::time::Duration::from_secs(10), stream.read(&mut buffer))
                .await;

        let n = match read_result {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => {
                warn!(
                    "🔄 [TX FORWARD] Failed to read response from {}: {}",
                    peer_addr, e
                );
                continue;
            }
            Err(_) => {
                warn!(
                    "🔄 [TX FORWARD] Timeout reading response from {}",
                    peer_addr
                );
                continue;
            }
        };

        let response_str = String::from_utf8_lossy(&buffer[..n]);

        // Parse JSON body from HTTP response
        let body_start = response_str
            .find("\r\n\r\n")
            .map(|i| i + 4)
            .or_else(|| response_str.find("\n\n").map(|i| i + 2))
            .unwrap_or(0);

        let body = &response_str[body_start..];

        match serde_json::from_str::<SubmitTransactionResponse>(body.trim()) {
            Ok(resp) => {
                if resp.success {
                    info!(
                        "✅ [TX FORWARD] Successfully forwarded transaction to {}",
                        peer_addr
                    );
                    return Ok(resp);
                } else {
                    warn!(
                        "🔄 [TX FORWARD] Validator {} rejected transaction: {:?}",
                        peer_addr, resp.error
                    );
                    continue;
                }
            }
            Err(e) => {
                warn!(
                    "🔄 [TX FORWARD] Failed to parse response from {}: {}",
                    peer_addr, e
                );
                continue;
            }
        }
    }

    Err(anyhow::anyhow!(
        "Failed to forward transaction to any validator"
    ))
}
