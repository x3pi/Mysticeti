// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

//! Persistence helpers for crash recovery of executor state.

use anyhow::Result;
use std::path::Path;
use tracing::{info, trace, warn};

/// Write uvarint to buffer (Go's binary.ReadUvarint format)
pub fn write_uvarint(buf: &mut Vec<u8>, mut value: u64) -> Result<()> {
    use std::io::Write;
    loop {
        let mut b = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            b |= 0x80;
        }
        Write::write_all(buf, &[b])?;
        if value == 0 {
            break;
        }
    }
    Ok(())
}

// Note: Executor is now configured via executor_enabled field in node_X.toml
// This function is kept for backward compatibility but is no longer used
#[allow(dead_code)]
pub fn is_executor_enabled(config_dir: &Path) -> bool {
    let config_file = config_dir.join("enable_executor.toml");
    config_file.exists()
}

// Persist last successfully sent index AND commit_index to file for crash recovery
// Uses atomic write (temp file + rename) to prevent corruption
pub async fn persist_last_sent_index(
    storage_path: &Path,
    index: u64,
    commit_index: u32,
) -> Result<()> {
    use tokio::io::AsyncWriteExt;

    let persist_dir = storage_path.join("executor_state");
    std::fs::create_dir_all(&persist_dir)?;

    let temp_path = persist_dir.join("last_sent_index.tmp");
    let final_path = persist_dir.join("last_sent_index.bin");

    // Write to temp file
    let mut file = tokio::fs::File::create(&temp_path).await?;
    // Format: [global_exec_index: u64][commit_index: u32]
    file.write_all(&index.to_le_bytes()).await?;
    file.write_all(&commit_index.to_le_bytes()).await?;
    file.flush().await?;
    file.sync_all().await?;
    drop(file);

    // Atomic rename
    std::fs::rename(&temp_path, &final_path)?;

    trace!(
        "💾 [PERSIST] Saved last_sent_index={}, commit_index={} to {:?}",
        index,
        commit_index,
        final_path
    );
    Ok(())
}

// Persist the last block number retrieved from Go for crash recovery
pub async fn persist_last_block_number(storage_path: &Path, block_number: u64) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let persist_dir = storage_path.join("executor_state");
    std::fs::create_dir_all(&persist_dir)?;
    let temp_path = persist_dir.join("last_block_number.tmp");
    let final_path = persist_dir.join("last_block_number.bin");
    let mut file = tokio::fs::File::create(&temp_path).await?;
    file.write_all(&block_number.to_le_bytes()).await?;
    file.flush().await?;
    file.sync_all().await?;
    drop(file);
    std::fs::rename(&temp_path, &final_path)?;
    trace!(
        "💾 [PERSIST] Saved last_block_number={} to {:?}",
        block_number,
        final_path
    );
    Ok(())
}

// Read persisted last block number, if any
pub async fn read_last_block_number(storage_path: &Path) -> Result<u64> {
    use tokio::io::AsyncReadExt;
    let persist_dir = storage_path.join("executor_state");
    let final_path = persist_dir.join("last_block_number.bin");
    let mut file = tokio::fs::File::open(&final_path).await?;
    let mut buf = [0u8; 8];
    file.read_exact(&mut buf).await?;
    Ok(u64::from_le_bytes(buf))
}

/// Load persisted last sent index from file
/// Returns None if file doesn't exist or is corrupted
/// Returns (global_exec_index, commit_index)
pub fn load_persisted_last_index(storage_path: &Path) -> Option<(u64, u32)> {
    let persist_path = storage_path
        .join("executor_state")
        .join("last_sent_index.bin");

    if !persist_path.exists() {
        return None;
    }

    match std::fs::read(&persist_path) {
        Ok(bytes) => {
            if bytes.len() == 12 {
                // New format: u64 + u32
                let index = u64::from_le_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                ]);
                let commit = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
                info!(
                    "📂 [PERSIST] Loaded persisted last_sent_index={}, commit_index={} from {:?}",
                    index, commit, persist_path
                );
                Some((index, commit))
            } else if bytes.len() == 8 {
                // Legacy format: u64 only (commit_index assumed 0 or unknown)
                let index = u64::from_le_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                ]);
                warn!(
                    "⚠️ [PERSIST] Legacy format detected (only u64). Defaulting commit_index to 0."
                );
                Some((index, 0))
            } else {
                warn!(
                    "⚠️ [PERSIST] Corrupted last_sent_index file: {} bytes (expected 8 or 12)",
                    bytes.len()
                );
                None
            }
        }
        Err(e) => {
            warn!("⚠️ [PERSIST] Failed to read last_sent_index: {}", e);
            None
        }
    }
}
