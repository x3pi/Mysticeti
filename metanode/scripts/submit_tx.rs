// Simple program to submit test transaction directly to consensus
use std::sync::Arc;
use metanode::config::NodeConfig;
use metanode::node::ConsensusNode;
use tokio;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🚀 Submitting test transaction to consensus...");

    // Load config for node 0
    let config = NodeConfig::load("config/node_0.toml")?;

    // Create consensus node (but don't start it fully)
    let node = ConsensusNode::new_with_registry(config, prometheus::Registry::new()).await?;

    // Get transaction submitter
    let submitter = node.transaction_submitter();

    // Create test transaction
    let tx_data = b"test_transaction_direct_submit";

    // Submit transaction
    match submitter.submit_transaction(vec![tx_data.to_vec()]).await {
        Ok(_) => println!("✅ Test transaction submitted successfully!"),
        Err(e) => println!("❌ Failed to submit transaction: {}", e),
    }

    Ok(())
}
