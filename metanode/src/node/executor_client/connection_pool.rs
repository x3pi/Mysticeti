// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

//! Connection pool for Go executor request sockets.
//!
//! Maintains multiple connections with round-robin selection and automatic
//! reconnection on failure. This allows parallel RPC queries to Go Master
//! without contention on a single mutex-guarded connection.

use anyhow::Result;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, trace, warn};

use super::socket_stream::{SocketAddress, SocketStream};

/// A pool of socket connections with round-robin selection and health checking.
#[allow(dead_code)]
pub struct ConnectionPool {
    /// Pool of connections, each individually mutex-guarded
    connections: Vec<Arc<Mutex<Option<SocketStream>>>>,
    /// Socket address to connect to
    address: SocketAddress,
    /// Round-robin counter for connection selection
    next_index: AtomicUsize,
    /// Pool size
    pool_size: usize,
    /// Connection timeout in seconds
    connect_timeout_secs: u64,
}

impl ConnectionPool {
    /// Create a new connection pool with the given size.
    pub fn new(address: SocketAddress, pool_size: usize, connect_timeout_secs: u64) -> Self {
        let connections = (0..pool_size).map(|_| Arc::new(Mutex::new(None))).collect();

        info!(
            "🏊 [CONN POOL] Created pool: address={}, size={}, timeout={}s",
            address.as_str(),
            pool_size,
            connect_timeout_secs
        );

        Self {
            connections,
            address,
            next_index: AtomicUsize::new(0),
            pool_size,
            connect_timeout_secs,
        }
    }

    /// Get a healthy connection from the pool using round-robin selection.
    /// If the selected connection is dead or missing, reconnects automatically.
    /// Returns the connection guard and the slot index.
    #[allow(dead_code)]
    pub async fn get_connection(
        &self,
    ) -> Result<(tokio::sync::MutexGuard<'_, Option<SocketStream>>, usize)> {
        let idx = self.next_index.fetch_add(1, Ordering::Relaxed) % self.pool_size;
        let mut guard = self.connections[idx].lock().await;

        // Check if connection exists and is healthy
        let needs_reconnect = match guard.as_mut() {
            Some(stream) => {
                match stream.writable().await {
                    Ok(_) => {
                        trace!(
                            "🏊 [CONN POOL] Reusing connection slot {} to {}",
                            idx,
                            self.address.as_str()
                        );
                        false // Connection is healthy
                    }
                    Err(e) => {
                        warn!(
                            "⚠️ [CONN POOL] Connection slot {} is dead: {}, reconnecting...",
                            idx, e
                        );
                        true
                    }
                }
            }
            None => true,
        };

        if needs_reconnect {
            *guard = None;
            self.connect_slot(&mut guard, idx).await?;
        }

        Ok((guard, idx))
    }

    /// Connect a specific pool slot with retry logic.
    async fn connect_slot(
        &self,
        guard: &mut tokio::sync::MutexGuard<'_, Option<SocketStream>>,
        slot: usize,
    ) -> Result<()> {
        const MAX_RETRIES: u32 = 3;
        const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(100);

        for attempt in 1..=MAX_RETRIES {
            match SocketStream::connect(&self.address, self.connect_timeout_secs).await {
                Ok(stream) => {
                    info!(
                        "🏊 [CONN POOL] ✅ Connected slot {} to {} (attempt {}/{})",
                        slot,
                        self.address.as_str(),
                        attempt,
                        MAX_RETRIES
                    );
                    **guard = Some(stream);
                    return Ok(());
                }
                Err(e) => {
                    if attempt < MAX_RETRIES {
                        warn!(
                            "⚠️ [CONN POOL] Slot {} connect failed (attempt {}/{}): {}, retrying...",
                            slot, attempt, MAX_RETRIES, e
                        );
                        tokio::time::sleep(RETRY_DELAY).await;
                    } else {
                        warn!(
                            "⚠️ [CONN POOL] Slot {} connect failed after {} attempts: {}",
                            slot, MAX_RETRIES, e
                        );
                        return Err(e.into());
                    }
                }
            }
        }
        unreachable!()
    }

    /// Reset all connections in the pool.
    pub async fn reset_all(&self) {
        info!(
            "🔄 [CONN POOL] Resetting all {} connections...",
            self.pool_size
        );
        for (i, conn) in self.connections.iter().enumerate() {
            let mut guard = conn.lock().await;
            if guard.is_some() {
                info!("🔌 [CONN POOL] Closing connection slot {}", i);
            }
            *guard = None;
        }
        info!("✅ [CONN POOL] All connections reset.");
    }

    /// Get the pool size.
    #[allow(dead_code)]
    pub fn size(&self) -> usize {
        self.pool_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_creation() {
        let addr = SocketAddress::Unix("/tmp/test_pool.sock".to_string());
        let pool = ConnectionPool::new(addr, 4, 30);
        assert_eq!(pool.size(), 4);
        assert_eq!(pool.connections.len(), 4);
    }

    #[test]
    fn test_round_robin_index() {
        let addr = SocketAddress::Unix("/tmp/test_rr.sock".to_string());
        let pool = ConnectionPool::new(addr, 3, 30);

        // Verify round-robin wraps around
        assert_eq!(pool.next_index.fetch_add(1, Ordering::Relaxed) % 3, 0);
        assert_eq!(pool.next_index.fetch_add(1, Ordering::Relaxed) % 3, 1);
        assert_eq!(pool.next_index.fetch_add(1, Ordering::Relaxed) % 3, 2);
        assert_eq!(pool.next_index.fetch_add(1, Ordering::Relaxed) % 3, 0); // wraps
    }
}
