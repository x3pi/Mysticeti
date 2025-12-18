// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;
use crate::epoch_change::{EpochChangeManager, EpochChangeProposal, EpochChangeVote};
use consensus_config::ProtocolKeyPair;
use consensus_core::epoch_change_provider::{EpochChangeProvider, EpochChangeProcessor};

/// Global epoch change hook for Core to access
/// This allows Core to get epoch change data without direct dependency
static EPOCH_CHANGE_HOOK: Mutex<Option<Arc<EpochChangeHook>>> = Mutex::new(None);

/// Cached epoch change data for sync access
/// Updated async, read sync to avoid blocking runtime
struct CachedEpochChangeData {
    proposal: Option<Vec<u8>>,
    votes: Vec<Vec<u8>>,
}

/// Provider implementation for Core
struct EpochChangeProviderImpl {
    hook: Arc<EpochChangeHook>,
}

impl EpochChangeProvider for EpochChangeProviderImpl {
    fn get_proposal(&self) -> Option<Vec<u8>> {
        self.hook.get_epoch_change_data_sync().0
    }
    
    fn get_votes(&self) -> Vec<Vec<u8>> {
        self.hook.get_epoch_change_data_sync().1
    }
}

/// Processor implementation for AuthorityService
/// Uses a channel to batch process proposals and votes together
struct EpochChangeProcessorImpl {
    #[allow(dead_code)] // Reserved for future use (e.g., direct hook access)
    hook: Arc<EpochChangeHook>,
    // Channel to batch process epoch change data
    tx: tokio::sync::mpsc::UnboundedSender<(Option<Vec<u8>>, Vec<Vec<u8>>)>,
}

impl EpochChangeProcessorImpl {
    fn new(hook: Arc<EpochChangeHook>) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(Option<Vec<u8>>, Vec<Vec<u8>>)>();
        let hook_clone = hook.clone();
        
        // Spawn background task to process epoch change data
        tokio::spawn(async move {
            while let Some((proposal, votes)) = rx.recv().await {
                if let Err(e) = hook_clone.process_block_epoch_change(
                    proposal.as_deref(),
                    &votes
                ).await {
                    tracing::warn!("Failed to process epoch change data: {}", e);
                }
            }
        });
        
        Self { hook, tx }
    }
}

impl EpochChangeProcessor for EpochChangeProcessorImpl {
    fn process_proposal(&self, proposal_bytes: &[u8]) {
        // Send proposal with empty votes (votes will be sent separately if present)
        let _ = self.tx.send((Some(proposal_bytes.to_vec()), Vec::new()));
    }
    
    fn process_vote(&self, vote_bytes: &[u8]) {
        // Send vote with no proposal (proposal was already processed or will be in another block)
        let _ = self.tx.send((None, vec![vote_bytes.to_vec()]));
    }
}

// Helper to process proposal and votes together (called from authority_service)
// NOTE: This method is kept for future optimization when processing both together
impl EpochChangeProcessorImpl {
    #[allow(dead_code)]
    pub fn process_together(&self, proposal_bytes: Option<&[u8]>, votes_bytes: &[Vec<u8>]) {
        let proposal = proposal_bytes.map(|b| b.to_vec());
        let votes = votes_bytes.to_vec();
        let _ = self.tx.send((proposal, votes));
    }
}

/// Hook to provide epoch change data to Core for block creation
/// This allows Core to include epoch change proposals/votes in blocks
/// without needing direct access to EpochChangeManager
pub struct EpochChangeHook {
    epoch_change_manager: Arc<RwLock<EpochChangeManager>>,
    protocol_keypair: Arc<ProtocolKeyPair>,
    own_index: consensus_config::AuthorityIndex,
    /// Cached data for sync access (updated async, read sync)
    cached_data: Arc<Mutex<CachedEpochChangeData>>,
}

impl EpochChangeHook {
    pub fn new(
        epoch_change_manager: Arc<RwLock<EpochChangeManager>>,
        protocol_keypair: Arc<ProtocolKeyPair>,
        own_index: consensus_config::AuthorityIndex,
    ) -> Self {
        let cached_data = Arc::new(Mutex::new(CachedEpochChangeData {
            proposal: None,
            votes: Vec::new(),
        }));
        
        let cached_data_clone = cached_data.clone();
        let manager_clone = epoch_change_manager.clone();
        
        // Spawn background task to update cache periodically
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(100));
            loop {
                interval.tick().await;
                
                // Update cache
                let manager = manager_clone.read().await;
                let proposal = manager.get_pending_proposal_to_broadcast()
                    .and_then(|p| p.to_bytes().ok());
                let votes = manager.get_pending_votes_to_broadcast()
                    .into_iter()
                    .filter_map(|v| v.to_bytes().ok())
                    .collect();
                drop(manager);
                
                // Update cache
                let mut cache = cached_data_clone.lock().unwrap();
                cache.proposal = proposal;
                cache.votes = votes;
            }
        });
        
        Self {
            epoch_change_manager,
            protocol_keypair,
            own_index,
            cached_data,
        }
    }

    /// Get epoch change data to include in next block
    /// Returns: (proposal_bytes, votes_bytes)
    /// NOTE: This method is kept for potential future direct async access
    #[allow(dead_code)]
    pub async fn get_epoch_change_data_for_block(&self) -> (Option<Vec<u8>>, Vec<Vec<u8>>) {
        let manager = self.epoch_change_manager.read().await;
        
        // Get pending proposal to broadcast
        let proposal = manager.get_pending_proposal_to_broadcast()
            .and_then(|p| p.to_bytes().ok());
        
        // Get pending votes to broadcast
        let votes = manager.get_pending_votes_to_broadcast()
            .into_iter()
            .filter_map(|v| v.to_bytes().ok())
            .collect();
        
        (proposal, votes)
    }

    /// Process epoch change data from a received block
    pub async fn process_block_epoch_change(
        &self,
        proposal_bytes: Option<&[u8]>,
        votes_bytes: &[Vec<u8>],
    ) -> Result<(), anyhow::Error> {
        // Process proposal if present
        if let Some(bytes) = proposal_bytes {
            match EpochChangeProposal::from_bytes(bytes) {
                Ok(proposal) => {
                    tracing::info!("📥 Received epoch change proposal in block: epoch {} -> {}, proposal_hash={}", 
                        proposal.new_epoch - 1, 
                        proposal.new_epoch,
                        hex::encode(&self.epoch_change_manager.read().await.hash_proposal(&proposal)[..8]));
                    
                    let mut manager = self.epoch_change_manager.write().await;
                    match manager.process_proposal(proposal.clone()) {
                        Ok(()) => {
                            tracing::info!("✅ Processed epoch change proposal: epoch {} -> {}", 
                                proposal.new_epoch - 1, proposal.new_epoch);
                            
                            // Auto-vote on valid proposal
                            match manager.vote_on_proposal(
                                &proposal,
                                self.own_index,
                                &self.protocol_keypair,
                            ) {
                                Ok(vote) => {
                                    tracing::info!("🗳️  Auto-voted on proposal: epoch {} -> {}, approve={}", 
                                        proposal.new_epoch - 1, proposal.new_epoch, vote.approve);
                                }
                                Err(e) => {
                                    tracing::warn!("⚠️  Failed to auto-vote on proposal: {}", e);
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("❌ Failed to process epoch change proposal: {}", e);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("⚠️  Failed to deserialize epoch change proposal: {}", e);
                }
            }
        }

        // Process votes
        for vote_bytes in votes_bytes {
            match EpochChangeVote::from_bytes(vote_bytes) {
                Ok(vote) => {
                    let mut manager = self.epoch_change_manager.write().await;
                    match manager.process_vote(vote.clone()) {
                        Ok(()) => {
                            tracing::info!("✅ Processed epoch change vote: voter={}, approve={}, proposal_hash={}", 
                                vote.voter().value(),
                                vote.approve,
                                hex::encode(&vote.proposal_hash[..8]));
                            
                            // Check quorum after processing vote
                            if let Some(proposal) = manager.get_all_pending_proposals()
                                .iter()
                                .find(|p| {
                                    let hash = manager.hash_proposal(p);
                                    hash == vote.proposal_hash
                                }) {
                                if let Some(approved) = manager.check_proposal_quorum(proposal) {
                                    if approved {
                                        tracing::info!("🎉 QUORUM REACHED for epoch change proposal: epoch {} -> {}", 
                                            proposal.new_epoch - 1, proposal.new_epoch);
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("❌ Failed to process epoch change vote: {}", e);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("⚠️  Failed to deserialize epoch change vote: {}", e);
                }
            }
        }

        Ok(())
    }
}

impl EpochChangeHook {
    /// Initialize global hook and provider (called from ConsensusNode)
    pub fn init_global(hook: Arc<Self>) {
        let mut global = EPOCH_CHANGE_HOOK.lock().unwrap();
        if global.is_some() {
            panic!("EpochChangeHook already initialized");
        }
        *global = Some(hook.clone());
        drop(global);
        
        // Initialize provider for Core
        let provider = Box::new(EpochChangeProviderImpl { hook: hook.clone() });
        consensus_core::epoch_change_provider::init_epoch_change_provider(provider);
        
        // Initialize processor for AuthorityService
        let processor = Box::new(EpochChangeProcessorImpl::new(hook));
        consensus_core::epoch_change_provider::init_epoch_change_processor(processor);
    }

    /// Get global hook (called from metanode layer)
    /// NOTE: Reserved for future use (e.g., direct access from other modules)
    #[allow(dead_code)]
    pub fn get_global() -> Option<Arc<Self>> {
        EPOCH_CHANGE_HOOK.lock().unwrap().clone()
    }

    /// Get epoch change data synchronously (for use in Core's sync context)
    /// Reads from cache to avoid blocking runtime
    pub fn get_epoch_change_data_sync(&self) -> (Option<Vec<u8>>, Vec<Vec<u8>>) {
        // Read from cache (updated async by background task)
        let cache = self.cached_data.lock().unwrap();
        (cache.proposal.clone(), cache.votes.clone())
    }
}

