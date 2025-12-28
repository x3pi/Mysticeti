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
use tracing::info;

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
    /// Enable executor client to send committed blocks to Go executor (default: false)
    /// Only node 0 should have this enabled
    #[serde(default)]
    pub executor_enabled: bool,
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
        for (idx, (protocol_keypair, network_keypair, _authority_keypair)) in keypairs.iter().enumerate() {
            // Per-node committee file (prevents nodes clobbering each other's committee.json on epoch transition).
            let node_committee_path = output_dir.join(format!("committee_node_{}.json", idx));
            fs::write(&node_committee_path, &committee_json)?;

            let config = NodeConfig {
                node_id: idx,
                network_address: format!("127.0.0.1:{}", 9000 + idx),
                protocol_key_path: Some(output_dir.join(format!("node_{}_protocol_key.json", idx))),
                network_key_path: Some(output_dir.join(format!("node_{}_network_key.json", idx))),
                committee_path: Some(node_committee_path),
                storage_path: output_dir.join(format!("storage/node_{}", idx)),
                enable_metrics: true,
                metrics_port: 9100 + idx as u16,
                speed_multiplier: 0.2, // Default: 5x slower (0.2 = 1/5 speed)
                leader_timeout_ms: None,
                min_round_delay_ms: None,
                time_based_epoch_change: true, // Enabled by default
                epoch_duration_seconds: Some(86400), // Default: 1 day (24 * 60 * 60 seconds)
                max_clock_drift_seconds: 5,
                enable_ntp_sync: false, // Disabled by default (enable for production)
                ntp_servers: default_ntp_servers(),
                ntp_sync_interval_seconds: 300,
                executor_enabled: idx == 0, // Only node 0 has executor enabled by default
                commit_sync_batch_size: default_commit_sync_batch_size(),
                commit_sync_parallel_fetches: default_commit_sync_parallel_fetches(),
                commit_sync_batches_ahead: default_commit_sync_batches_ahead(),
                adaptive_catchup_enabled: default_adaptive_catchup(),
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

    pub fn load_committee(&self) -> Result<Committee> {
        let path = self
            .committee_path
            .as_ref()
            .context("Committee path not specified")?;
        let content = fs::read_to_string(path)?;
        
        // Try to load as CommitteeConfig first (with epoch_timestamp_ms)
        // If that fails, fall back to plain Committee (backward compatibility)
        match serde_json::from_str::<CommitteeConfig>(&content) {
            Ok(config) => Ok(config.committee),
            Err(_) => {
                // Fallback: try loading as plain Committee (for backward compatibility)
        let committee: Committee = serde_json::from_str(&content)?;
        Ok(committee)
            }
        }
    }

    /// Save committee.json in the extended format (Committee + epoch_timestamp_ms + last_global_exec_index).
    /// This is used during epoch transition to atomically update epoch + timestamp + global exec index for all nodes.
    /// NOTE: This function preserves existing last_global_exec_index. For explicit control, use save_committee_with_global_exec_index.
    #[allow(dead_code)] // Kept for backward compatibility and potential future use
    pub fn save_committee_with_epoch_timestamp(
        committee_path: &Path,
        committee: &Committee,
        epoch_timestamp_ms: u64,
    ) -> Result<()> {
        // Load existing last_global_exec_index if available (preserve it)
        let existing_last_global_exec_index = Self::load_last_global_exec_index(committee_path).ok();
        
        let committee_config = CommitteeConfig {
            committee: committee.clone(),
            epoch_timestamp_ms: Some(epoch_timestamp_ms),
            last_global_exec_index: existing_last_global_exec_index,
        };
        let committee_json = serde_json::to_string_pretty(&committee_config)?;

        // Atomic write: write to temp file then rename.
        // Prevents partial/corrupt committee.json if process crashes mid-write.
        let tmp_path = committee_path.with_extension("json.tmp");
        fs::write(&tmp_path, committee_json)?;
        fs::rename(&tmp_path, committee_path)?;
        Ok(())
    }

    /// Save committee.json with epoch timestamp and last global execution index
    pub fn save_committee_with_global_exec_index(
        committee_path: &Path,
        committee: &Committee,
        epoch_timestamp_ms: u64,
        last_global_exec_index: u64,
    ) -> Result<()> {
        let committee_config = CommitteeConfig {
            committee: committee.clone(),
            epoch_timestamp_ms: Some(epoch_timestamp_ms),
            last_global_exec_index: Some(last_global_exec_index),
        };
        let committee_json = serde_json::to_string_pretty(&committee_config)?;

        // Atomic write: write to temp file then rename.
        let tmp_path = committee_path.with_extension("json.tmp");
        fs::write(&tmp_path, committee_json)?;
        fs::rename(&tmp_path, committee_path)?;
        Ok(())
    }

    /// Load last_global_exec_index from committee.json
    pub fn load_last_global_exec_index(committee_path: &Path) -> Result<u64> {
        if !committee_path.exists() {
            // If committee.json doesn't exist, start from 0
            return Ok(0);
        }
        
        let content = fs::read_to_string(committee_path)?;
        match serde_json::from_str::<CommitteeConfig>(&content) {
            Ok(config) => Ok(config.last_global_exec_index.unwrap_or(0)),
            Err(_) => {
                // Fallback: try loading as plain Committee (backward compatibility)
                Ok(0) // Default to 0 if can't parse
            }
        }
    }

    pub fn load_epoch_timestamp(&self) -> Result<u64> {
        // First, try to load from committee.json (new format)
        if let Some(committee_path) = &self.committee_path {
            if committee_path.exists() {
                let content = fs::read_to_string(committee_path)?;
                if let Ok(config) = serde_json::from_str::<CommitteeConfig>(&content) {
                    if let Some(timestamp) = config.epoch_timestamp_ms {
                        return Ok(timestamp);
                    }
                }
            }
        }
        
        // Fallback 1: Try to load from epoch_timestamp.txt (backward compatibility)
        if let Some(committee_path) = &self.committee_path {
            let epoch_path = committee_path
                .parent()
                .context("Committee path has no parent directory")?
                .join("epoch_timestamp.txt");
            
            if epoch_path.exists() {
                let content = fs::read_to_string(&epoch_path)?;
                let timestamp = content.trim().parse::<u64>()
                    .context("Failed to parse epoch timestamp")?;
                
                // Migrate to committee.json if possible
                self.migrate_epoch_timestamp_to_committee(timestamp)?;
                
                return Ok(timestamp);
            }
        }
        
        // Fallback 2: use current time rounded to nearest second
        // This ensures all nodes starting at the same second get the same timestamp
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        
        // Round down to nearest second to increase chance of same timestamp
        let timestamp_seconds = now_ms / 1000;
        let timestamp_ms = timestamp_seconds * 1000;
        
        // Save to committee.json if possible
        if let Some(committee_path) = &self.committee_path {
            if committee_path.exists() {
                self.save_epoch_timestamp_to_committee(timestamp_ms)?;
            } else {
                // If committee.json doesn't exist, save to epoch_timestamp.txt (backward compatibility)
                if let Some(parent) = committee_path.parent() {
                    let epoch_path = parent.join("epoch_timestamp.txt");
                    // Only write if file doesn't exist (to avoid race conditions)
                    if !epoch_path.exists() {
                        let _ = std::fs::write(&epoch_path, timestamp_ms.to_string());
                    }
                }
            }
        }
        
        Ok(timestamp_ms)
    }
    
    /// Migrate epoch_timestamp from epoch_timestamp.txt to committee.json
    fn migrate_epoch_timestamp_to_committee(&self, timestamp: u64) -> Result<()> {
        if let Some(committee_path) = &self.committee_path {
            if committee_path.exists() {
                let content = fs::read_to_string(committee_path)?;
                // Try to load as CommitteeConfig
                match serde_json::from_str::<CommitteeConfig>(&content) {
                    Ok(mut config) => {
                        // Already has epoch_timestamp_ms, no migration needed
                        if config.epoch_timestamp_ms.is_some() {
                            return Ok(());
                        }
                        // Add epoch_timestamp_ms
                        config.epoch_timestamp_ms = Some(timestamp);
                        let updated_json = serde_json::to_string_pretty(&config)?;
                        fs::write(committee_path, updated_json)?;
                        info!("✅ Migrated epoch_timestamp to committee.json");
                    }
                    Err(_) => {
                        // Load as plain Committee and add epoch_timestamp_ms
                        let committee: Committee = serde_json::from_str(&content)?;
                        let config = CommitteeConfig {
                            committee,
                            epoch_timestamp_ms: Some(timestamp),
                            last_global_exec_index: None, // Preserve existing if any
                        };
                        let updated_json = serde_json::to_string_pretty(&config)?;
                        fs::write(committee_path, updated_json)?;
                        info!("✅ Migrated epoch_timestamp to committee.json");
                    }
                }
            }
        }
        Ok(())
    }
    
    /// Save epoch_timestamp to committee.json
    fn save_epoch_timestamp_to_committee(&self, timestamp: u64) -> Result<()> {
        if let Some(committee_path) = &self.committee_path {
            if committee_path.exists() {
                let content = fs::read_to_string(committee_path)?;
                // Try to load as CommitteeConfig
                match serde_json::from_str::<CommitteeConfig>(&content) {
                    Ok(mut config) => {
                        // Update epoch_timestamp_ms
                        config.epoch_timestamp_ms = Some(timestamp);
                        let updated_json = serde_json::to_string_pretty(&config)?;
                        fs::write(committee_path, updated_json)?;
                    }
                    Err(_) => {
                        // Load as plain Committee and add epoch_timestamp_ms
                        let committee: Committee = serde_json::from_str(&content)?;
                        let config = CommitteeConfig {
                            committee,
                            epoch_timestamp_ms: Some(timestamp),
                            last_global_exec_index: None, // Preserve existing if any
                        };
                        let updated_json = serde_json::to_string_pretty(&config)?;
                        fs::write(committee_path, updated_json)?;
                    }
                }
            }
        }
        Ok(())
    }

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

