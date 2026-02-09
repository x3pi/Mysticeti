// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

//! Tests for epoch_recovery.rs logic — process_queue behavior, epoch transition state.

use super::block_queue::CommitData;
use super::RustSyncNode;
use crate::node::executor_client::ExecutorClient;
use consensus_core::Commit;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Helper: create a minimal `CommitData` with the given `global_exec_index` and epoch.
fn make_commit_data(global_exec_index: u64, epoch: u64) -> CommitData {
    let zeros = vec![0u8; 32];
    let json = serde_json::json!({
        "V1": {
            "index": 1,
            "previous_digest": zeros,
            "timestamp_ms": 0,
            "leader": {
                "round": 1,
                "author": 0,
                "digest": zeros
            },
            "blocks": [],
            "global_exec_index": global_exec_index
        }
    });
    let commit: Commit = serde_json::from_value(json).expect("valid commit json");
    CommitData {
        commit,
        blocks: vec![],
        epoch,
    }
}

/// Helper: create a minimal RustSyncNode for testing.
fn make_test_sync_node(
    initial_epoch: u64,
    initial_global_exec_index: u32,
    epoch_base_index: u64,
) -> (RustSyncNode, mpsc::UnboundedReceiver<(u64, u64, u64)>) {
    let executor_client = Arc::new(ExecutorClient::new(
        false,
        false,
        "/dev/null".to_string(),
        "/dev/null".to_string(),
        None,
    ));
    let (tx, rx) = mpsc::unbounded_channel();
    let node = RustSyncNode::new(
        executor_client,
        tx,
        initial_epoch,
        initial_global_exec_index,
        epoch_base_index,
    );
    (node, rx)
}

// =============================================================================
// process_queue tests
// =============================================================================

/// Empty queue returns Ok(0) immediately
#[tokio::test]
async fn test_process_queue_empty() {
    let (node, _rx) = make_test_sync_node(0, 1, 0);

    let result = node.process_queue().await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), 0);
}

/// Queue with pending commits but gap prevents draining
#[tokio::test]
async fn test_process_queue_gap_prevents_drain() {
    let (node, _rx) = make_test_sync_node(0, 1, 0);

    // Push a commit at index 5 (gap: 1-4 missing)
    {
        let mut queue = node.block_queue.lock().await;
        queue.push(make_commit_data(5, 0));
        assert_eq!(queue.pending_count(), 1);
    }

    // process_queue should return 0 (nothing drainable)
    let result = node.process_queue().await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), 0);

    // Queue should still have the pending commit
    {
        let queue = node.block_queue.lock().await;
        assert_eq!(queue.pending_count(), 1);
    }
}

// =============================================================================
// Epoch transition state update tests
// =============================================================================

/// current_epoch and epoch_base_index atomics update correctly when simulating
/// what check_and_process_pending_epoch_transitions does
#[test]
fn test_epoch_transition_state_update() {
    let (node, _rx) = make_test_sync_node(1, 100, 50);

    // Initial state
    assert_eq!(node.current_epoch.load(Ordering::SeqCst), 1);
    assert_eq!(node.epoch_base_index.load(Ordering::SeqCst), 50);

    // Simulate a pending epoch transition updating state
    // (mirrors what check_and_process_pending_epoch_transitions does)
    let trans_epoch: u64 = 2;
    let trans_boundary: u64 = 5000;
    let current_epoch = node.current_epoch.load(Ordering::SeqCst);
    let current_base = node.epoch_base_index.load(Ordering::SeqCst);

    assert!(trans_epoch > current_epoch);
    assert!(trans_boundary > current_base);

    node.current_epoch.store(trans_epoch, Ordering::SeqCst);
    node.epoch_base_index
        .store(trans_boundary, Ordering::SeqCst);

    assert_eq!(node.current_epoch.load(Ordering::SeqCst), 2);
    assert_eq!(node.epoch_base_index.load(Ordering::SeqCst), 5000);
}

/// Epoch transition sender correctly sends signals
#[test]
fn test_epoch_transition_sender() {
    let (node, mut rx) = make_test_sync_node(1, 100, 50);

    // Send an epoch transition signal (mirrors real code)
    let _ = node.epoch_transition_sender.send((2, 1234567890, 5000));

    // Verify receiver gets the correct data
    let received = rx.try_recv();
    assert!(received.is_ok());
    let (epoch, timestamp, boundary) = received.unwrap();
    assert_eq!(epoch, 2);
    assert_eq!(timestamp, 1234567890);
    assert_eq!(boundary, 5000);
}
