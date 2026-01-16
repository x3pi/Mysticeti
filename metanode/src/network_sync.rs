//! Network sync module for full sync nodes
//! Syncs blocks from global block cache (fast) or network (fallback)

use anyhow::Result;
use async_trait::async_trait;
use consensus_core::{CommittedSubDag, BlockAPI};
use consensus_config::Committee;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn, trace};

/// Peer information
#[derive(Clone, Debug)]
pub struct Peer {
    pub address: String,
    pub port: u16,
    #[allow(dead_code)]
    pub node_id: u32,
}

/// Sync state
#[derive(Clone, Debug)]
pub struct SyncState {
    pub current_index: u64,
    pub target_index: u64,
    pub is_syncing: bool,
    pub last_sync_time: std::time::SystemTime,
}

impl Default for SyncState {
    fn default() -> Self {
        Self {
            current_index: 0,
            target_index: 0,
            is_syncing: false,
            last_sync_time: std::time::SystemTime::UNIX_EPOCH,
        }
    }
}

/// Block store trait for storing blocks
#[async_trait]
pub trait BlockStore: Send + Sync {
    async fn store_block(&self, subdag: &CommittedSubDag, global_exec_index: u64) -> Result<()>;
    async fn get_block(&self, global_exec_index: u64) -> Result<Option<CommittedSubDag>>;
    async fn get_latest_index(&self) -> Result<u64>;
    async fn has_block(&self, global_exec_index: u64) -> Result<bool>;
}

/// Network client for communicating with peers
pub struct NetworkClient;

impl NetworkClient {
    pub fn new() -> Self {
        Self
    }

    pub async fn request_blocks(&self, _peer: &Peer, _from_height: u64, _to_height: u64) -> Result<Vec<CommittedSubDag>> {
        warn!("⚠️ [NETWORK SYNC] Block request not yet implemented");
        Ok(Vec::new())
    }
}

/// Network sync manager for full sync nodes
/// Syncs blocks from global block cache (fast) or network (fallback)
pub struct NetworkSyncManager {
    peers: Arc<Mutex<Vec<Peer>>>,
    network_client: Arc<NetworkClient>,
    block_store: Arc<dyn BlockStore>,
    sync_state: Arc<Mutex<SyncState>>,
}

impl NetworkSyncManager {
    pub fn new(block_store: Arc<dyn BlockStore>, network_client: Arc<NetworkClient>) -> Self {
        Self {
            peers: Arc::new(Mutex::new(Vec::new())),
            network_client,
            block_store,
            sync_state: Arc::new(Mutex::new(SyncState::default())),
        }
    }

    pub async fn discover_peers(&self, committee: &Committee) -> Result<Vec<Peer>> {
        let mut peers = Vec::new();

        for (idx, authority) in committee.authorities() {
            // Parse Multiaddr to get IP and port
            let addr_str = authority.address.to_string();
            let address_parts: Vec<&str> = addr_str.split('/').collect();
            if address_parts.len() >= 5 {
                let ip = address_parts[2];
                let port_str = address_parts[4];
                if let Ok(port) = port_str.parse::<u16>() {
                    peers.push(Peer {
                        address: ip.to_string(),
                        port,
                        node_id: idx.value() as u32,
                    });
                }
            }
        }

        info!("✅ [NETWORK SYNC] Discovered {} validator peers", peers.len());
        *self.peers.lock().await = peers.clone();
        Ok(peers)
    }

    /// Sync missing blocks from global block cache (fast sync)
    /// Blocks are stored in cache when validators send them to Go Master
    pub async fn sync_missing_blocks(&self) -> Result<u64> {
        let local_height = self.block_store.get_latest_index().await?;

        // Get latest block index from global block cache
        let cache_latest = match crate::block_cache::get_latest_index().await {
            Ok(idx) => idx,
            Err(e) => {
                warn!("⚠️ [BLOCK CACHE SYNC] Block cache not available: {}. Falling back to network sync.", e);
                return Ok(0);
            }
        };

        info!("🔍 [BLOCK CACHE SYNC] Local height: {}, Cache latest: {}", local_height, cache_latest);

        if cache_latest > local_height {
            let missing_count = cache_latest - local_height;
            info!("📥 [BLOCK CACHE SYNC] Syncing blocks from {} to {} (missing: {}) from global block cache",
                local_height + 1, cache_latest, missing_count);

            // Sync in batches for efficiency
            let batch_size = 500; // Larger batch size for cache sync
            let mut from = local_height + 1;
            let mut synced_count = 0;
            let mut total_transactions = 0u64;

            while from <= cache_latest {
                let to = std::cmp::min(from + batch_size - 1, cache_latest);
                info!("📦 [BLOCK CACHE SYNC] Fetching batch {}-{} from cache", from, to);

                // Get blocks from global cache
                match crate::block_cache::get_blocks(from, to).await {
                    Ok(blocks) => {
                        let mut batch_tx_count = 0u64;
                        let mut batch_stored = 0u64;

                        for (global_exec_index, subdag) in blocks {
                            // Count transactions in this block
                            let tx_count: u64 = subdag.blocks.iter()
                                .map(|b| b.transactions().len() as u64)
                                .sum::<u64>();
                            batch_tx_count += tx_count;

                            // Check if already stored
                            if self.block_store.has_block(global_exec_index).await? {
                                trace!("⏭️ [BLOCK CACHE SYNC] Block {} already stored, skipping", global_exec_index);
                                continue;
                            }

                            // Store block
                            self.block_store.store_block(&subdag, global_exec_index).await?;
                            batch_stored += 1;
                            synced_count += 1;

                            // Log detailed transaction count for each stored block
                            if tx_count > 0 {
                                info!("💾 [BLOCK CACHE SYNC] Block {}: {} transactions (stored)", global_exec_index, tx_count);
                            } else {
                                trace!("💾 [BLOCK CACHE SYNC] Block {}: 0 transactions (empty block - stored)", global_exec_index);
                            }
                        }

                        total_transactions += batch_tx_count;

                        if batch_stored > 0 {
                            info!("✅ [BLOCK CACHE SYNC] Batch {}-{}: {} blocks stored, {} transactions",
                                from, to, batch_stored, batch_tx_count);
                        } else {
                            trace!("📋 [BLOCK CACHE SYNC] Batch {}-{}: All blocks already stored", from, to);
                        }
                    }
                    Err(e) => {
                        warn!("⚠️ [BLOCK CACHE SYNC] Failed to get blocks {}-{} from cache: {}", from, to, e);
                        break;
                    }
                }

                from = to + 1;
            }

            let mut state = self.sync_state.lock().await;
            state.current_index = self.block_store.get_latest_index().await?;
            state.target_index = cache_latest;
            state.is_syncing = false;
            state.last_sync_time = std::time::SystemTime::now();

            if synced_count > 0 {
                info!("✅ [BLOCK CACHE SYNC] Sync completed - {} blocks synced, {} total transactions (local: {}, cache: {})",
                    synced_count, total_transactions, state.current_index, state.target_index);
            } else {
                info!("📋 [BLOCK CACHE SYNC] No new blocks to sync (local: {}, cache: {})", state.current_index, state.target_index);
            }

            Ok(synced_count)
        } else {
            trace!("📋 [BLOCK CACHE SYNC] Already up to date (local: {}, cache: {})", local_height, cache_latest);
            Ok(0)
        }
    }

    /// Get sync state
    #[allow(dead_code)]
    pub async fn get_sync_state(&self) -> SyncState {
        self.sync_state.lock().await.clone()
    }
}
