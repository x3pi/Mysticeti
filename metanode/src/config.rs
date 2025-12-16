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

        // Save committee with epoch timestamp
        let committee_path = output_dir.join("committee.json");
        let committee_json = serde_json::to_string_pretty(&committee)?;
        fs::write(&committee_path, committee_json)?;

        // Save epoch timestamp separately
        let epoch_path = output_dir.join("epoch_timestamp.txt");
        fs::write(&epoch_path, epoch_start_timestamp.to_string())?;

        // Generate individual node configs
        for (idx, (protocol_keypair, network_keypair, _authority_keypair)) in keypairs.iter().enumerate() {
            let config = NodeConfig {
                node_id: idx,
                network_address: format!("127.0.0.1:{}", 9000 + idx),
                protocol_key_path: Some(output_dir.join(format!("node_{}_protocol_key.json", idx))),
                network_key_path: Some(output_dir.join(format!("node_{}_network_key.json", idx))),
                committee_path: Some(committee_path.clone()),
                storage_path: output_dir.join(format!("storage/node_{}", idx)),
                enable_metrics: true,
                metrics_port: 9100 + idx as u16,
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
        let committee: Committee = serde_json::from_str(&content)?;
        Ok(committee)
    }

    pub fn load_epoch_timestamp(&self) -> Result<u64> {
        // Try to load from epoch_timestamp.txt in the same directory as committee
        if let Some(committee_path) = &self.committee_path {
            let epoch_path = committee_path
                .parent()
                .context("Committee path has no parent directory")?
                .join("epoch_timestamp.txt");
            
            if epoch_path.exists() {
                let content = fs::read_to_string(&epoch_path)?;
                let timestamp = content.trim().parse::<u64>()
                    .context("Failed to parse epoch timestamp")?;
                return Ok(timestamp);
            }
        }
        
        // Fallback: use current time (for backward compatibility)
        Ok(std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64)
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

