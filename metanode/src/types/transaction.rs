// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use consensus_core::{TransactionVerifier, ValidationError};

/// No-op transaction verifier for testing
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
