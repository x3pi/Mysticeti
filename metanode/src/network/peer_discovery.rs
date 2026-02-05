// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

//! Dynamic Peer Discovery Service
//!
//! This module enables SyncOnly nodes to automatically discover validator
//! addresses from the Validator smart contract, eliminating hardcoded configs.
//!
//! ## How it works
//!
//! 1. Query `getValidatorCount()` from contract 0x...1001
//! 2. For each validator, query `validators(address)` to get `Hostname`
//! 3. Build peer_rpc_addresses from Hostname + peer_rpc_port
//! 4. Refresh every 60 seconds

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

/// Validator contract address (precompiled)
const VALIDATOR_CONTRACT: &str = "0x0000000000000000000000000000000000001001";

/// Default refresh interval (5 minutes - only refresh when needed)
const DEFAULT_REFRESH_INTERVAL_SECS: u64 = 300;

/// Default peer RPC port suffix for discovered validators
const DEFAULT_PEER_RPC_PORT: u16 = 6090;

/// JSON-RPC request structure
#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    method: String,
    params: Vec<serde_json::Value>,
    id: u64,
}

/// JSON-RPC response structure
#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    jsonrpc: String,
    result: Option<String>,
    #[allow(dead_code)]
    error: Option<serde_json::Value>,
    #[allow(dead_code)]
    id: u64,
}

/// Validator info extracted from contract
#[derive(Debug, Clone)]
pub struct ValidatorInfo {
    pub address: String,
    pub hostname: String,
    pub primary_address: String,
    pub name: String,
}

/// Dynamic Peer Discovery Service
///
/// Periodically queries the Validator smart contract to discover
/// active validator peer RPC addresses.
pub struct PeerDiscoveryService {
    /// Go RPC URL (e.g., "http://127.0.0.1:8545")
    go_rpc_url: String,

    /// Discovered peer addresses (thread-safe)
    peer_addresses: Arc<RwLock<Vec<String>>>,

    /// Refresh interval
    refresh_interval: Duration,

    /// Peer RPC port to use when building addresses
    peer_rpc_port: u16,

    /// HTTP client for RPC calls
    client: reqwest::Client,
}

impl PeerDiscoveryService {
    /// Create a new PeerDiscoveryService
    pub fn new(go_rpc_url: String, peer_rpc_port: u16) -> Self {
        Self {
            go_rpc_url,
            peer_addresses: Arc::new(RwLock::new(Vec::new())),
            refresh_interval: Duration::from_secs(DEFAULT_REFRESH_INTERVAL_SECS),
            peer_rpc_port,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    /// Create with custom refresh interval
    pub fn with_refresh_interval(mut self, interval: Duration) -> Self {
        self.refresh_interval = interval;
        self
    }

    /// Get shared reference to peer addresses
    pub fn get_addresses_handle(&self) -> Arc<RwLock<Vec<String>>> {
        self.peer_addresses.clone()
    }

    /// Get current peer addresses (snapshot)
    pub async fn get_addresses(&self) -> Vec<String> {
        self.peer_addresses.read().await.clone()
    }

    /// Start the discovery service (spawns background task)
    pub fn start(self: Arc<Self>) -> JoinHandle<()> {
        let service = self.clone();

        tokio::spawn(async move {
            info!(
                "🔍 [PEER DISCOVERY] Starting service (refresh every {}s)",
                service.refresh_interval.as_secs()
            );

            // Initial fetch
            if let Err(e) = service.refresh().await {
                warn!("🔍 [PEER DISCOVERY] Initial fetch failed: {}", e);
            }

            // Periodic refresh loop
            let mut interval = tokio::time::interval(service.refresh_interval);
            loop {
                interval.tick().await;

                match service.refresh().await {
                    Ok(count) => {
                        debug!(
                            "🔍 [PEER DISCOVERY] Refreshed: {} validators discovered",
                            count
                        );
                    }
                    Err(e) => {
                        warn!("🔍 [PEER DISCOVERY] Refresh failed: {}", e);
                    }
                }
            }
        })
    }

    /// Refresh validator list from smart contract
    async fn refresh(&self) -> Result<usize> {
        // 1. Get validator count
        let count = self.get_validator_count().await?;
        debug!("🔍 [PEER DISCOVERY] Validator count: {}", count);

        if count == 0 {
            return Ok(0);
        }

        let mut addresses = Vec::new();

        // 2. For each validator, get address and info
        for i in 0..count {
            match self.get_validator_address_by_index(i).await {
                Ok(validator_addr) => {
                    // Get validator info to extract hostname
                    match self.get_validator_info(&validator_addr).await {
                        Ok(info) => {
                            // Build peer RPC address from primary_address or hostname
                            // Format: http://{host}:{port}
                            let host = Self::extract_host(&info.primary_address);
                            if !host.is_empty() {
                                let peer_addr =
                                    format!("http://{}:{}", host, self.peer_rpc_port + i as u16);
                                addresses.push(peer_addr);
                                debug!(
                                    "🔍 [PEER DISCOVERY] Found validator {}: {} -> {}",
                                    info.name,
                                    info.primary_address,
                                    addresses.last().unwrap()
                                );
                            }
                        }
                        Err(e) => {
                            warn!(
                                "🔍 [PEER DISCOVERY] Failed to get info for {}: {}",
                                validator_addr, e
                            );
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        "🔍 [PEER DISCOVERY] Failed to get validator at index {}: {}",
                        i, e
                    );
                }
            }
        }

        // 3. Update shared state
        let discovered_count = addresses.len();
        {
            let mut guard = self.peer_addresses.write().await;
            *guard = addresses;
        }

        info!(
            "🔍 [PEER DISCOVERY] Updated peer list: {} addresses",
            discovered_count
        );

        Ok(discovered_count)
    }

    /// Extract host from address like "127.0.0.1:4000" or "/ip4/127.0.0.1/tcp/9000"
    fn extract_host(addr: &str) -> String {
        // Handle format: "127.0.0.1:4000"
        if let Some(host) = addr.split(':').next() {
            if !host.is_empty() && !host.starts_with('/') {
                return host.to_string();
            }
        }

        // Handle format: "/ip4/127.0.0.1/tcp/9000"
        if addr.starts_with("/ip4/") {
            let parts: Vec<&str> = addr.split('/').collect();
            if parts.len() >= 3 {
                return parts[2].to_string();
            }
        }

        String::new()
    }

    /// Call getValidatorCount() on contract
    async fn get_validator_count(&self) -> Result<u64> {
        // Method selector for getValidatorCount()
        let call_data = "0x7071688a";

        let result = self.eth_call(call_data).await?;

        // Parse hex result to u64
        let result = result.trim_start_matches("0x");
        let count = u64::from_str_radix(result, 16).unwrap_or(0);
        Ok(count)
    }

    /// Call validatorAddresses(uint256 index) on contract
    async fn get_validator_address_by_index(&self, index: u64) -> Result<String> {
        // Method selector for validatorAddresses(uint256):
        // keccak256("validatorAddresses(uint256)")[:4] = 0x8c7c9e0c
        let index_padded = format!("{:064x}", index);
        let call_data = format!("0x8c7c9e0c{}", index_padded);

        let result = self.eth_call(&call_data).await?;

        // Result is 32 bytes address (padded)
        // Extract last 40 chars (20 bytes) as address
        let result = result.trim_start_matches("0x");
        if result.len() >= 40 {
            let addr = &result[result.len() - 40..];
            return Ok(format!("0x{}", addr));
        }

        Err(anyhow::anyhow!("Invalid address response"))
    }

    /// Call validators(address) to get full info
    async fn get_validator_info(&self, address: &str) -> Result<ValidatorInfo> {
        // Method selector for validators(address)
        // keccak256("validators(address)")[:4] = 0xfa52c7d8
        let addr_padded = format!("{:0>64}", address.trim_start_matches("0x"));
        let call_data = format!("0xfa52c7d8{}", addr_padded);

        let result = self.eth_call(&call_data).await?;

        // Parse the complex ABI response
        // For now, we'll use a simplified approach - just extract what we need
        // The response contains multiple dynamic strings, we need offset-based parsing

        let info = self.parse_validator_response(address, &result)?;
        Ok(info)
    }

    /// Parse validator response (simplified - extracts primary_address and name)
    fn parse_validator_response(&self, address: &str, hex_result: &str) -> Result<ValidatorInfo> {
        let result = hex_result.trim_start_matches("0x");

        // The response is ABI-encoded with dynamic types
        // We'll do basic extraction - in production, use proper ABI decoder

        // For simplicity, attempt to extract readable strings
        // Primary address is typically at a known offset

        // Default fallback - build from node index pattern
        // This is a simplified approach; full ABI decoding would be more robust

        // Try to find IP pattern in the hex (e.g., "127.0.0.1")
        let decoded = Self::hex_to_ascii_safe(result);

        let primary_address = Self::find_ip_address(&decoded).unwrap_or_else(|| "127.0.0.1".into());
        let name = Self::find_node_name(&decoded).unwrap_or_else(|| "validator".into());

        Ok(ValidatorInfo {
            address: address.to_string(),
            hostname: primary_address.clone(),
            primary_address,
            name,
        })
    }

    /// Convert hex to ASCII (safe - ignores non-printable)
    fn hex_to_ascii_safe(hex: &str) -> String {
        let mut result = String::new();
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .filter_map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
            .collect();

        for b in bytes {
            if b.is_ascii_graphic() || b == b' ' {
                result.push(b as char);
            }
        }
        result
    }

    /// Find IP address pattern in string
    fn find_ip_address(s: &str) -> Option<String> {
        // Simple pattern matching for IP:port or IP
        let patterns = ["127.0.0.1", "192.168.", "10.", "172."];
        for pattern in patterns {
            if let Some(pos) = s.find(pattern) {
                // Extract until non-IP character
                let end = s[pos..]
                    .find(|c: char| !c.is_ascii_digit() && c != '.' && c != ':')
                    .unwrap_or(s.len() - pos);
                let ip_str = &s[pos..pos + end];
                // Return just the IP part (before port if present)
                let ip = ip_str.split(':').next()?;
                return Some(ip.to_string());
            }
        }
        None
    }

    /// Find node name pattern in string
    fn find_node_name(s: &str) -> Option<String> {
        // Look for "node-X" pattern
        if let Some(pos) = s.find("node-") {
            let end = pos + 6; // "node-X" is 6 chars
            if end <= s.len() {
                return Some(s[pos..end].to_string());
            }
        }
        None
    }

    /// Perform eth_call to Go RPC
    async fn eth_call(&self, data: &str) -> Result<String> {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "eth_call".to_string(),
            params: vec![
                serde_json::json!({
                    "to": VALIDATOR_CONTRACT,
                    "data": data,
                }),
                serde_json::json!("latest"),
            ],
            id: 1,
        };

        let response = self
            .client
            .post(&self.go_rpc_url)
            .json(&request)
            .send()
            .await?
            .json::<JsonRpcResponse>()
            .await?;

        if let Some(result) = response.result {
            Ok(result)
        } else if let Some(err) = response.error {
            Err(anyhow::anyhow!("RPC error: {:?}", err))
        } else {
            Err(anyhow::anyhow!("Empty RPC response"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_host_ip_port() {
        assert_eq!(
            PeerDiscoveryService::extract_host("127.0.0.1:4000"),
            "127.0.0.1"
        );
        assert_eq!(
            PeerDiscoveryService::extract_host("192.168.1.100:8545"),
            "192.168.1.100"
        );
    }

    #[test]
    fn test_extract_host_multiaddr() {
        assert_eq!(
            PeerDiscoveryService::extract_host("/ip4/127.0.0.1/tcp/9000"),
            "127.0.0.1"
        );
        assert_eq!(
            PeerDiscoveryService::extract_host("/ip4/10.0.0.5/tcp/4000"),
            "10.0.0.5"
        );
    }

    #[test]
    fn test_find_ip_address() {
        assert_eq!(
            PeerDiscoveryService::find_ip_address("Primary: 127.0.0.1:4000"),
            Some("127.0.0.1".to_string())
        );
    }
}
