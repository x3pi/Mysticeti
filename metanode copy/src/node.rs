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
use std::time::Duration;
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
        let (commit_consumer, commit_receiver, mut block_receiver) = CommitConsumerArgs::new(0, 0);
        
        // Create ordered commit processor
        let commit_processor = crate::commit_processor::CommitProcessor::new(commit_receiver);
        tokio::spawn(async move {
            if let Err(e) = commit_processor.run().await {
                tracing::error!("Commit processor error: {}", e);
            }
        });
        
        // Spawn task to consume blocks (for logging)
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

        // Apply speed multiplier to consensus parameters (to slow down system)
        let speed_multiplier = config.speed_multiplier;
        if speed_multiplier != 1.0 {
            info!("Applying speed multiplier: {}x ({}x slower)", 
                speed_multiplier, 1.0 / speed_multiplier);
            
            // Calculate delays based on multiplier
            // Default values: leader_timeout=200ms, min_round_delay=50ms
            let leader_timeout = config.leader_timeout_ms
                .map(|ms| Duration::from_millis(ms))
                .unwrap_or_else(|| Duration::from_millis((200.0 / speed_multiplier) as u64));
            
            let min_round_delay = config.min_round_delay_ms
                .map(|ms| Duration::from_millis(ms))
                .unwrap_or_else(|| Duration::from_millis((50.0 / speed_multiplier) as u64));
            
            let max_forward_time_drift = Duration::from_millis((500.0 / speed_multiplier) as u64);
            let round_prober_interval_ms = (5000.0 / speed_multiplier) as u64;
            let round_prober_request_timeout_ms = (4000.0 / speed_multiplier) as u64;
            
            parameters.leader_timeout = leader_timeout;
            parameters.min_round_delay = min_round_delay;
            parameters.max_forward_time_drift = max_forward_time_drift;
            parameters.round_prober_interval_ms = round_prober_interval_ms;
            parameters.round_prober_request_timeout_ms = round_prober_request_timeout_ms;
            
            info!("Consensus delays: leader_timeout={:?}, min_round_delay={:?}, max_forward_time_drift={:?}",
                parameters.leader_timeout,
                parameters.min_round_delay,
                parameters.max_forward_time_drift);
        }

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

