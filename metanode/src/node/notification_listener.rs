// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

//! Committee Notification Listener
//!
//! Listens for committee change notifications from Go Master via Unix socket.
//! When a validator registers or deregisters, Go pushes a notification which
//! triggers immediate committee check instead of waiting for 5-second polling.

use anyhow::Result;
use std::os::unix::net::UnixListener;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

/// Notification type for committee changes
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct CommitteeChangeNotification {
    pub block_number: u64,
    pub change_type: String, // "REGISTER" or "DEREGISTER"
    pub validator_address: String,
    pub new_validator_count: u64,
}

/// Committee notification listener that receives push notifications from Go
#[allow(dead_code)]
pub struct CommitteeNotificationListener {
    socket_path: String,
}

impl CommitteeNotificationListener {
    /// Create a new notification listener
    pub fn new(socket_path: &str) -> Self {
        Self {
            socket_path: socket_path.to_string(),
        }
    }

    /// Start listening for notifications and send them through channel
    pub async fn start(
        &self,
        tx: mpsc::Sender<CommitteeChangeNotification>,
    ) -> Result<tokio::task::JoinHandle<()>> {
        let socket_path = self.socket_path.clone();

        // Remove old socket file if exists
        let _ = std::fs::remove_file(&socket_path);

        // Create Unix listener
        let listener = UnixListener::bind(&socket_path)?;
        listener.set_nonblocking(true)?;

        info!("📡 [NOTIFICATION LISTENER] Listening on {}", socket_path);

        let handle = tokio::spawn(async move {
            // Convert to tokio listener
            let listener = match tokio::net::UnixListener::from_std(listener) {
                Ok(l) => l,
                Err(e) => {
                    error!("Failed to create tokio UnixListener: {}", e);
                    return;
                }
            };

            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let tx_clone = tx.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream, tx_clone).await {
                                warn!("[NOTIFICATION LISTENER] Connection error: {}", e);
                            }
                        });
                    }
                    Err(e) => {
                        error!("[NOTIFICATION LISTENER] Accept error: {}", e);
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                }
            }
        });

        Ok(handle)
    }
}

/// Handle incoming connection from Go
#[allow(dead_code)]
async fn handle_connection(
    mut stream: tokio::net::UnixStream,
    tx: mpsc::Sender<CommitteeChangeNotification>,
) -> Result<()> {
    info!("📡 [NOTIFICATION LISTENER] Go connected");

    let mut buffer = vec![0u8; 4096];

    loop {
        // Read length prefix (4 bytes, little endian)
        let mut len_buf = [0u8; 4];
        match stream.read_exact(&mut len_buf).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                info!("[NOTIFICATION LISTENER] Go disconnected");
                break;
            }
            Err(e) => {
                return Err(e.into());
            }
        }

        let msg_len = u32::from_le_bytes(len_buf) as usize;
        if msg_len > buffer.len() {
            buffer.resize(msg_len, 0);
        }

        // Read message
        stream.read_exact(&mut buffer[..msg_len]).await?;

        // Parse protobuf - for now just parse manually
        // Format: block_number(u64) + change_type_len(u8) + change_type + validator_addr_len(u8) + validator_addr + count(u64)
        // Simplified: just trigger notification
        let notification = CommitteeChangeNotification {
            block_number: 0,
            change_type: "UNKNOWN".to_string(),
            validator_address: "".to_string(),
            new_validator_count: 0,
        };

        info!("📢 [NOTIFICATION LISTENER] Received committee change notification!");

        if tx.send(notification).await.is_err() {
            warn!("[NOTIFICATION LISTENER] Channel closed, stopping");
            break;
        }
    }

    Ok(())
}

/// Start a notification-driven epoch monitor
/// This replaces the polling-based epoch monitor for faster response
#[allow(dead_code)]
pub fn start_notification_driven_monitor(
    socket_path: &str,
) -> (
    mpsc::Receiver<CommitteeChangeNotification>,
    tokio::task::JoinHandle<()>,
) {
    let (tx, rx) = mpsc::channel::<CommitteeChangeNotification>(100);

    let listener = CommitteeNotificationListener::new(socket_path);

    let handle = tokio::spawn(async move {
        match listener.start(tx).await {
            Ok(h) => {
                let _ = h.await;
            }
            Err(e) => {
                error!("[NOTIFICATION LISTENER] Failed to start: {}", e);
            }
        }
    });

    (rx, handle)
}
