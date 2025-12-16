// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use consensus_config::AuthorityIndex;
use consensus_core::{
    ConsensusAuthority, NetworkType, Clock, TransactionClient,
    CommitConsumerArgs,
};
use crate::transaction::NoopTransactionVerifier;
use prometheus::Registry;
use std::sync::Arc;
use sui_protocol_config::ProtocolConfig;
use tracing::info;

use crate::config::NodeConfig;

pub struct ConsensusNode {
    authority: ConsensusAuthority,
    #[allow(dead_code)] // Reserved for future use
    transaction_client: Arc<TransactionClient>,
}

impl ConsensusNode {
    pub async fn new(config: NodeConfig) -> Result<Self> {
        info!("Initializing consensus node {}...", config.node_id);

        // Load committee
        let committee = config.load_committee()?;
        info!("Loaded committee with {} authorities", committee.size());

        // Load keypairs
        let protocol_keypair = config.load_protocol_keypair()?;
        let network_keypair = config.load_network_keypair()?;

        // Get own authority index
        let own_index = AuthorityIndex::new_for_test(config.node_id as u32);
        if !committee.is_valid_index(own_index) {
            anyhow::bail!("Node ID {} is out of range for committee size {}", config.node_id, committee.size());
        }

        // Create storage directory
        std::fs::create_dir_all(&config.storage_path)?;

        // Initialize metrics registry
        let registry = Registry::new();

        // Create clock
        let clock = Arc::new(Clock::default());

        // Create transaction verifier (no-op for now)
        let transaction_verifier = Arc::new(NoopTransactionVerifier);

        // Create commit consumer args
        let (commit_consumer, mut commit_receiver, mut block_receiver) = CommitConsumerArgs::new(0, 0);
        
        // Spawn tasks to consume commits and blocks to prevent channel overflow
        tokio::spawn(async move {
            use tracing::info;
            while let Some(subdag) = commit_receiver.recv().await {
                info!(
                    "Received commit: index={}, leader={:?}, blocks={}",
                    subdag.commit_ref.index,
                    subdag.leader,
                    subdag.blocks.len()
                );
            }
        });
        
        tokio::spawn(async move {
            use tracing::debug;
            while let Some(output) = block_receiver.recv().await {
                debug!("Received {} certified blocks", output.blocks.len());
            }
        });

        // Get protocol config
        let protocol_config = ProtocolConfig::get_for_max_version_UNSAFE();

        // Create parameters with storage path
        let mut parameters = consensus_config::Parameters::default();
        parameters.db_path = config.storage_path.join("consensus_db");
        std::fs::create_dir_all(&parameters.db_path)?;

        // Load epoch start timestamp (must be same for all nodes)
        let epoch_start_timestamp = config.load_epoch_timestamp()?;
        info!("Using epoch start timestamp: {}", epoch_start_timestamp);

        // Start authority node
        info!("Starting consensus authority node...");
        let authority = ConsensusAuthority::start(
            NetworkType::Tonic,
            epoch_start_timestamp,
            own_index,
            committee,
            parameters,
            protocol_config,
            protocol_keypair,
            network_keypair,
            clock,
            transaction_verifier,
            commit_consumer,
            registry,
            0, // boot_counter
        )
        .await;

        let transaction_client = authority.transaction_client();

        info!("Consensus node {} initialized successfully", config.node_id);

        Ok(Self {
            authority,
            transaction_client,
        })
    }

    #[allow(dead_code)] // Reserved for future use
    pub fn transaction_client(&self) -> Arc<TransactionClient> {
        self.transaction_client.clone()
    }

    pub async fn shutdown(self) -> Result<()> {
        info!("Shutting down consensus node...");
        self.authority.stop().await;
        info!("Consensus node stopped");
        Ok(())
    }
}

