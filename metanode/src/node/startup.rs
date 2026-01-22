// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use std::sync::Arc;
use std::net::SocketAddr;
use tokio::sync::Mutex;
use tracing::{info, error, warn};
use mysten_metrics::start_prometheus_server;
use prometheus::Registry;

use crate::config::NodeConfig;
use crate::node::ConsensusNode;
use crate::network::rpc::RpcServer;
use crate::network::tx_socket_server::TxSocketServer;

/// Startup configuration and initialization
pub struct StartupConfig {
    pub node_config: NodeConfig,
    pub registry: Registry,
    pub registry_service: Option<Arc<mysten_metrics::RegistryService>>,
}

impl StartupConfig {
    pub fn new(node_config: NodeConfig, registry: Registry, registry_service: Option<Arc<mysten_metrics::RegistryService>>) -> Self {
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
}

impl InitializedNode {
    /// Initialize and start all node components
    pub async fn initialize(config: StartupConfig) -> Result<Self> {
        let StartupConfig { node_config, registry, registry_service } = config;

        // Start metrics server if enabled
        let _metrics_addr = if node_config.enable_metrics {
            let metrics_addr = SocketAddr::from(([127, 0, 0, 1], node_config.metrics_port));
            let _registry_service = start_prometheus_server(metrics_addr);
            info!("Metrics server started at http://127.0.0.1:{}/metrics", node_config.metrics_port);
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
            ConsensusNode::new_with_registry_and_service(
                node_config.clone(),
                registry,
            ).await?
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
            let rpc_server = RpcServer::with_node(tx_client.clone(), rpc_port, node_for_rpc.clone());
            rpc_server_handle = Some(tokio::spawn(async move {
                if let Err(e) = rpc_server.start().await {
                    error!("RPC server error: {}", e);
                }
            }));

            // Start Unix Domain Socket server for local IPC
            let socket_path = format!("/tmp/metanode-tx-{}.sock", node_config.node_id);
            let tx_client_uds = tx_client.clone();
            let node_for_uds = node.clone();
            let uds_server = TxSocketServer::with_node(
                socket_path.clone(),
                tx_client_uds,
                node_for_uds,
            );
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
        })
    }

    /// Run the main event loop
    pub async fn run_main_loop(self) -> Result<()> {
        // Chỉ cần wait signal, không cần loop sleep
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

        // 1. Flush remaining blocks to Go Master FIRST
        // Đảm bảo tất cả blocks đã commit được gửi sang Go trước khi shutdown consensus
        if let Ok(mutex) = Arc::try_unwrap(self.node.clone()) {
            let node = mutex.into_inner();
            node.flush_blocks_to_go_master().await?;
        } else {
            warn!("Could not unwrap node Arc for flushing, forcing shutdown...");
        }

        // 2. Shutdown consensus connections/tasks and node
        if let Ok(mutex) = Arc::try_unwrap(self.node) {
            let node = mutex.into_inner();
            node.shutdown().await?;
        } else {
            warn!("Could not unwrap node Arc, forcing shutdown...");
        }

        // 3. Shutdown servers LAST (sau khi đã flush hết blocks)
        // Dừng accept new requests từ clients
        if let Some(handle) = self.rpc_server_handle {
            handle.abort();
        }
        if let Some(handle) = self.uds_server_handle {
            handle.abort();
        }

        info!("Node stopped gracefully");
        Ok(())
    }
}