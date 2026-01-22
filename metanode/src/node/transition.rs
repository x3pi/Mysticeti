// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use tracing::{info, warn}; // Removed unused error
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use crate::config::NodeConfig;
use crate::executor_client::ExecutorClient;
use crate::node::{ConsensusNode, NodeMode};
use consensus_core::{ConsensusAuthority, NetworkType, CommitConsumerArgs, SystemTransactionProvider}; // Removed unused ReconfigState, DefaultSystemTransactionProvider
use prometheus::Registry;
use tokio::time::sleep;
use crate::tx_submitter::TransactionSubmitter; // Added TransactionSubmitter trait
// Removed unused RocksDBStore import

pub async fn transition_to_epoch_from_system_tx(
    node: &mut ConsensusNode,
    new_epoch: u64,
    new_epoch_timestamp_ms: u64,
    commit_index: u32,
    config: &NodeConfig,
) -> Result<()> {
    if node.is_transitioning.swap(true, Ordering::SeqCst) {
        warn!("⚠️ Transition already in progress, skipping.");
        node.is_transitioning.store(false, Ordering::SeqCst);
        return Ok(());
    }

    info!("🔄 TRANSITION: epoch {} -> {}", node.current_epoch, new_epoch);
    sleep(Duration::from_millis(500)).await;

    // Reset flag guard
    struct Guard(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Guard { fn drop(&mut self) { if self.0.load(Ordering::SeqCst) { self.0.store(false, Ordering::SeqCst); } } }
    let _guard = Guard(node.is_transitioning.clone());

    node.close_user_certs().await;

    // Wait for processor
    let timeout_secs = if config.epoch_transition_optimization == "fast" { 5 } else { 10 };
    let _ = wait_for_commit_processor_completion(node, commit_index, timeout_secs).await;
    
    // Deterministic calc using commit_index from EndOfEpoch
    let last_global_exec_index_at_transition = crate::checkpoint::calculate_global_exec_index(
        node.current_epoch, commit_index, node.last_global_exec_index
    );
    
    info!("📊 Snapshot: Last block of epoch {}: {}", node.current_epoch, last_global_exec_index_at_transition);
    sleep(Duration::from_millis(100)).await;

    // Stop old authority
    if let Some(auth) = node.authority.take() {
        auth.stop().await;
    }

    // Update state
    node.current_epoch = new_epoch;
    node.current_commit_index.store(0, Ordering::SeqCst);
    
    // Sync with Go
    let executor_client = if config.executor_read_enabled {
        Arc::new(ExecutorClient::new(true, false, config.executor_send_socket_path.clone(), config.executor_receive_socket_path.clone()))
    } else { anyhow::bail!("Executor read disabled"); };
    
    let synced_index = if let Ok(go_last) = executor_client.get_last_block_number().await {
        if go_last >= last_global_exec_index_at_transition { go_last } else { last_global_exec_index_at_transition }
    } else {
        last_global_exec_index_at_transition
    };

    {
        let mut g = node.shared_last_global_exec_index.lock().await;
        *g = synced_index;
    }
    node.last_global_exec_index = synced_index;
    node.update_execution_lock_epoch(new_epoch).await;
    node.system_transaction_provider.update_epoch(new_epoch, new_epoch_timestamp_ms).await;

    // Prepare DB
    let db_path = node.storage_path.join("epochs").join(format!("epoch_{}", new_epoch)).join("consensus_db");
    if db_path.exists() { let _ = std::fs::remove_dir_all(&db_path); }
    std::fs::create_dir_all(&db_path)?;

    // Fetch committee
    let committee = crate::node::committee::build_committee_from_go_validators_at_block(&executor_client, synced_index).await?;
    node.check_and_update_node_mode(&committee, config).await?;

    let node_hostname = format!("node-{}", config.node_id);
    if let Some((idx, _)) = committee.authorities().find(|(_, a)| a.hostname == node_hostname) {
        node.own_index = idx;
    } else {
        node.own_index = consensus_config::AuthorityIndex::ZERO;
    }

    // Setup new processor
    let (commit_consumer, commit_receiver, mut block_receiver) = CommitConsumerArgs::new(0, 0);
    let epoch_cb = crate::commit_callbacks::create_epoch_transition_callback(node.epoch_transition_sender.clone());
    
    let exec_client_proc = if node.executor_commit_enabled {
        Some(Arc::new(ExecutorClient::new_with_initial_index(
            true, true, config.executor_send_socket_path.clone(), config.executor_receive_socket_path.clone(), synced_index + 1
        )))
    } else { None };

    let mut processor = crate::commit_processor::CommitProcessor::new(commit_receiver)
        .with_commit_index_callback(crate::commit_callbacks::create_commit_index_callback(node.current_commit_index.clone()))
        .with_global_exec_index_callback(crate::commit_callbacks::create_global_exec_index_callback(node.shared_last_global_exec_index.clone()))
        .with_shared_last_global_exec_index(node.shared_last_global_exec_index.clone())
        .with_epoch_info(new_epoch, synced_index)
        .with_is_transitioning(node.is_transitioning.clone())
        .with_pending_transactions_queue(node.pending_transactions_queue.clone())
        .with_epoch_transition_callback(epoch_cb);

    if let Some(c) = exec_client_proc {
        processor = processor.with_executor_client(c);
    }

    tokio::spawn(async move {
        let _ = processor.run().await;
    });
    tokio::spawn(async move { while block_receiver.recv().await.is_some() {} });

    // Start Authority
    if matches!(node.node_mode, NodeMode::Validator) {
        let mut params = node.parameters.clone();
        params.db_path = db_path;
        node.boot_counter += 1;
        
        node.authority = Some(ConsensusAuthority::start(
            NetworkType::Tonic, new_epoch_timestamp_ms, node.own_index, committee, params,
            node.protocol_config.clone(), node.protocol_keypair.clone(), node.network_keypair.clone(),
            node.clock.clone(), node.transaction_verifier.clone(), commit_consumer, Registry::new(),
            node.boot_counter, Some(node.system_transaction_provider.clone() as Arc<dyn SystemTransactionProvider>)
        ).await);
    }

    // Update proxy
    if let Some(auth) = &node.authority {
        if let Some(proxy) = &node.transaction_client_proxy {
            proxy.set_client(auth.transaction_client()).await;
        } else {
             node.transaction_client_proxy = Some(Arc::new(crate::tx_submitter::TransactionClientProxy::new(auth.transaction_client())));
        }
    } else {
        node.transaction_client_proxy = None;
    }

    sleep(Duration::from_millis(1000)).await;
    
    if test_consensus_readiness(node).await {
        info!("✅ Consensus ready.");
    }

    node.is_transitioning.store(false, Ordering::SeqCst);
    let _ = node.submit_queued_transactions().await;

    node.reset_reconfig_state().await;
    
    // Notify Go
    let _ = executor_client.advance_epoch(new_epoch, new_epoch_timestamp_ms).await;
    
    Ok(())
}

async fn wait_for_commit_processor_completion(node: &ConsensusNode, target: u32, max_wait: u64) -> Result<()> {
    let start = std::time::Instant::now();
    loop {
        let current = node.current_commit_index.load(Ordering::SeqCst);
        if current >= target { return Ok(()); }
        if start.elapsed().as_secs() >= max_wait { return Err(anyhow::anyhow!("Timeout")); }
        sleep(Duration::from_millis(100)).await;
    }
}


async fn test_consensus_readiness(node: &ConsensusNode) -> bool {
    if let Some(proxy) = &node.transaction_client_proxy {
        match proxy.submit(vec![vec![0u8; 64]]).await {
             Ok(_) => true,
             Err(_) => false,
        }
    } else { false }
}