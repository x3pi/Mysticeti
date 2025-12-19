// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

/// Calculate deterministic global execution index (Checkpoint Sequence Number)
/// This ensures all nodes compute the same value from consensus state
/// 
/// Formula:
/// - Epoch 0: global_exec_index = commit_index
/// - Epoch N (N > 0): global_exec_index = last_global_exec_index + commit_index
/// 
/// This is similar to Sui's Checkpoint Sequence Number which increases continuously
/// across epochs without resetting.
pub fn calculate_global_exec_index(
    epoch: u64,
    commit_index: u32,
    last_global_exec_index: u64,
) -> u64 {
    if epoch == 0 {
        // Epoch 0: start from commit_index (which starts from 0 or 1)
        commit_index as u64
    } else {
        // Epoch N: continue from last_global_exec_index
        // last_global_exec_index is the last global_exec_index of previous epoch
        last_global_exec_index + commit_index as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_global_exec_index_epoch_0() {
        // Epoch 0: global_exec_index = commit_index
        assert_eq!(calculate_global_exec_index(0, 0, 0), 0);
        assert_eq!(calculate_global_exec_index(0, 1, 0), 1);
        assert_eq!(calculate_global_exec_index(0, 100, 0), 100);
    }

    #[test]
    fn test_calculate_global_exec_index_epoch_1() {
        // Epoch 1: global_exec_index = last_global_exec_index + commit_index
        // Assume epoch 0 ended at commit_index 100, so last_global_exec_index = 100
        assert_eq!(calculate_global_exec_index(1, 0, 100), 100);
        assert_eq!(calculate_global_exec_index(1, 1, 100), 101);
        assert_eq!(calculate_global_exec_index(1, 50, 100), 150);
    }

    #[test]
    fn test_calculate_global_exec_index_epoch_2() {
        // Epoch 2: continue from epoch 1
        // Assume epoch 1 ended at commit_index 50, so last_global_exec_index = 150
        assert_eq!(calculate_global_exec_index(2, 0, 150), 150);
        assert_eq!(calculate_global_exec_index(2, 1, 150), 151);
        assert_eq!(calculate_global_exec_index(2, 25, 150), 175);
    }

    #[test]
    fn test_deterministic_across_nodes() {
        // All nodes with same epoch, commit_index, last_global_exec_index
        // should compute the same global_exec_index
        let epoch = 5;
        let commit_index = 42;
        let last_global_exec_index = 1000;
        
        let result1 = calculate_global_exec_index(epoch, commit_index, last_global_exec_index);
        let result2 = calculate_global_exec_index(epoch, commit_index, last_global_exec_index);
        let result3 = calculate_global_exec_index(epoch, commit_index, last_global_exec_index);
        
        assert_eq!(result1, result2);
        assert_eq!(result2, result3);
        assert_eq!(result1, 1042); // 1000 + 42
    }
}

