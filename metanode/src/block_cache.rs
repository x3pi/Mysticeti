//! Global block cache for fast full node synchronization
//! 
//! When validators commit blocks and send them to Go Master, they are also stored in this cache.
//! Full nodes can then quickly sync blocks from this cache instead of requesting from network.

use anyhow::Result;
use consensus_core::{CommittedSubDag, BlockAPI};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn, trace};

/// Global block cache - per-process in-memory (not shared between processes)
/// Validators write blocks here, full nodes read from here
static BLOCK_CACHE: tokio::sync::OnceCell<Arc<RwLock<BlockCache>>> = tokio::sync::OnceCell::const_new();

/// Block cache structure - in-memory only
struct BlockCache {
    /// Map from global_exec_index to CommittedSubDag
    blocks: HashMap<u64, CommittedSubDag>,
    /// Maximum number of blocks to keep in cache (to prevent memory issues)
    max_size: usize,
    /// Latest block index in cache
    latest_index: u64,
}

impl BlockCache {
    fn new(max_size: usize) -> Self {
        Self {
            blocks: HashMap::new(),
            max_size,
            latest_index: 0,
        }
    }

    /// Store a block in cache
    fn store_block(&mut self, global_exec_index: u64, subdag: CommittedSubDag) {
        // Count transactions in this block
        let tx_count: usize = subdag.blocks.iter()
            .map(|b| b.transactions().len())
            .sum();

        // Store in memory
        self.blocks.insert(global_exec_index, subdag);

        // Update latest index
        if global_exec_index > self.latest_index {
            self.latest_index = global_exec_index;
        }

        // Cleanup old blocks if cache is too large
        if self.blocks.len() > self.max_size {
            let oldest_index = self.latest_index.saturating_sub(self.max_size as u64);
            self.blocks.retain(|&idx, _| idx >= oldest_index);
            trace!("🧹 [BLOCK CACHE] Cleaned up old blocks, keeping blocks >= {}", oldest_index);
        }

        if tx_count > 0 {
            info!("💾 [BLOCK CACHE] Stored block {} with {} transactions (cache size: {}, latest: {})",
                global_exec_index, tx_count, self.blocks.len(), self.latest_index);
        } else {
            trace!("💾 [BLOCK CACHE] Stored block {} (no transactions) (cache size: {}, latest: {})",
                global_exec_index, self.blocks.len(), self.latest_index);
        }
    }

    /// Get multiple blocks from cache
    fn get_blocks(&self, from: u64, to: u64) -> Vec<(u64, CommittedSubDag)> {
        let mut result = Vec::new();
        for idx in from..=to {
            if let Some(subdag) = self.blocks.get(&idx) {
                result.push((idx, subdag.clone()));
            }
        }
        result
    }

    /// Get latest block index
    fn get_latest_index(&self) -> u64 {
        self.latest_index
    }
}

/// Initialize global block cache
pub fn init_block_cache(max_size: usize) {
    if BLOCK_CACHE.get().is_none() {
        let cache = Arc::new(RwLock::new(BlockCache::new(max_size)));
        let _ = BLOCK_CACHE.set(cache);
        info!("✅ [BLOCK CACHE] Initialized per-process block cache (max_size: {})", max_size);
    }
}

/// Get global block cache instance
fn get_cache() -> Result<Arc<RwLock<BlockCache>>> {
    BLOCK_CACHE.get()
        .ok_or_else(|| anyhow::anyhow!("Block cache not initialized"))
        .map(|c| c.clone())
}

/// Store a block in global cache
/// Called by validators when they send blocks to Go Master
pub async fn store_block(global_exec_index: u64, subdag: &CommittedSubDag) -> Result<()> {
    let cache = get_cache()?;
    let mut cache_guard = cache.write().await;
    cache_guard.store_block(global_exec_index, subdag.clone());
    Ok(())
}

/// Get multiple blocks from global cache
/// Called by full nodes to sync blocks in batches
pub async fn get_blocks(from: u64, to: u64) -> Result<Vec<(u64, CommittedSubDag)>> {
    let cache = get_cache()?;
    let cache_guard = cache.read().await;
    Ok(cache_guard.get_blocks(from, to))
}

/// Get latest block index from cache
pub async fn get_latest_index() -> Result<u64> {
    let cache = get_cache()?;
    let cache_guard = cache.read().await;
    Ok(cache_guard.get_latest_index())
}
