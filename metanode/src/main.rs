// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing::info;

mod config;
mod node;
mod transaction;
mod rpc;
mod commit_processor;
mod epoch_change;
mod epoch_change_bridge;
mod epoch_change_hook;
mod clock_sync;
mod tx_submitter;

use config::NodeConfig;
use node::ConsensusNode;
use tracing::error;
use std::sync::Arc;
use tokio::sync::Mutex;

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

            let node = Arc::new(Mutex::new(ConsensusNode::new(node_config.clone()).await?));
            
            // Start RPC server for client submissions
            let rpc_port = node_config.metrics_port + 1000; // RPC port = metrics_port + 1000
            let tx_client = { node.lock().await.transaction_submitter() };
            let rpc_server = rpc::RpcServer::new(tx_client, rpc_port);
            tokio::spawn(async move {
                if let Err(e) = rpc_server.start().await {
                    error!("RPC server error: {}", e);
                }
            });

            // Epoch transition coordinator:
            // If a proposal is quorum-approved AND commit-index barrier is passed,
            // call node.transition_to_epoch() to persist config + clear state + stop process.
            let node_for_epoch = node.clone();
            tokio::spawn(async move {
                let mut last_triggered: Option<Vec<u8>> = None;
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

                    // Read state without holding lock too long
                    let (manager, commit_index) = {
                        let guard = node_for_epoch.lock().await;
                        (guard.epoch_change_manager(), guard.current_commit_index())
                    };

                    let manager_guard = manager.read().await;
                    if let Some(proposal) = manager_guard.get_transition_ready_proposal(commit_index) {
                        let hash = manager_guard.hash_proposal(&proposal);
                        // Trigger once per proposal hash
                        if last_triggered.as_ref() == Some(&hash) {
                            continue;
                        }
                        last_triggered = Some(hash);
                        drop(manager_guard);

                        // Perform real transition (this will exit process on success)
                        let mut guard = node_for_epoch.lock().await;
                        if let Err(e) = guard.transition_to_epoch(&proposal, commit_index).await {
                            error!("Epoch transition failed: {}", e);
                        }
                    }
                }
            });
            
            info!("Consensus node started successfully");
            info!("RPC server available at http://127.0.0.1:{}", rpc_port);
            info!("Press Ctrl+C to stop the node");

            // Wait for shutdown signal
            tokio::signal::ctrl_c().await?;
            info!("Shutting down node...");
            
            // If epoch transition already stopped the process, we won't reach here.
            // Otherwise, shutdown normally.
            let node = Arc::try_unwrap(node)
                .ok()
                .map(|m| m.into_inner());
            if let Some(node) = node {
                node.shutdown().await?;
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

