// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

//! Peer RPC module — WAN-based block synchronization and peer discovery.
//!
//! Submodules:
//! - `types` — Shared request/response types  
//! - `server` — PeerRpcServer (HTTP endpoints for peer queries)
//! - `client` — Client functions for querying remote peers

mod client;
mod server;
mod types;

// Re-export all public items to maintain backwards-compatible paths
// (e.g. crate::network::peer_rpc::PeerRpcServer still works)
pub use client::{
    forward_transaction_to_validators, query_peer_epoch_boundary_data, query_peer_epochs_network,
    query_peer_info,
};
pub use server::PeerRpcServer;
#[allow(unused_imports)]
pub use types::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_peer_info_response_serialization() {
        let info = PeerInfoResponse {
            node_id: 4,
            epoch: 1,
            last_block: 4500,
            network_address: "127.0.0.1:9004".to_string(),
            timestamp_ms: 1234567890000,
        };

        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"node_id\":4"));
        assert!(json.contains("\"epoch\":1"));
        assert!(json.contains("\"last_block\":4500"));
    }
}
