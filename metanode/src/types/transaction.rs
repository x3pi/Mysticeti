// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use consensus_core::{TransactionClient, TransactionVerifier, ValidationError};
use std::sync::Arc;

/// No-op transaction verifier for testing
#[allow(dead_code)] // Used in node.rs
pub struct NoopTransactionVerifier;

impl TransactionVerifier for NoopTransactionVerifier {
    fn verify_batch(&self, _batch: &[&[u8]]) -> Result<(), ValidationError> {
        Ok(())
    }

    fn verify_and_vote_batch(
        &self,
        _block_ref: &consensus_types::block::BlockRef,
        _batch: &[&[u8]],
    ) -> Result<Vec<consensus_types::block::TransactionIndex>, ValidationError> {
        // Return empty vec - no transactions to reject
        Ok(vec![])
    }
}

/// Simple transaction handler
/// Reserved for future use - allows submitting transactions to consensus
#[allow(dead_code)] // Reserved for future use
pub struct TransactionHandler {
    client: Arc<TransactionClient>,
}

#[allow(dead_code)] // Reserved for future use
impl TransactionHandler {
    pub fn new(client: Arc<TransactionClient>) -> Self {
        Self { client }
    }

    pub async fn submit_transaction(&self, tx: Vec<u8>) -> Result<()> {
        // Submit transaction to consensus
        let transactions = vec![tx];
        let (_block_ref, _indices, _status_receiver) = self.client.submit(transactions).await
            .map_err(|e| anyhow::anyhow!("Failed to submit transaction: {}", e))?;
        Ok(())
    }

    pub async fn submit_transactions(&self, transactions: Vec<Vec<u8>>) -> Result<()> {
        // Submit multiple transactions to consensus
        let (_block_ref, _indices, _status_receiver) = self.client.submit(transactions).await
            .map_err(|e| anyhow::anyhow!("Failed to submit transactions: {}", e))?;
        Ok(())
    }
}
