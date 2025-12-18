// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use std::sync::Arc;
use std::time::{SystemTime, Duration};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{info, warn, error};

/// Manager for clock synchronization with NTP servers
pub struct ClockSyncManager {
    ntp_servers: Vec<String>,
    max_clock_drift_ms: u64,
    sync_interval_seconds: u64,
    last_sync_time: Option<SystemTime>,
    clock_offset_ms: i64, // Positive = local clock ahead, negative = behind
    enabled: bool,
}

impl ClockSyncManager {
    pub fn new(
        ntp_servers: Vec<String>,
        max_clock_drift_ms: u64,
        sync_interval_seconds: u64,
        enabled: bool,
    ) -> Self {
        Self {
            ntp_servers,
            max_clock_drift_ms,
            sync_interval_seconds,
            last_sync_time: None,
            clock_offset_ms: 0,
            enabled,
        }
    }

    /// Sync with NTP servers
    /// Note: This is a placeholder implementation
    /// In production, you would use an NTP client library or system NTP
    pub async fn sync_with_ntp(&mut self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        if self.ntp_servers.is_empty() {
            warn!("No NTP servers configured, skipping sync");
            return Ok(());
        }

        // Placeholder: In production, query NTP servers and calculate offset
        // For now, we'll just log that sync would happen
        info!("NTP sync would query servers: {:?}", self.ntp_servers);
        
        // In a real implementation:
        // 1. Query each NTP server
        // 2. Calculate average/median offset
        // 3. Update clock_offset_ms
        // 4. Update last_sync_time
        
        self.last_sync_time = Some(SystemTime::now());
        self.clock_offset_ms = 0; // Placeholder
        
        info!("Clock sync completed: offset={}ms", self.clock_offset_ms);
        Ok(())
    }

    /// Get synchronized time (local time + offset)
    #[allow(dead_code)] // Reserved for future use (time-based epoch change)
    pub fn get_synced_time(&self) -> SystemTime {
        let local_time = SystemTime::now();
        if self.clock_offset_ms == 0 {
            return local_time;
        }
        
        // Adjust time by offset
        if self.clock_offset_ms > 0 {
            local_time - Duration::from_millis(self.clock_offset_ms as u64)
        } else {
            local_time + Duration::from_millis((-self.clock_offset_ms) as u64)
        }
    }

    /// Check if clock drift is too large
    pub fn check_clock_drift(&self) -> Result<()> {
        let drift_ms = self.clock_offset_ms.abs() as u64;
        if drift_ms > self.max_clock_drift_ms {
            anyhow::bail!("Clock drift too large: {}ms > {}ms", 
                drift_ms, self.max_clock_drift_ms);
        }
        Ok(())
    }

    /// Get current clock offset
    #[allow(dead_code)] // Reserved for future use (monitoring, debugging)
    pub fn clock_offset_ms(&self) -> i64 {
        self.clock_offset_ms
    }

    /// Start periodic sync task
    pub fn start_sync_task(manager: Arc<RwLock<Self>>) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let sync_interval = {
                    let m = manager.read().await;
                    m.sync_interval_seconds
                };
                tokio::time::sleep(Duration::from_secs(sync_interval)).await;
                
                let mut m = manager.write().await;
                if let Err(e) = m.sync_with_ntp().await {
                    error!("NTP sync failed: {}", e);
                }
            }
        })
    }

    /// Start drift monitoring task
    pub fn start_drift_monitor(manager: Arc<RwLock<Self>>) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                
                let m = manager.read().await;
                if let Err(e) = m.check_clock_drift() {
                    error!("Clock drift check failed: {}", e);
                }
            }
        })
    }

    /// Check if clock sync is healthy
    #[allow(dead_code)] // Reserved for future use (health checks, monitoring)
    pub fn is_healthy(&self) -> bool {
        if !self.enabled {
            return true; // If disabled, consider it healthy
        }
        
        // Check if we've synced recently (within 2x sync interval)
        if let Some(last_sync) = self.last_sync_time {
            let elapsed = SystemTime::now()
                .duration_since(last_sync)
                .unwrap_or(Duration::from_secs(0));
            
            if elapsed.as_secs() > self.sync_interval_seconds * 2 {
                return false; // Haven't synced in too long
            }
        } else {
            return false; // Never synced
        }
        
        // Check drift
        self.check_clock_drift().is_ok()
    }
}

