// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing::{info, error, warn};

mod config;
mod node;
mod transaction;
mod rpc;
mod tx_socket_server;
mod executor_client;
mod commit_processor;
mod epoch_change;
mod epoch_change_bridge;
mod epoch_change_hook;
mod clock_sync;
mod tx_submitter;
mod checkpoint;
mod checkpoint_epoch_transition;
mod tx_hash;
mod sui_epoch_transition;

use config::NodeConfig;
use node::ConsensusNode;
use std::sync::Arc;
use std::net::SocketAddr;
use tokio::sync::Mutex;
use mysten_metrics::start_prometheus_server;

#[derive(Parser)]
#[command(name = "metanode")]
#[command(about = "MetaNode Consensus Engine - Multi-node consensus based on Sui Mysticeti")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start a consensus node
    Start {
        /// Path to node configuration file
        #[arg(short, long, default_value = "config/node.toml")]
        config: PathBuf,
    },
    /// Generate node configuration files for multiple nodes
    Generate {
        /// Number of nodes to generate
        #[arg(short, long, default_value = "4")]
        nodes: usize,
        /// Output directory for config files
        #[arg(short, long, default_value = "config")]
        output: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "metanode=info,consensus_core=info".into()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Start { config } => {
            info!("Starting MetaNode Consensus Engine...");
            info!("Loading configuration from: {:?}", config);

            let node_config = NodeConfig::load(&config)?;
            info!("Node ID: {}", node_config.node_id);
            info!("Network address: {}", node_config.network_address);

            // Start metrics server if enabled
            let registry_service = if node_config.enable_metrics {
                let metrics_addr = SocketAddr::from(([127, 0, 0, 1], node_config.metrics_port));
                let registry_service = start_prometheus_server(metrics_addr);
                info!("Metrics server started at http://127.0.0.1:{}/metrics", node_config.metrics_port);
                Some(registry_service)
            } else {
                info!("Metrics server is disabled (enable_metrics = false)");
                None
            };

            // Get registry from RegistryService if metrics is enabled, otherwise create a new one
            let registry = if let Some(ref rs) = registry_service {
                rs.default_registry()
            } else {
                prometheus::Registry::new()
            };

            let registry_service_arc = registry_service.as_ref().map(|rs| Arc::new(rs.clone()));
            
            // Create the ConsensusNode wrapped in a Mutex for safe concurrent access
            // We use Arc<Mutex<>> because multiple tasks (RPC, UDS) need access to the node
            let node = Arc::new(Mutex::new(
                ConsensusNode::new_with_registry_and_service(
                    node_config.clone(),
                    registry,
                    registry_service_arc,
                ).await?
            ));
            
            // Start RPC server for client submissions (HTTP)
            let rpc_port = node_config.metrics_port + 1000;
            let tx_client = { node.lock().await.transaction_submitter() };
            let node_for_rpc = node.clone();
            let rpc_server = rpc::RpcServer::with_node(tx_client.clone(), rpc_port, node_for_rpc.clone());
            tokio::spawn(async move {
                if let Err(e) = rpc_server.start().await {
                    error!("RPC server error: {}", e);
                }
            });

            // Start Unix Domain Socket server for local IPC
            let socket_path = format!("/tmp/metanode-tx-{}.sock", node_config.node_id);
            let tx_client_uds = tx_client.clone();
            let node_for_uds = node.clone();
            let uds_server = tx_socket_server::TxSocketServer::with_node(
                socket_path.clone(),
                tx_client_uds,
                node_for_uds,
            );
            tokio::spawn(async move {
                if let Err(e) = uds_server.start().await {
                    error!("UDS server error: {}", e);
                }
            });
            info!("Unix Domain Socket server available at {}", socket_path);
            
            info!("Consensus node started successfully");
            info!("RPC server available at http://127.0.0.1:{}", rpc_port);
            info!("Press Ctrl+C to stop the node");

            // --- MAIN LOOP ---
            // Simplified: Only monitor reconfiguration completion (epoch transitions now triggered automatically in commit processing)
            let mut last_processed_proposal_hash: Option<Vec<u8>> = None;

            loop {
                // 1. Check for shutdown signal or sleep
                let sleep = tokio::time::sleep(tokio::time::Duration::from_millis(2000)); // Less frequent checking
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {
                        info!("Received Ctrl+C, initiating shutdown...");
                        break;
                    }
                    _ = sleep => {
                        // Continue to monitor reconfiguration status
                    }
                }

                // 2. Check for completed reconfiguration (Sui-style: monitor what happened)
                let (proposal_opt, current_commit_index) = {
                    let guard = node.lock().await;
                    let manager = guard.epoch_change_manager();
                    let manager_guard = manager.read().await;
                    (
                        manager_guard.get_transition_ready_proposal(guard.current_commit_index()),
                        guard.current_commit_index()
                    )
                };

                // 3. Execute reconfiguration if proposal is ready (triggered automatically from commit processing)
                if let Some(proposal) = proposal_opt {
                    // Double check we haven't processed this already
                    let proposal_hash = {
                        let guard = node.lock().await;
                        guard.epoch_change_manager().read().await.hash_proposal(&proposal)
                    };

                    if last_processed_proposal_hash.as_ref() != Some(&proposal_hash) {
                        info!("🚀 MAIN LOOP: Executing reconfiguration for epoch {} -> {} (triggered automatically from commit processing)",
                              proposal.new_epoch - 1, proposal.new_epoch);

                        // Acquire lock for the actual transition
                        let mut guard = node.lock().await;

                        // Execute the reconfiguration (Sui-style integrated flow)
                        match guard.transition_to_epoch(&proposal, current_commit_index, &node_config).await {
                            Ok(_) => {
                                info!("✅ MAIN LOOP: Epoch reconfiguration completed successfully!");
                                last_processed_proposal_hash = Some(proposal_hash);
                            },
                            Err(e) => {
                                error!("❌ MAIN LOOP: Epoch reconfiguration failed: {}", e);
                                // Sleep a bit to avoid rapid retry loops on persistent errors
                                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                            }
                        }
                    }
                }
            }

            // --- SHUTDOWN CLEANUP ---
            info!("Shutting down node...");
            if let Ok(mutex) = Arc::try_unwrap(node) {
                let node = mutex.into_inner();
                node.shutdown().await?;
            } else {
                warn!("Could not unwrap node Arc, forcing shutdown...");
            }
            info!("Node stopped");
        }
        Commands::Generate { nodes, output } => {
            info!("Generating configuration for {} nodes...", nodes);
            NodeConfig::generate_multiple(nodes, &output).await?;
            info!("Configuration files generated in: {:?}", output);
        }
    }

    Ok(())
}