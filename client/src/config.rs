// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context, Result};
use consensus_config::{Committee, NetworkKeyPair, ProtocolKeyPair};
use fastcrypto::ed25519::Ed25519PrivateKey;
use fastcrypto::traits::ToFromBytes;
use serde::{Deserialize, Serialize};
use std::{fs, path::{Path, PathBuf}};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConfig {
    /// Node identifier (0-based index)
    pub node_id: usize,
    /// Network address for this node
    pub network_address: String,
    /// Protocol keypair file path
    pub protocol_key_path: Option<PathBuf>,
    /// Network keypair file path
    pub network_key_path: Option<PathBuf>,
    /// Committee configuration file path
    pub committee_path: Option<PathBuf>,
    /// Storage directory
    pub storage_path: PathBuf,
    /// Enable metrics
    pub enable_metrics: bool,
    /// Metrics port
    pub metrics_port: u16,
}

impl NodeConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {:?}", path))?;
        let config: NodeConfig = toml::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {:?}", path))?;
        Ok(config)
    }

    pub fn load_committee(&self) -> Result<Committee> {
        let path = self.committee_path.as_ref()
            .ok_or_else(|| anyhow::anyhow!("Committee path not set"))?;
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read committee file: {:?}", path))?;
        let committee: Committee = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse committee file: {:?}", path))?;
        Ok(committee)
    }

    pub fn load_protocol_keypair(&self) -> Result<ProtocolKeyPair> {
        let path = self.protocol_key_path.as_ref()
            .ok_or_else(|| anyhow::anyhow!("Protocol key path not set"))?;
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read protocol key file: {:?}", path))?;
        
        use base64::{Engine as _, engine::general_purpose};
        let bytes = general_purpose::STANDARD.decode(content.trim())
            .with_context(|| "Failed to decode base64 key")?;
        
        if bytes.len() != 64 {
            anyhow::bail!("Invalid key length: expected 64 bytes, got {}", bytes.len());
        }
        
        let private_bytes: [u8; 32] = bytes[0..32].try_into().unwrap();
        let _public_bytes: [u8; 32] = bytes[32..64].try_into().unwrap();
        
        let private_key = Ed25519PrivateKey::from_bytes(&private_bytes)
            .with_context(|| "Failed to create private key from bytes")?;
        let keypair = fastcrypto::ed25519::Ed25519KeyPair::from(private_key);
        Ok(ProtocolKeyPair::new(keypair))
    }

    pub fn load_network_keypair(&self) -> Result<NetworkKeyPair> {
        let path = self.network_key_path.as_ref()
            .ok_or_else(|| anyhow::anyhow!("Network key path not set"))?;
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read network key file: {:?}", path))?;
        
        use base64::{Engine as _, engine::general_purpose};
        let bytes = general_purpose::STANDARD.decode(content.trim())
            .with_context(|| "Failed to decode base64 key")?;
        
        if bytes.len() != 64 {
            anyhow::bail!("Invalid key length: expected 64 bytes, got {}", bytes.len());
        }
        
        let private_bytes: [u8; 32] = bytes[0..32].try_into().unwrap();
        let _public_bytes: [u8; 32] = bytes[32..64].try_into().unwrap();
        
        let private_key = Ed25519PrivateKey::from_bytes(&private_bytes)
            .with_context(|| "Failed to create private key from bytes")?;
        let keypair = fastcrypto::ed25519::Ed25519KeyPair::from(private_key);
        Ok(NetworkKeyPair::new(keypair))
    }

    pub fn load_epoch_timestamp(&self) -> Result<u64> {
        let config_dir = self.committee_path.as_ref()
            .and_then(|p| p.parent())
            .ok_or_else(|| anyhow::anyhow!("Cannot determine config directory"))?;
        
        let epoch_path = config_dir.join("epoch_timestamp.txt");
        if !epoch_path.exists() {
            // Fallback: try to find in current directory or use default
            return Ok(std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64);
        }
        
        let content = fs::read_to_string(&epoch_path)
            .with_context(|| format!("Failed to read epoch timestamp file: {:?}", epoch_path))?;
        
        content.trim().parse::<u64>()
            .with_context(|| format!("Failed to parse epoch timestamp: {}", content))
    }
}

