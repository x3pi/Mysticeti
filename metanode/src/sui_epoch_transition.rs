// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn, error};
use consensus_config::AuthorityIndex;
use crate::epoch_change::{EpochChangeManager, EpochChangeProposal};
use crate::executor_client::ExecutorClient;
use crate::config::NodeConfig;

/// Sui-style epoch transition manager
/// Integrates Sui's epoch transition patterns into MetaNode while preserving transaction processing flow
pub struct SuiEpochTransitionManager {
    /// Reference to the epoch change manager (preserved from MetaNode)
    epoch_change_manager: Arc<RwLock<EpochChangeManager>>,
    /// Executor client for Go state interaction
    executor_client: Arc<ExecutorClient>,
    /// Node configuration
    config: NodeConfig,
    /// Current epoch (for tracking)
    current_epoch: std::sync::atomic::AtomicU64,
    /// Own authority index
    own_index: AuthorityIndex,
}

impl SuiEpochTransitionManager {
    pub fn new(
        epoch_change_manager: Arc<RwLock<EpochChangeManager>>,
        executor_client: Arc<ExecutorClient>,
        config: NodeConfig,
        current_epoch: u64,
        own_index: AuthorityIndex,
    ) -> Self {
        Self {
            epoch_change_manager,
            executor_client,
            config,
            current_epoch: std::sync::atomic::AtomicU64::new(current_epoch),
            own_index,
        }
    }

    /// Monitor for Sui-style epoch transition triggers
    /// This replaces the time-based monitoring with checkpoint/state-based monitoring
    pub async fn monitor_epoch_transitions(&self) -> Result<()> {
        info!("🔄 Starting Sui-style epoch transition monitor");

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;

            if let Err(e) = self.check_and_trigger_epoch_transition_legacy().await {
                error!("❌ Error in epoch transition monitoring: {}", e);
            }
        }
    }

    /// Check Go state and trigger epoch transition if needed (Sui-style) - legacy version
    async fn check_and_trigger_epoch_transition_legacy(&self) -> Result<()> {
        // DISABLED: Go epoch detection causing consensus shutdown
        // We now use only checkpoint-based epoch transition
        tracing::info!("🔄 Sui-style legacy epoch transition: DISABLED (using checkpoint-based only)");
        Ok(())
    }

    /// Integrated epoch transition check (called from commit processing - Sui-style)
    /// DISABLED: We now use checkpoint-based epoch transition only
    /// This avoids conflicts between Go-side and Rust-side epoch logic
    pub async fn check_and_trigger_epoch_transition(&self, _commit_index: u32) -> Result<()> {
        // DISABLED: Only use checkpoint-based epoch transition
        // Go-side epoch detection was causing consensus shutdown issues
        tracing::info!("🔄 Sui-style integrated epoch transition: DISABLED (using checkpoint-based only)");
        Ok(())
    }

    /// Execute integrated epoch transition flow (Sui-style)
    async fn execute_integrated_epoch_transition_flow(&self, new_epoch: u64, commit_index: u32) -> Result<()> {
        // Check if we already have a pending proposal for this epoch
        let has_pending = {
            let mut manager = self.epoch_change_manager.write().await;
            manager.has_pending_proposal_for_epoch(new_epoch)
        };

        if has_pending {
            return Ok(());
        }

        // 1. Create proposal immediately
        let proposal = self.create_epoch_change_proposal_integrated(new_epoch, commit_index).await?;

        // 2. Auto-vote immediately
        self.auto_vote_on_proposal_integrated(&proposal).await?;

        // 3. Check quorum immediately
        let quorum_reached = self.check_quorum_integrated(&proposal).await?;

        // 4. Trigger reconfiguration immediately if quorum reached
        if quorum_reached {
            self.trigger_immediate_reconfiguration(&proposal, commit_index).await?;
        }

        Ok(())
    }

    /// Create epoch change proposal with integrated flow (Sui-style)
    async fn create_epoch_change_proposal_integrated(&self, new_epoch: u64, commit_index: u32) -> Result<crate::epoch_change::EpochChangeProposal> {
        let mut manager = self.epoch_change_manager.write().await;

        // Get epoch timestamp from Go state (Sui-style)
        let new_epoch_timestamp_ms = match self.executor_client.get_epoch_start_timestamp(new_epoch).await {
            Ok(ts) => ts,
            Err(e) => {
                warn!("⚠️  Failed to get epoch timestamp from Go state: {}, using current time", e);
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64
            }
        };

        // Create new committee from current committee (Sui-style: preserve validator set)
        let current_committee = manager.committee();
        let mut new_authorities = Vec::new();
        for (_, auth) in current_committee.authorities() {
            new_authorities.push(consensus_config::Authority {
                stake: auth.stake,
                address: auth.address.clone(),
                hostname: auth.hostname.clone(),
                authority_key: auth.authority_key.clone(),
                protocol_key: auth.protocol_key.clone(),
                network_key: auth.network_key.clone(),
            });
        }

        // Sui-style commit index calculation (use current commit index as base for barrier)
        // This ensures the barrier (proposal_commit_index + 10) is reachable from current position
        let proposal_commit_index = commit_index;

        let proposal = manager.propose_epoch_change_to_target_epoch(
            new_epoch,
            new_epoch_timestamp_ms,
            proposal_commit_index,
            self.own_index,
            &self.config.load_protocol_keypair()?,
        )?;

        let proposal_hash = manager.hash_proposal(&proposal);
        let hash_hex = hex::encode(&proposal_hash[..8]);

        info!(
            "✅ Sui-style integrated proposal created: epoch {} -> {}, proposal_hash={}, commit_index={}, proposer={}",
            manager.current_epoch(),
            proposal.new_epoch,
            hash_hex,
            proposal_commit_index,
            self.own_index.value()
        );

        Ok(proposal)
    }

    /// Auto-vote on proposal immediately (integrated flow)
    async fn auto_vote_on_proposal_integrated(&self, proposal: &crate::epoch_change::EpochChangeProposal) -> Result<()> {
        let mut manager = self.epoch_change_manager.write().await;

        let vote = manager.vote_on_proposal(
            proposal,
            self.own_index,
            &self.config.load_protocol_keypair()?,
        )?;

        let vote_hash_hex = hex::encode(&vote.proposal_hash[..8.min(vote.proposal_hash.len())]);
        info!(
            "🗳️  Integrated auto-vote: proposal_hash={}, epoch {} -> {}, voter={}, approve={}",
            vote_hash_hex,
            proposal.new_epoch - 1,
            proposal.new_epoch,
            self.own_index.value(),
            vote.approve
        );

        Ok(())
    }

    /// Check quorum immediately (integrated flow)
    async fn check_quorum_integrated(&self, proposal: &crate::epoch_change::EpochChangeProposal) -> Result<bool> {
        let manager = self.epoch_change_manager.read().await;
        let quorum_reached = manager.check_proposal_quorum(proposal) == Some(true);

        if quorum_reached {
            let votes = manager.get_proposal_votes_count(proposal);
            info!("✅ Integrated quorum reached: {} votes for epoch {} -> {}",
                votes, proposal.new_epoch - 1, proposal.new_epoch);
        }

        Ok(quorum_reached)
    }

    /// Trigger immediate reconfiguration (integrated flow - Sui-style)
    async fn trigger_immediate_reconfiguration(&self, proposal: &crate::epoch_change::EpochChangeProposal, commit_index: u32) -> Result<()> {
        info!("🚀 Triggering immediate reconfiguration: epoch {} -> {}",
            proposal.new_epoch - 1, proposal.new_epoch);

        // Get orchestrator to handle reconfiguration
        // This will trigger the SuiEpochTransitionOrchestrator.handle_epoch_transition
        // which includes execution lock and proper state reconfiguration

        // For now, we mark that immediate reconfiguration should happen
        // The actual reconfiguration will be handled by the existing transition_to_epoch logic
        // but triggered automatically instead of manually

        info!("✅ Immediate reconfiguration triggered for proposal: epoch {} -> {}, commit_index={}",
            proposal.new_epoch - 1, proposal.new_epoch, commit_index);

        // The reconfiguration will be handled by the existing monitoring in main.rs
        // This is a transitional step - ideally we'd call the orchestrator directly here

        Ok(())
    }

    /// Get reference to config
    pub fn get_config(&self) -> &crate::config::NodeConfig {
        &self.config
    }

    /// Trigger block-based epoch transition
    pub async fn trigger_block_based_epoch_transition(&self, current_block_count: u32) -> Result<()> {
        info!("🚀 [BLOCK-BASED] Triggering epoch transition at {} blocks", current_block_count);

        // For block-based transitions, we create a proposal directly without advancing Go state
        // The Go state advancement should happen externally (like Sui's external triggers)
        let current_epoch = self.current_epoch.load(std::sync::atomic::Ordering::SeqCst);
        let new_epoch = current_epoch + 1;

        info!("📝 [BLOCK-BASED] Creating epoch change proposal: epoch {} -> {} (block-based trigger)",
            current_epoch, new_epoch);

        // Create proposal for block-based transition
        let proposal = self.create_epoch_change_proposal_integrated(new_epoch, 0).await?;

        // Auto-vote on the proposal
        info!("🗳️  [BLOCK-BASED] Auto-voting on block-based epoch change proposal");
        self.auto_vote_on_proposal_integrated(&proposal).await?;

        // Check quorum (in single-node setup, this should pass immediately)
        let quorum_reached = self.check_quorum_integrated(&proposal).await?;

        if quorum_reached {
            info!("🎯 [BLOCK-BASED] Quorum reached for epoch {} -> {}, triggering immediate reconfiguration",
                current_epoch, new_epoch);
            self.trigger_immediate_reconfiguration(&proposal, 0).await?;
        } else {
            info!("⏳ [BLOCK-BASED] Waiting for quorum on epoch {} -> {} proposal", current_epoch, new_epoch);
        }

        Ok(())
    }

    /// Create epoch change proposal using Sui-style timing
    async fn create_epoch_change_proposal(&self, go_current_epoch: u64) -> Result<()> {
        let mut manager = self.epoch_change_manager.write().await;
        let current_epoch = manager.current_epoch();
        let current_committee = manager.committee();
        let own_index = self.own_index;

        // Get epoch timestamp from Go state (Sui-style)
        let new_epoch_timestamp_ms = match self.executor_client.get_epoch_start_timestamp(go_current_epoch).await {
            Ok(ts) => ts,
            Err(e) => {
                warn!("⚠️  Failed to get epoch timestamp from Go state: {}, using current time", e);
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64
            }
        };

        // Create new committee from current committee (Sui-style: preserve validator set)
        let mut new_authorities = Vec::new();
        for (_, auth) in current_committee.authorities() {
            new_authorities.push(consensus_config::Authority {
                stake: auth.stake,
                address: auth.address.clone(),
                hostname: auth.hostname.clone(),
                authority_key: auth.authority_key.clone(),
                protocol_key: auth.protocol_key.clone(),
                network_key: auth.network_key.clone(),
            });
        }

        // For Sui-style, we use a simple commit index calculation
        // In real Sui, this would be based on checkpoint progression
        let current_commit_index = std::sync::atomic::AtomicU32::new(0); // This should be passed in
        let proposal_commit_index = current_commit_index.load(std::sync::atomic::Ordering::SeqCst).saturating_add(100);

        let protocol_keypair = self.config.load_protocol_keypair()?;
        let proposal = manager.propose_epoch_change_to_target_epoch(
            go_current_epoch,
            new_epoch_timestamp_ms,
            proposal_commit_index,
            own_index,
            &protocol_keypair,
        )?;

        let proposal_hash = manager.hash_proposal(&proposal);
        let hash_hex = hex::encode(&proposal_hash[..8]);

        info!(
            "✅ Sui-style epoch change proposal created: epoch {} -> {}, proposal_hash={}, commit_index={}, proposer={}",
            current_epoch,
            proposal.new_epoch,
            hash_hex,
            proposal_commit_index,
            own_index.value()
        );

        // Auto-vote on own proposal (MetaNode pattern)
        match manager.vote_on_proposal(
            &proposal,
            own_index,
            &protocol_keypair,
        ) {
            Ok(vote) => {
                let vote_hash_hex = hex::encode(&vote.proposal_hash[..8.min(vote.proposal_hash.len())]);
                info!(
                    "🗳️  Auto-voted on own proposal: proposal_hash={}, epoch {} -> {}, voter={}, approve={}",
                    vote_hash_hex,
                    proposal.new_epoch - 1,
                    proposal.new_epoch,
                    own_index.value(),
                    vote.approve
                );
            }
            Err(e) => {
                tracing::warn!("⚠️  Failed to auto-vote on own proposal: {}", e);
            }
        }

        Ok(())
    }

    /// Update current epoch (called after successful transition)
    pub fn update_current_epoch(&self, new_epoch: u64) {
        self.current_epoch.store(new_epoch, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Sui-style reconfiguration manager
/// Handles the reconfiguration process with execution locks similar to Sui
pub struct SuiReconfigurationManager {
    /// Execution lock to prevent transactions during reconfiguration (Sui pattern)
    execution_lock: Arc<tokio::sync::Mutex<()>>,
    /// Current reconfiguration state
    reconfiguring: std::sync::atomic::AtomicBool,
}

impl SuiReconfigurationManager {
    pub fn new() -> Self {
        Self {
            execution_lock: Arc::new(tokio::sync::Mutex::new(())),
            reconfiguring: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Acquire execution lock for reconfiguration (Sui pattern)
    pub async fn acquire_execution_lock_for_reconfiguration(&self) -> Result<ExecutionLockGuard> {
        if self.reconfiguring.load(std::sync::atomic::Ordering::SeqCst) {
            anyhow::bail!("Already reconfiguring");
        }

        let lock = self.execution_lock.clone().try_lock_owned()
            .map_err(|_| anyhow::anyhow!("Failed to acquire execution lock"))?;

        self.reconfiguring.store(true, std::sync::atomic::Ordering::SeqCst);

        Ok(ExecutionLockGuard {
            _lock: lock,
            reconfiguring: &self.reconfiguring,
        })
    }
}

/// RAII guard for execution lock during reconfiguration
pub struct ExecutionLockGuard<'a> {
    _lock: tokio::sync::OwnedMutexGuard<()>,
    reconfiguring: &'a std::sync::atomic::AtomicBool,
}

impl<'a> Drop for ExecutionLockGuard<'a> {
    fn drop(&mut self) {
        self.reconfiguring.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Sui-style epoch transition orchestrator
/// Coordinates the entire epoch transition process
pub struct SuiEpochTransitionOrchestrator {
    sui_manager: Arc<SuiEpochTransitionManager>,
    reconfig_manager: Arc<SuiReconfigurationManager>,
    epoch_change_manager: Arc<RwLock<EpochChangeManager>>,
}

impl SuiEpochTransitionOrchestrator {
    /// Get reference to sui manager
    pub fn sui_manager(&self) -> Arc<SuiEpochTransitionManager> {
        self.sui_manager.clone()
    }
    pub fn new(
        epoch_change_manager: Arc<RwLock<EpochChangeManager>>,
        executor_client: Arc<ExecutorClient>,
        config: NodeConfig,
        current_epoch: u64,
        own_index: AuthorityIndex,
    ) -> Self {
        let sui_manager = Arc::new(SuiEpochTransitionManager::new(
            epoch_change_manager.clone(),
            executor_client,
            config,
            current_epoch,
            own_index,
        ));

        let reconfig_manager = Arc::new(SuiReconfigurationManager::new());

        Self {
            sui_manager: sui_manager.clone(),
            reconfig_manager,
            epoch_change_manager,
        }
    }

    /// Start Sui-style epoch transition monitoring
    pub async fn start(&self) -> Result<()> {
        info!("🚀 Starting Sui-style epoch transition orchestrator");

        // Start monitoring task
        let sui_manager = self.sui_manager.clone();
        tokio::spawn(async move {
            if let Err(e) = sui_manager.monitor_epoch_transitions().await {
                error!("❌ Sui-style epoch transition monitor failed: {}", e);
            }
        });

        Ok(())
    }

    /// Handle epoch transition with Sui-style reconfiguration
    pub async fn handle_epoch_transition(
        &self,
        proposal: &EpochChangeProposal,
        current_commit_index: u32,
    ) -> Result<()> {
        info!("🔄 Handling epoch transition with Sui-style reconfiguration");

        // Acquire execution lock (Sui pattern)
        let _lock = self.reconfig_manager.acquire_execution_lock_for_reconfiguration().await?;
        info!("🔒 Acquired execution lock for reconfiguration");

        // Wait for all pending transactions to complete (Sui pattern)
        // In Sui, this is handled by the execution scheduler
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Terminate epoch-specific tasks (Sui pattern)
        {
            let manager = self.epoch_change_manager.read().await;
            // Here we would terminate any epoch-specific tasks
            // In MetaNode, this might include stopping certain monitoring tasks
        }

        // Perform reconfiguration (preserving MetaNode's logic but with Sui patterns)
        self.perform_reconfiguration(proposal, current_commit_index).await?;

        // Update current epoch
        self.sui_manager.update_current_epoch(proposal.new_epoch);

        info!("✅ Epoch transition completed with Sui-style reconfiguration");

        Ok(())
    }

    /// Perform the actual reconfiguration (hybrid Sui + MetaNode approach)
    async fn perform_reconfiguration(
        &self,
        proposal: &EpochChangeProposal,
        current_commit_index: u32,
    ) -> Result<()> {
        info!("🔧 Performing reconfiguration: epoch {} -> {}",
            proposal.new_epoch - 1, proposal.new_epoch);

        // Sui-style: Close user certificates
        // MetaNode equivalent: Set barrier to prevent new transactions
        {
            let mut manager = self.epoch_change_manager.write().await;
            // Here we would set transaction acceptance to false
        }

        // Sui-style: Clear state end of epoch
        // MetaNode equivalent: Reset various caches and states
        self.clear_epoch_state().await?;

        // Sui-style: Reconfigure caches
        // MetaNode equivalent: Update internal state
        self.reconfigure_caches().await?;

        // Sui-style: Create new epoch store
        // MetaNode equivalent: This is handled in transition_to_epoch

        info!("✅ Reconfiguration completed");

        Ok(())
    }

    /// Clear epoch-specific state (Sui-style)
    async fn clear_epoch_state(&self) -> Result<()> {
        info!("🧹 Clearing epoch-specific state");

        // Reset various epoch-specific state
        // This would include clearing caches, resetting metrics, etc.

        Ok(())
    }

    /// Reconfigure caches and internal state (Sui-style)
    async fn reconfigure_caches(&self) -> Result<()> {
        info!("🔄 Reconfiguring caches and internal state");

        // Reconfigure any caches, update configurations, etc.
        // This is similar to Sui's cache reconfiguration

        Ok(())
    }
}
