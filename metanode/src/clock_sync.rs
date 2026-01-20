// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use std::sync::Arc;
use std::time::{SystemTime, Duration};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio::process::Command;
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
    /// Production implementation (Option B): query system time sync (chrony).
    ///
    /// We intentionally do NOT implement a full NTP client inside the consensus process. Instead:
    /// - Use system chrony/ntpd to keep OS clock in sync (battle-tested)
    /// - Read current offset/health from the system and apply gating for epoch proposals
    pub async fn sync_with_ntp(&mut self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        if self.ntp_servers.is_empty() {
            warn!("No NTP servers configured, skipping sync");
            return Ok(());
        }

        // We keep ntp_servers in config for ops documentation, but the actual sync is done by chrony.
        // This method reads chrony's tracking offset (system time vs NTP time).
        let offset_ms = self.query_chrony_offset_ms().await?;
        self.last_sync_time = Some(SystemTime::now());
        self.clock_offset_ms = offset_ms;
        info!("Clock sync completed (chrony): offset={}ms", self.clock_offset_ms);
        Ok(())
    }

    /// Query chrony for current clock offset.
    /// Returns offset in milliseconds (positive = local clock ahead, negative = behind).
    async fn query_chrony_offset_ms(&self) -> Result<i64> {
        // `chronyc tracking` output examples:
        // - "System time     : 0.000000123 seconds fast of NTP time"
        // - "System time     : 0.000000123 seconds slow of NTP time"
        // - "Last offset     : +0.000001234 seconds"
        //
        // We prefer "System time" line if present because it's explicit about fast/slow.
        let output = Command::new("chronyc")
            .arg("tracking")
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to execute chronyc (is chrony installed?): {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("chronyc tracking failed: {}", stderr.trim());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        self.parse_chronyc_tracking_offset_ms(&stdout)
            .ok_or_else(|| anyhow::anyhow!("Unable to parse clock offset from `chronyc tracking` output"))
    }

    fn parse_chronyc_tracking_offset_ms(&self, tracking_output: &str) -> Option<i64> {
        // 1) Try "System time" line with fast/slow of NTP time.
        for line in tracking_output.lines() {
            let line = line.trim();
            if line.starts_with("System time") {
                // Extract "...: <value> seconds fast|slow of NTP time"
                // Split on ':' then parse first token as f64 seconds.
                let rhs = line.split_once(':')?.1.trim();
                let mut parts = rhs.split_whitespace();
                let secs_str = parts.next()?;
                let secs: f64 = secs_str.parse().ok()?;
                let direction = rhs;
                let ms = (secs * 1000.0).round() as i64;
                if direction.contains("fast of NTP time") {
                    return Some(ms);
                }
                if direction.contains("slow of NTP time") {
                    return Some(-ms);
                }
                // If direction unknown, fallthrough to "Last offset".
            }
        }

        // 2) Fallback: "Last offset : +0.000001 seconds"
        for line in tracking_output.lines() {
            let line = line.trim();
            if line.starts_with("Last offset") {
                let rhs = line.split_once(':')?.1.trim();
                let mut parts = rhs.split_whitespace();
                let signed_secs_str = parts.next()?; // may include '+'/'-'
                let secs: f64 = signed_secs_str.parse().ok()?;
                let ms = (secs * 1000.0).round() as i64;
                return Some(ms);
            }
        }

        None
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
        let drift_ms = self.clock_offset_ms.unsigned_abs();
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

