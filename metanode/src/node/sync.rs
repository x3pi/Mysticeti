// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use tracing::{info, trace}; // Removed unused warn
use std::sync::Arc;
use std::time::Duration;
use crate::config::NodeConfig;
use crate::node::executor_client::ExecutorClient;
use crate::node::{ConsensusNode, NodeMode};

pub async fn start_sync_task(node: &mut ConsensusNode, config: &NodeConfig) -> Result<()> {
    if !matches!(node.node_mode, NodeMode::SyncOnly) || node.sync_task_handle.is_some() {
        return Ok(());
    }

    info!("🚀 [SYNC TASK] Starting sync task");
    let executor_client = Arc::new(ExecutorClient::new(
        true, false,
        config.executor_send_socket_path.clone(),
        config.executor_receive_socket_path.clone(),
    ));

    let last_global_exec_index = node.shared_last_global_exec_index.clone();
    let current_epoch = node.current_epoch;
    let node_mode = node.node_mode.clone();

    let task = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let _ = perform_sync_operation(&executor_client, &last_global_exec_index, current_epoch, &node_mode).await;
        }
    });

    node.sync_task_handle = Some(task);
    Ok(())
}

pub async fn stop_sync_task(node: &mut ConsensusNode) -> Result<()> {
    if let Some(handle) = node.sync_task_handle.take() {
        info!("🛑 [SYNC TASK] Stopping...");
        handle.abort();
        let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
    }
    Ok(())
}

async fn perform_sync_operation(
    executor_client: &Arc<ExecutorClient>,
    shared_index: &Arc<tokio::sync::Mutex<u64>>,
    _epoch: u64,
    node_mode: &NodeMode,
) -> Result<()> {
    if !matches!(node_mode, NodeMode::SyncOnly) { return Ok(()); }
    
    let go_last = executor_client.get_last_block_number().await?;
    let mut idx = shared_index.lock().await;
    if go_last > *idx {
        *idx = go_last;
        trace!("📈 [SYNC] Updated index to {}", go_last);
    }
    Ok(())
}