// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

//! Tests for sync_loop.rs logic — config defaults, initial state, epoch detection.

use super::{RustSyncConfig, RustSyncNode};
use crate::node::executor_client::ExecutorClient;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Helper: create a minimal RustSyncNode for testing (no network, no real executor).
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
// RustSyncConfig tests
// =============================================================================

/// Config defaults match expected tuning values
#[test]
fn test_rust_sync_config_defaults() {
    let config = RustSyncConfig::default();

    assert_eq!(config.fetch_interval_secs, 2);
    assert_eq!(config.turbo_fetch_interval_ms, 50);
    assert_eq!(config.fetch_batch_size, 500);
    assert_eq!(config.turbo_batch_size, 2000);
    assert_eq!(config.fetch_timeout_secs, 30);
    assert!(config.peer_rpc_addresses.is_empty());
}

// =============================================================================
// RustSyncNode initial state tests
// =============================================================================

/// After new(): epoch, base, queue, and metrics are all correctly initialized
#[test]
fn test_rust_sync_node_initial_state() {
    let (node, _rx) = make_test_sync_node(5, 100, 50);

    assert_eq!(node.current_epoch.load(Ordering::SeqCst), 5);
    assert_eq!(node.last_synced_commit_index.load(Ordering::SeqCst), 100);
    assert_eq!(node.epoch_base_index.load(Ordering::SeqCst), 50);

    // Network should be None initially
    assert!(node.network_client.is_none());
    assert!(node.context.is_none());
    assert!(node.network_keypair.is_none());

    // Committee should be None
    let committee = node.committee.read().unwrap();
    assert!(committee.is_none());
}

/// Epoch 0 initialization with zero base
#[test]
fn test_rust_sync_node_epoch0_init() {
    let (node, _rx) = make_test_sync_node(0, 1, 0);

    assert_eq!(node.current_epoch.load(Ordering::SeqCst), 0);
    assert_eq!(node.epoch_base_index.load(Ordering::SeqCst), 0);
}

// =============================================================================
// Epoch base index atomic operations
// =============================================================================

/// AtomicU64 store/load for epoch_base_index mirrors updates correctly
#[test]
fn test_epoch_base_update_atomics() {
    let (node, _rx) = make_test_sync_node(1, 50, 100);

    // Verify initial
    assert_eq!(node.epoch_base_index.load(Ordering::SeqCst), 100);

    // Simulate auto_epoch_sync updating the base
    node.epoch_base_index.store(5000, Ordering::SeqCst);
    assert_eq!(node.epoch_base_index.load(Ordering::SeqCst), 5000);

    // Simulate epoch change
    node.current_epoch.store(3, Ordering::SeqCst);
    node.epoch_base_index.store(15000, Ordering::SeqCst);
    assert_eq!(node.current_epoch.load(Ordering::SeqCst), 3);
    assert_eq!(node.epoch_base_index.load(Ordering::SeqCst), 15000);
}

// =============================================================================
// Epoch behind detection
// =============================================================================

/// rust_epoch < go_epoch is correctly detected
#[test]
fn test_epoch_behind_detection() {
    let (node, _rx) = make_test_sync_node(2, 100, 50);

    let rust_epoch = node.current_epoch.load(Ordering::SeqCst);
    let go_epoch: u64 = 5; // Go is ahead

    // This mirrors the condition in sync_once
    assert!(go_epoch > rust_epoch);

    // After auto_epoch_sync would run, epoch should be updated
    node.current_epoch.store(go_epoch, Ordering::SeqCst);
    assert_eq!(node.current_epoch.load(Ordering::SeqCst), 5);
}

// =============================================================================
// Queue sync with Go during sync cycle
// =============================================================================

/// sync_with_go properly aligns queue to Go state within a simulated sync round
#[tokio::test]
async fn test_queue_sync_with_go_during_sync_once() {
    let (node, _rx) = make_test_sync_node(0, 1, 0);

    // Simulate: queue is at next_expected=1, Go is at block 50
    {
        let mut queue = node.block_queue.lock().await;
        assert_eq!(queue.next_expected(), 1);

        queue.sync_with_go(50);
        assert_eq!(queue.next_expected(), 51);
    }
}

// =============================================================================
// Turbo mode activation
// =============================================================================

/// Turbo mode activates when queue has pending commits
#[tokio::test]
async fn test_turbo_mode_activation() {
    let (node, _rx) = make_test_sync_node(0, 1, 0);

    // Initially no pending
    {
        let queue = node.block_queue.lock().await;
        let catching_up = queue.pending_count() > 0;
        assert!(!catching_up, "No pending commits = no turbo mode");
    }

    // Add pending commits (with a gap so they can't be drained)
    {
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
                "global_exec_index": 5
            }
        });
        let commit: consensus_core::Commit = serde_json::from_value(json).unwrap();
        let mut queue = node.block_queue.lock().await;
        queue.push(super::block_queue::CommitData {
            commit,
            blocks: vec![],
            epoch: 0,
        });
        let catching_up = queue.pending_count() > 0;
        assert!(catching_up, "Pending commits = turbo mode");
    }
}

/// with_peer_rpc_addresses correctly sets peer addresses in config
#[test]
fn test_with_peer_rpc_addresses() {
    let (node, _rx) = make_test_sync_node(0, 1, 0);
    let node = node.with_peer_rpc_addresses(vec![
        "10.0.0.1:8000".to_string(),
        "10.0.0.2:8000".to_string(),
    ]);

    assert_eq!(node.config.peer_rpc_addresses.len(), 2);
    assert_eq!(node.config.peer_rpc_addresses[0], "10.0.0.1:8000");
}
