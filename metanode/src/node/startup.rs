// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use mysten_metrics::start_prometheus_server;
use prometheus::Registry;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::config::NodeConfig;
use crate::network::rpc::RpcServer;
use crate::network::tx_socket_server::TxSocketServer;
use crate::node::ConsensusNode;

/// Startup configuration and initialization
pub struct StartupConfig {
    pub node_config: NodeConfig,
    pub registry: Registry,
    pub registry_service: Option<Arc<mysten_metrics::RegistryService>>,
}

impl StartupConfig {
    pub fn new(
        node_config: NodeConfig,
        registry: Registry,
        registry_service: Option<Arc<mysten_metrics::RegistryService>>,
    ) -> Self {
        Self {
            node_config,
            registry,
            registry_service,
        }
    }
}

/// Represents the initialized node and its servers
pub struct InitializedNode {
    pub node: Arc<Mutex<ConsensusNode>>,
    pub rpc_server_handle: Option<tokio::task::JoinHandle<()>>,
    pub uds_server_handle: Option<tokio::task::JoinHandle<()>>,
    pub node_config: NodeConfig,
}

impl InitializedNode {
    /// Initialize and start all node components
    pub async fn initialize(config: StartupConfig) -> Result<Self> {
        let StartupConfig {
            node_config,
            registry,
            registry_service,
        } = config;

        // Start metrics server if enabled
        let _metrics_addr = if node_config.enable_metrics {
            let metrics_addr = SocketAddr::from(([127, 0, 0, 1], node_config.metrics_port));
            let _registry_service = start_prometheus_server(metrics_addr);
            info!(
                "Metrics server started at http://127.0.0.1:{}/metrics",
                node_config.metrics_port
            );
            Some(metrics_addr)
        } else {
            info!("Metrics server is disabled (enable_metrics = false)");
            None
        };

        // Get registry from RegistryService if metrics is enabled, otherwise create a new one
        let registry = if let Some(ref rs) = registry_service {
            rs.default_registry()
        } else {
            registry
        };

        // Create the ConsensusNode wrapped in a Mutex for safe concurrent access
        let node = Arc::new(Mutex::new(
            ConsensusNode::new_with_registry_and_service(node_config.clone(), registry).await?,
        ));

        // Register node in global registry for transition handler access
        crate::node::set_transition_handler_node(node.clone()).await;

        // Get transaction submitter for servers
        let tx_client = { node.lock().await.transaction_submitter() };

        let mut rpc_server_handle = None;
        let mut uds_server_handle = None;

        if let Some(tx_client) = tx_client {
            // Start RPC server for client submissions (HTTP) - only for validator nodes
            let rpc_port = node_config.metrics_port + 1000;
            let node_for_rpc = node.clone();
            let rpc_server =
                RpcServer::with_node(tx_client.clone(), rpc_port, node_for_rpc.clone());
            rpc_server_handle = Some(tokio::spawn(async move {
                if let Err(e) = rpc_server.start().await {
                    error!("RPC server error: {}", e);
                }
            }));

            // Start Unix Domain Socket server for local IPC
            let socket_path = format!("/tmp/metanode-tx-{}.sock", node_config.node_id);
            let tx_client_uds = tx_client.clone();
            let node_for_uds = node.clone();
            let uds_server =
                TxSocketServer::with_node(socket_path.clone(), tx_client_uds, node_for_uds);
            uds_server_handle = Some(tokio::spawn(async move {
                if let Err(e) = uds_server.start().await {
                    error!("UDS server error: {}", e);
                }
            }));
            info!("Unix Domain Socket server available at {}", socket_path);

            info!("Consensus node started successfully (validator mode)");
            info!("RPC server available at http://127.0.0.1:{}", rpc_port);
        } else {
            // Sync-only node: no RPC/UDS servers needed
            info!("Sync-only node started successfully (no transaction submission servers)");
        }

        Ok(Self {
            node,
            rpc_server_handle,
            uds_server_handle,
            node_config: node_config.clone(),
        })
    }

    /// Run the main event loop
    pub async fn run_main_loop(self) -> Result<()> {
        // --- [SYNC-BEFORE-CONSENSUS] ---
        // Block startup until we catch up with the network
        // This prevents the node from proposing blocks on a fork or when behind
        let catchup_manager = {
            let node_guard = self.node.lock().await;
            if let Some(client) = node_guard.executor_client.clone() {
                Some(crate::node::catchup::CatchupManager::new(
                    client,
                    self.node_config.peer_go_master_sockets.clone(),
                    self.node_config.executor_receive_socket_path.clone(),
                ))
            } else {
                warn!("⚠️ [STARTUP] No executor client available, skipping catchup check");
                None
            }
        };

        if let Some(cm) = catchup_manager {
            info!("⏳ [STARTUP] Verifying sync status before joining consensus...");
            let check_interval = std::time::Duration::from_secs(2);
            let timeout = std::time::Duration::from_secs(600); // 10 minutes timeout
            let start = std::time::Instant::now();

            // Force SyncingUp mode while waiting
            {
                let mut node_guard = self.node.lock().await;
                if node_guard.node_mode == crate::node::NodeMode::Validator {
                    info!("🔄 [STARTUP] Switching to SyncingUp mode while waiting for catchup");
                    node_guard.node_mode = crate::node::NodeMode::SyncingUp;
                }
            }

            loop {
                // Check timeout
                if start.elapsed() > timeout {
                    warn!("⚠️ [STARTUP] Catchup timed out after 600s. Forcing start (risky).");
                    break;
                }

                // Get current local state
                let (local_epoch, local_commit) = {
                    let node = self.node.lock().await;
                    // RocksDBStore read is expensive? No, we use in-memory counters if available?
                    // ConsensusNode has current_commit_index (AtomicU32) but we need u64 mapping?
                    // Let's use current_epoch.
                    // Commit index is trickier. Let's assume passed 0 for now as catchup checks Epoch primarily.
                    // But for Commit sync, we need local commit.
                    // Use commit_processor's tracked index?
                    // Node has `current_commit_index` (AtomicU32).
                    (
                        node.current_epoch,
                        node.current_commit_index
                            .load(std::sync::atomic::Ordering::Relaxed)
                            as u64,
                    )
                };

                match cm.check_sync_status(local_epoch, local_commit).await {
                    Ok(status) => {
                        if status.ready {
                            info!(
                                "✅ [STARTUP] Node is synced (gap={}). Joining consensus!",
                                status.commit_gap
                            );
                            break;
                        }

                        if status.epoch_match {
                            info!(
                                "🔄 [CATCHUP] Syncing blocks: LocalExec={}, Network={}, Gap={}",
                                status.go_last_block, status.network_block_height, status.block_gap
                            );
                        } else {
                            info!(
                                "🔄 [CATCHUP] Syncing epoch: Local={}, Network={}",
                                local_epoch, status.go_epoch
                            );
                        }
                    }
                    Err(e) => {
                        warn!("⚠️ [STARTUP] Failed to check sync status: {}", e);
                    }
                }

                tokio::time::sleep(check_interval).await;
            }

            // Restore Validator mode
            {
                let mut node_guard = self.node.lock().await;
                // Only switch if we intended to be a validator (based on committee)
                // We re-run check_and_update_node_mode to determine correct mode
                let _config = node_guard.protocol_config.clone(); // Need config... wait, protocol_config is strict.
                                                                  // We need NodeConfig. It is not stored in ConsensusNode except parts.
                                                                  // But we can just set to Validator if it was SyncingUp.
                                                                  // Actually, check_and_update_node_mode requires Committee and NodeConfig.
                                                                  // We don't have them easily here.
                                                                  // Simpler: Set to Validator.
                if node_guard.node_mode == crate::node::NodeMode::SyncingUp {
                    info!("✅ [STARTUP] Switching to Validator mode");
                    node_guard.node_mode = crate::node::NodeMode::Validator;
                }
            }
        }

        info!("Press Ctrl+C to stop the node");
        tokio::signal::ctrl_c().await?;
        info!("Received Ctrl+C, initiating shutdown...");
        self.shutdown().await
    }

    /// Shutdown the node and all servers
    /// Thứ tự tắt được tối ưu để đảm bảo data integrity:
    /// 1. Shutdown consensus connections/tasks (để tránh new blocks)
    /// 2. Flush remaining blocks to Go Master (đảm bảo không mất blocks)
    /// 3. Shutdown servers (dừng accept new requests)
    pub async fn shutdown(self) -> Result<()> {
        info!("Shutting down node...");

        // 1. Shutdown servers FIRST (stop accepting new requests)
        if let Some(handle) = self.rpc_server_handle {
            handle.abort();
            // Optional: wait for it to finish
            let _ = handle.await;
        }
        if let Some(handle) = self.uds_server_handle {
            handle.abort();
            let _ = handle.await;
        }

        // 2. Lock node and perform shutdown sequence
        // We use lock() instead of try_unwrap() because the node is shared (e.g. global registry)
        let mut node = self.node.lock().await;

        // 3. Flush remaining blocks to Go Master
        if let Err(e) = node.flush_blocks_to_go_master().await {
            warn!("⚠️ [SHUTDOWN] Failed to flush blocks: {}", e);
        }

        // 4. Shutdown consensus connections/tasks
        if let Err(e) = node.shutdown().await {
            error!("❌ [SHUTDOWN] Error during consensus shutdown: {}", e);
        }

        info!("Node stopped gracefully");
        Ok(())
    }
}
