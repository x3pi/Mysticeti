// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use consensus_core::VerifiedBlock;
use crate::epoch_change::{EpochChangeManager, EpochChangeProposal, EpochChangeVote};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// Bridge to integrate epoch change with block creation and processing
#[allow(dead_code)] // Will be used when implementing block integration
pub struct EpochChangeBridge;

impl EpochChangeBridge {
    /// Extract epoch change proposal from block and process it
    /// NOTE: This method is kept for future use when block integration is fully enabled
    #[allow(dead_code)]
    pub async fn process_block_epoch_change(
        block: &VerifiedBlock,
        epoch_change_manager: &Arc<RwLock<EpochChangeManager>>,
    ) -> Result<()> {
        // VerifiedBlock implements Deref to Block, so we can use it directly
        
        // Extract proposal if present
        if let Some(proposal_bytes) = block.epoch_change_proposal() {
            match EpochChangeProposal::from_bytes(proposal_bytes) {
                Ok(proposal) => {
                    let mut manager = epoch_change_manager.write().await;
                    let current_epoch = manager.current_epoch();

                    // Ignore stale/out-of-order proposals (common around epoch boundaries).
                    if proposal.new_epoch != current_epoch + 1 {
                        tracing::debug!(
                            "⏭️  Ignoring epoch change proposal: proposal targets epoch {} but current_epoch={} (expected {}), proposal_hash={}",
                            proposal.new_epoch,
                            current_epoch,
                            current_epoch + 1,
                            hex::encode(&manager.hash_proposal(&proposal)[..8]),
                        );
                        // Not an error.
                    } else {
                        let proposal_hash = manager.hash_proposal(&proposal);
                        if !manager.has_pending_proposal_hash(&proposal_hash) {
                            info!(
                                "📥 Received epoch change proposal in block: epoch {} -> {}, proposal_hash={}",
                                proposal.new_epoch - 1,
                                proposal.new_epoch,
                                hex::encode(&proposal_hash[..8])
                            );
                        }

                        match manager.process_proposal(proposal.clone()) {
                        Ok(()) => {
                            tracing::debug!(
                                "✅ Processed epoch change proposal: epoch {} -> {}",
                                proposal.new_epoch - 1,
                                proposal.new_epoch
                            );
                            
                            // Note: Auto-vote requires protocol keypair which is not available in this context
                            // Auto-vote will be handled in block creation context where keypair is available
                        }
                        Err(e) => {
                            warn!("❌ Failed to process epoch change proposal: {}", e);
                        }
                    }
                    }
                }
                Err(e) => {
                    warn!("⚠️  Failed to deserialize epoch change proposal: {}", e);
                }
            }
        }

        // Extract votes if present
        for vote_bytes in block.epoch_change_votes() {
            match EpochChangeVote::from_bytes(vote_bytes) {
                Ok(vote) => {
                    let mut manager = epoch_change_manager.write().await;
                    if !manager.has_pending_proposal_hash(&vote.proposal_hash) {
                        tracing::debug!(
                            "⏭️  Ignoring epoch change vote for unknown/stale proposal: voter={}, approve={}, proposal_hash={}",
                            vote.voter().value(),
                            vote.approve,
                            hex::encode(&vote.proposal_hash[..8])
                        );
                        continue;
                    }
                    match manager.process_vote(vote.clone()) {
                        Ok(()) => {
                            tracing::debug!(
                                "✅ Processed epoch change vote: voter={}, approve={}, proposal_hash={}",
                                vote.voter().value(),
                                vote.approve,
                                hex::encode(&vote.proposal_hash[..8])
                            );
                            
                            // Check quorum after processing vote
                            if let Some(proposal) = manager.get_all_pending_proposals()
                                .iter()
                                .find(|p| {
                                    let hash = manager.hash_proposal(p);
                                    hash == vote.proposal_hash
                                }) {
                                if let Some(approved) = manager.check_proposal_quorum(proposal) {
                                    if approved {
                                        info!("🎉 QUORUM REACHED for epoch change proposal: epoch {} -> {}", 
                                            proposal.new_epoch - 1, proposal.new_epoch);
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!("❌ Failed to process epoch change vote: {}", e);
                        }
                    }
                }
                Err(e) => {
                    warn!("⚠️  Failed to deserialize epoch change vote: {}", e);
                }
            }
        }

        Ok(())
    }

    /// Get epoch change data to include in next block
    #[allow(dead_code)] // Will be used when implementing block creation
    pub async fn get_epoch_change_data_for_block(
        epoch_change_manager: &Arc<RwLock<EpochChangeManager>>,
    ) -> (Option<Vec<u8>>, Vec<Vec<u8>>) {
        let manager = epoch_change_manager.read().await;
        
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

    /// Include epoch change data in a block (mutates the block)
    /// Note: This requires mutable access to the block, which may not be available
    /// in all contexts. For now, this is a placeholder for future integration.
    #[allow(dead_code)] // Will be used when implementing block creation
    pub async fn get_epoch_change_data_for_block_creation(
        epoch_change_manager: &Arc<RwLock<EpochChangeManager>>,
    ) -> (Option<Vec<u8>>, Vec<Vec<u8>>) {
        Self::get_epoch_change_data_for_block(epoch_change_manager).await
    }
}

