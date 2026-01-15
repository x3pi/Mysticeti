// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

/// Calculate deterministic global execution index (Checkpoint Sequence Number)
/// This ensures all nodes compute the same value from consensus state
/// 
/// Formula:
/// - Epoch 0: global_exec_index = commit_index (commit_index starts from 1, so global_exec_index starts from 1)
/// - Epoch N (N > 0): global_exec_index = last_global_exec_index + commit_index
/// 
/// IMPORTANT: In Mysticeti, commit_index starts from 1 in every epoch
/// (`CommitProcessor::next_expected_index` starts at 1). Therefore, epoch N's first commit
/// (commit_index=1) must map to global_exec_index = last_global_exec_index + 1 for continuous,
/// strictly increasing execution order across epochs.
/// 
/// This is similar to Sui's Checkpoint Sequence Number which increases continuously
/// across epochs without resetting.
/// Calculate global execution index as sequential block number
/// GLOBAL REFACTOR: global_exec_index now represents sequential block numbers
/// that are shared between Rust and Go for unified block ordering
pub fn calculate_global_exec_index(
    _epoch: u64,  // No longer used - block numbers are global and sequential
    _commit_index: u32, // No longer used - each commit gets next sequential number
    last_global_exec_index: u64,
) -> u64 {
    // GLOBAL BLOCK NUMBERING: Each commit gets the next sequential block number
    // This ensures Rust and Go use the same block numbering system
    //
    // Example:
    // - Genesis: global_exec_index = 1 (first block)
    // - Next commit: global_exec_index = 2
    // - After epoch transition: global_exec_index = last_global_exec_index + 1
    //
    // This creates: 1, 2, 3, 4, 5, ... continuous block numbers

    last_global_exec_index + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_global_exec_index_sequential_block_numbers() {
        // GLOBAL BLOCK NUMBERING: global_exec_index = last_global_exec_index + 1
        // Independent of epoch and commit_index - represents sequential block numbers

        // Genesis case: first block
        assert_eq!(calculate_global_exec_index(0, 0, 0), 1); // First block is block 1

        // Sequential blocks (epoch and commit_index ignored)
        assert_eq!(calculate_global_exec_index(0, 1, 1), 2); // Block 2
        assert_eq!(calculate_global_exec_index(5, 100, 2), 3); // Block 3 (epoch/commit_index ignored)
        assert_eq!(calculate_global_exec_index(10, 500, 99), 100); // Block 100

        // Epoch transitions: continue sequential numbering
        assert_eq!(calculate_global_exec_index(1, 1, 100), 101); // Block 101 (after epoch transition)
        assert_eq!(calculate_global_exec_index(1, 50, 101), 102); // Block 102
        assert_eq!(calculate_global_exec_index(2, 1, 150), 151); // Block 151 (after another epoch transition)
    }

    #[test]
    fn test_global_exec_index_deterministic() {
        // All nodes with same last_global_exec_index should compute same next block number
        let last_global_exec_index = 1000;

        let result1 = calculate_global_exec_index(0, 0, last_global_exec_index);
        let result2 = calculate_global_exec_index(5, 50, last_global_exec_index);
        let result3 = calculate_global_exec_index(10, 100, last_global_exec_index);

        assert_eq!(result1, result2);
        assert_eq!(result2, result3);
        assert_eq!(result1, 1001); // Always last_global_exec_index + 1
    }
}

