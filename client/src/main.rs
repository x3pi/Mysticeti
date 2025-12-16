// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing::info;

#[derive(Parser)]
#[command(name = "metanode-client")]
#[command(about = "MetaNode Client - Submit transactions to consensus nodes via RPC")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Submit a transaction to a running node via RPC
    Submit {
        /// Node RPC endpoint (e.g., http://127.0.0.1:10100)
        #[arg(short, long, default_value = "http://127.0.0.1:10100")]
        endpoint: String,
        /// Transaction data (hex encoded or text)
        #[arg(short, long)]
        data: Option<String>,
        /// Transaction file path
        #[arg(short, long)]
        file: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "metanode_client=info".into()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Submit { endpoint, data, file } => {
            info!("Submitting transaction to node at: {}", endpoint);

            // Get transaction data
            let tx_data = if let Some(data_str) = data {
                // Try to decode as hex first, then use as text
                hex::decode(&data_str)
                    .unwrap_or_else(|_| data_str.as_bytes().to_vec())
            } else if let Some(file_path) = file {
                std::fs::read(&file_path)
                    .map_err(|e| anyhow::anyhow!("Failed to read file {:?}: {}", file_path, e))?
            } else {
                anyhow::bail!("Either --data or --file must be provided");
            };

            info!("Transaction size: {} bytes", tx_data.len());

            // Submit via HTTP POST
            let url = format!("{}/submit", endpoint.trim_end_matches('/'));
            let client = reqwest::Client::new();
            let response = client
                .post(&url)
                .body(tx_data)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to send request: {}", e))?;

            if response.status().is_success() {
                let result: serde_json::Value = response.json().await?;
                println!("✅ Transaction submitted successfully!");
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                let error_text = response.text().await?;
                anyhow::bail!("Server error: {}", error_text);
            }
        }
    }

    Ok(())
}

