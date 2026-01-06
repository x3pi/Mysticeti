// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context, Result};
use consensus_config::{
    Authority, AuthorityKeyPair, Committee, NetworkKeyPair,
    ProtocolKeyPair,
};
use fastcrypto::traits::ToFromBytes;
use mysten_network::Multiaddr;
use serde::{Deserialize, Serialize};
use std::{fs, path::{Path, PathBuf}};

/// Extended committee configuration with epoch timestamp and global execution index
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CommitteeConfig {
    #[serde(flatten)]
    committee: Committee,
    /// Epoch start timestamp in milliseconds (for genesis blocks)
    #[serde(skip_serializing_if = "Option::is_none")]
    epoch_timestamp_ms: Option<u64>,
    /// Last global execution index (checkpoint sequence number) from previous epoch
    /// This ensures deterministic global_exec_index calculation across all nodes
    #[serde(skip_serializing_if = "Option::is_none")]
    last_global_exec_index: Option<u64>,
}

fn default_speed_multiplier() -> f64 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConfig {
    /// Node identifier (0-based index)
    pub node_id: usize,
    /// Network address for this node
    pub network_address: String,
    /// Protocol keypair file path (or generate new)
    pub protocol_key_path: Option<PathBuf>,
    /// Network keypair file path (or generate new)
    pub network_key_path: Option<PathBuf>,
    /// Committee configuration file path
    pub committee_path: Option<PathBuf>,
    /// Storage directory
    pub storage_path: PathBuf,
    /// Enable metrics
    pub enable_metrics: bool,
    /// Metrics port
    pub metrics_port: u16,
    /// Speed multiplier (1.0 = normal speed, 0.05 = 20x slower)
    #[serde(default = "default_speed_multiplier")]
    pub speed_multiplier: f64,
    /// Leader timeout in milliseconds (overrides speed_multiplier if set)
    #[serde(default)]
    pub leader_timeout_ms: Option<u64>,
    /// Minimum round delay in milliseconds (overrides speed_multiplier if set)
    #[serde(default)]
    pub min_round_delay_ms: Option<u64>,
    /// Time-based epoch change configuration
    #[serde(default)]
    pub time_based_epoch_change: bool,
    /// Epoch duration in seconds (None = disabled, Some(86400) = 24h)
    #[serde(default)]
    pub epoch_duration_seconds: Option<u64>,
    /// Max allowed clock drift in seconds (default: 5)
    #[serde(default = "default_max_clock_drift_seconds")]
    pub max_clock_drift_seconds: u64,
    /// Clock synchronization configuration
    #[serde(default)]
    pub enable_ntp_sync: bool,
    /// NTP servers to use for clock sync
    #[serde(default = "default_ntp_servers")]
    pub ntp_servers: Vec<String>,
    /// NTP sync interval in seconds (default: 300 = 5 minutes)
    #[serde(default = "default_ntp_sync_interval_seconds")]
    pub ntp_sync_interval_seconds: u64,
    /// Enable executor client for reading committee state from Go executor (default: false)
    /// All nodes can read committee and epoch state for proper operation
    #[serde(default)]
    pub executor_read_enabled: bool,
    /// Enable executor client to send committed blocks to Go executor (default: false)
    /// Only designated nodes should have this enabled for committing transactions
    /// This allows flexible testing with different commit node configurations
    #[serde(default)]
    pub executor_commit_enabled: bool,
    /// Socket path for sending blocks from Rust to Go executor (default: /tmp/executor{N}.sock)
    pub executor_send_socket_path: String,
    /// Socket path for receiving responses from Go executor (default: /tmp/rust-go.sock_{N+1})
    pub executor_receive_socket_path: String,
    /// Commit sync batch size for catch-up (default: 200, higher = faster catch-up but more memory)
    /// When node is lagging, larger batch size allows fetching more commits in parallel
    #[serde(default = "default_commit_sync_batch_size")]
    pub commit_sync_batch_size: u32,
    /// Commit sync parallel fetches (default: 16, higher = faster catch-up but more network load)
    /// Number of commit batches to fetch in parallel from different peers
    #[serde(default = "default_commit_sync_parallel_fetches")]
    pub commit_sync_parallel_fetches: usize,
    /// Commit sync batches ahead (default: 64, higher = more aggressive catch-up)
    /// Maximum number of commit batches to fetch ahead before throttling
    #[serde(default = "default_commit_sync_batches_ahead")]
    pub commit_sync_batches_ahead: usize,
    /// Enable adaptive catch-up: automatically increase batch size and parallel fetches when lagging (default: true)
    #[serde(default = "default_adaptive_catchup")]
    pub adaptive_catchup_enabled: bool,
    /// Enable adaptive delay: automatically adjust delay based on network average speed (default: true)
    /// When enabled, if node is faster than network average, it will automatically add delay to sync with network
    #[serde(default = "default_adaptive_delay")]
    pub adaptive_delay_enabled: bool,
    /// Base delay in milliseconds for adaptive delay calculation (default: 50ms)
    /// This is used as base when calculating adaptive delay when node is ahead of network
    #[serde(default = "default_adaptive_delay_ms")]
    pub adaptive_delay_ms: u64,
    /// Enable LVM snapshot creation after epoch transition (default: false)
    /// Only nodes with this enabled will create snapshots
    #[serde(default)]
    pub enable_lvm_snapshot: bool,
    /// Path to lvm-snap-rsync binary (required if enable_lvm_snapshot = true)
    #[serde(default)]
    pub lvm_snapshot_bin_path: Option<PathBuf>,
    /// Delay in seconds before creating snapshot after epoch transition (default: 120 = 2 minutes)
    /// This delay allows Go executor to finish processing and stabilize before snapshot
    #[serde(default = "default_lvm_snapshot_delay_seconds")]
    pub lvm_snapshot_delay_seconds: u64,
}

fn default_max_clock_drift_seconds() -> u64 {
    5
}

fn default_ntp_servers() -> Vec<String> {
    vec![
        "pool.ntp.org".to_string(),
        "time.google.com".to_string(),
    ]
}

fn default_ntp_sync_interval_seconds() -> u64 {
    300 // 5 minutes
}

fn default_commit_sync_batch_size() -> u32 {
    200 // Increased from default 100 for faster catch-up
}

fn default_commit_sync_parallel_fetches() -> usize {
    16 // Increased from default 8 for faster catch-up
}

fn default_commit_sync_batches_ahead() -> usize {
    64 // Increased from default 32 for more aggressive catch-up
}

fn default_adaptive_catchup() -> bool {
    true // Enable adaptive catch-up by default
}

fn default_adaptive_delay() -> bool {
    true // Enable adaptive delay by default
}

fn default_adaptive_delay_ms() -> u64 {
    50 // Default base delay: 50ms
}

fn default_lvm_snapshot_delay_seconds() -> u64 {
    5 // Default delay: 5 seconds
}

impl NodeConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {:?}", path))?;
        let config: NodeConfig = toml::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {:?}", path))?;
        Ok(config)
    }

    pub async fn generate_multiple(count: usize, output_dir: &Path) -> Result<()> {
        // Create output directory
        fs::create_dir_all(output_dir)?;

        // Generate committee
        let (committee, keypairs) = Self::generate_committee(count)?;

        // Generate epoch start timestamp (same for all nodes)
        let epoch_start_timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        // Save a shared committee with epoch timestamp (template / convenience).
        // IMPORTANT: for multi-node on a single machine, each node SHOULD use its own committee file
        // to avoid concurrent writes during in-process epoch transitions.
        let shared_committee_path = output_dir.join("committee.json");
        let committee_config = CommitteeConfig {
            committee: committee.clone(),
            epoch_timestamp_ms: Some(epoch_start_timestamp),
            last_global_exec_index: Some(0), // Start from 0 for new network
        };
        let committee_json = serde_json::to_string_pretty(&committee_config)?;
        fs::write(&shared_committee_path, &committee_json)?;

        // Generate individual node configs
        // NOTE: Không tạo file committee_node_*.json nữa. Tất cả nodes sẽ fetch committee từ Go state.
        for (idx, (protocol_keypair, network_keypair, _authority_keypair)) in keypairs.iter().enumerate() {
            let config = NodeConfig {
                node_id: idx,
                network_address: format!("127.0.0.1:{}", 9000 + idx),
                protocol_key_path: Some(output_dir.join(format!("node_{}_protocol_key.json", idx))),
                network_key_path: Some(output_dir.join(format!("node_{}_network_key.json", idx))),
                committee_path: None, // Không dùng file committee nữa - fetch từ Go state
                storage_path: output_dir.join(format!("storage/node_{}", idx)),
                enable_metrics: true,
                metrics_port: 9100 + idx as u16,
                speed_multiplier: 0.2, // Default: 5x slower (0.2 = 1/5 speed)
                leader_timeout_ms: None,
                min_round_delay_ms: None,
                time_based_epoch_change: true, // Enabled by default
                epoch_duration_seconds: Some(180), // Default: 3 minutes (3 * 60 seconds)
                max_clock_drift_seconds: 5,
                enable_ntp_sync: false, // Disabled by default (enable for production)
                ntp_servers: default_ntp_servers(),
                ntp_sync_interval_seconds: 300,
                executor_read_enabled: true, // All nodes can read committee state from Go
                executor_commit_enabled: idx == 0, // Only node 0 can commit blocks by default
                executor_send_socket_path: format!("/tmp/executor{}.sock", idx), // Rust -> Go
                executor_receive_socket_path: "/tmp/rust-go.sock_1".to_string(), // Go -> Rust (all nodes read from same socket)
                commit_sync_batch_size: default_commit_sync_batch_size(),
                commit_sync_parallel_fetches: default_commit_sync_parallel_fetches(),
                commit_sync_batches_ahead: default_commit_sync_batches_ahead(),
                adaptive_catchup_enabled: default_adaptive_catchup(),
                adaptive_delay_enabled: default_adaptive_delay(),
                adaptive_delay_ms: default_adaptive_delay_ms(),
                enable_lvm_snapshot: false, // Disabled by default
                lvm_snapshot_bin_path: None,
                lvm_snapshot_delay_seconds: default_lvm_snapshot_delay_seconds(),
            };

            // Save keys - use private_key_bytes and public key bytes
            if let Some(key_path) = &config.protocol_key_path {
                let private_bytes = protocol_keypair.clone().private_key_bytes();
                let public_key = protocol_keypair.public();
                let public_bytes = public_key.to_bytes();
                let mut combined = Vec::new();
                combined.extend_from_slice(&private_bytes);
                combined.extend_from_slice(public_bytes);
                use base64::{Engine as _, engine::general_purpose};
                let key_str = general_purpose::STANDARD.encode(&combined);
                fs::write(key_path, key_str)?;
            }
            if let Some(key_path) = &config.network_key_path {
                let private_bytes = network_keypair.clone().private_key_bytes();
                let public_key = network_keypair.public();
                let public_bytes = public_key.to_bytes();
                let mut combined = Vec::new();
                combined.extend_from_slice(&private_bytes);
                combined.extend_from_slice(&public_bytes);
                use base64::{Engine as _, engine::general_purpose};
                let key_str = general_purpose::STANDARD.encode(&combined);
                fs::write(key_path, key_str)?;
            }

            // Save config
            let config_path = output_dir.join(format!("node_{}.toml", idx));
            let config_toml = toml::to_string_pretty(&config)?;
            fs::write(config_path, config_toml)?;
        }

        Ok(())
    }

    fn generate_committee(
        size: usize,
    ) -> Result<(Committee, Vec<(ProtocolKeyPair, NetworkKeyPair, AuthorityKeyPair)>)> {
        let mut authorities = Vec::new();
        let mut keypairs = Vec::new();

        for i in 0..size {
            let protocol_keypair = ProtocolKeyPair::generate(&mut rand::thread_rng());
            let network_keypair = NetworkKeyPair::generate(&mut rand::thread_rng());
            let authority_keypair = AuthorityKeyPair::generate(&mut rand::thread_rng());

            let address: Multiaddr = format!("/ip4/127.0.0.1/tcp/{}", 9000 + i).parse()
                .context("Failed to parse address")?;

            let authority = Authority {
                stake: 1,
                address: address.clone(),
                hostname: format!("node-{}", i),
                authority_key: authority_keypair.public(),
                protocol_key: protocol_keypair.public(),
                network_key: network_keypair.public(),
            };

            authorities.push(authority);
            keypairs.push((protocol_keypair, network_keypair, authority_keypair));
        }

        let committee = Committee::new(0, authorities);
        Ok((committee, keypairs))
    }

    // REMOVED: All committee file operations - Committee is now ALWAYS fetched from Go state via Unix Domain Socket
    // This ensures all nodes have identical committee data and eliminates file-based inconsistencies
    // No more save/load committee from files - everything goes through Go state synchronization

    
    

    pub fn load_protocol_keypair(&self) -> Result<ProtocolKeyPair> {
        if let Some(path) = &self.protocol_key_path {
            let content = fs::read_to_string(path)?;
            use base64::{Engine as _, engine::general_purpose};
            let bytes = general_purpose::STANDARD.decode(content.trim())
                .context("Failed to decode base64 key")?;
            if bytes.len() != 64 {
                anyhow::bail!("Invalid key length: expected 64 bytes, got {}", bytes.len());
            }
            let private_bytes: [u8; 32] = bytes[0..32].try_into().unwrap();
            let _public_bytes: [u8; 32] = bytes[32..64].try_into().unwrap();
            use fastcrypto::ed25519::Ed25519PrivateKey;
            let private_key = Ed25519PrivateKey::from_bytes(&private_bytes)
                .context("Failed to create private key from bytes")?;
            let keypair = fastcrypto::ed25519::Ed25519KeyPair::from(private_key);
            Ok(ProtocolKeyPair::new(keypair))
        } else {
            // Generate new keypair
            Ok(ProtocolKeyPair::generate(&mut rand::thread_rng()))
        }
    }

    pub fn load_network_keypair(&self) -> Result<NetworkKeyPair> {
        if let Some(path) = &self.network_key_path {
            let content = fs::read_to_string(path)?;
            use base64::{Engine as _, engine::general_purpose};
            let bytes = general_purpose::STANDARD.decode(content.trim())
                .context("Failed to decode base64 key")?;
            if bytes.len() != 64 {
                anyhow::bail!("Invalid key length: expected 64 bytes, got {}", bytes.len());
            }
            let private_bytes: [u8; 32] = bytes[0..32].try_into().unwrap();
            let _public_bytes: [u8; 32] = bytes[32..64].try_into().unwrap();
            use fastcrypto::ed25519::Ed25519PrivateKey;
            let private_key = Ed25519PrivateKey::from_bytes(&private_bytes)
                .context("Failed to create private key from bytes")?;
            let keypair = fastcrypto::ed25519::Ed25519KeyPair::from(private_key);
            Ok(NetworkKeyPair::new(keypair))
        } else {
            // Generate new keypair
            Ok(NetworkKeyPair::generate(&mut rand::thread_rng()))
        }
    }
}

