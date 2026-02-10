// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

//! Block sync methods for ExecutorClient.
//! Used by validators to serve blocks and SyncOnly nodes to write blocks.

use anyhow::Result;
use prost::Message;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::info;

use super::proto;
use super::socket_stream::SocketStream;
use super::ExecutorClient;

impl ExecutorClient {
    /// Get a range of blocks from Go Master
    /// Used by validators to serve blocks to SyncOnly nodes
    pub async fn get_blocks_range(
        &self,
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<proto::BlockData>> {
        if !self.is_enabled() {
            return Err(anyhow::anyhow!("Executor client is not enabled"));
        }

        info!(
            "📤 [BLOCK SYNC] Requesting blocks {} to {} from Go Master",
            from_block, to_block
        );

        let request = proto::Request {
            payload: Some(proto::request::Payload::GetBlocksRangeRequest(
                proto::GetBlocksRangeRequest {
                    from_block,
                    to_block,
                },
            )),
        };

        let request_bytes = request.encode_to_vec();

        let mut stream = SocketStream::connect(&self.request_socket_address, 5).await?;

        let len_bytes = (request_bytes.len() as u32).to_le_bytes();
        stream.write_all(&len_bytes).await?;
        stream.write_all(&request_bytes).await?;
        stream.flush().await?;

        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await?;
        let response_len = u32::from_le_bytes(len_buf) as usize;

        let mut response_buf = vec![0u8; response_len];
        stream.read_exact(&mut response_buf).await?;

        let response: proto::Response = proto::Response::decode(&*response_buf)?;

        match response.payload {
            Some(proto::response::Payload::GetBlocksRangeResponse(resp)) => {
                if !resp.error.is_empty() {
                    return Err(anyhow::anyhow!("Go returned error: {}", resp.error));
                }
                info!(
                    "✅ [BLOCK SYNC] Received {} blocks from Go Master",
                    resp.count
                );
                Ok(resp.blocks)
            }
            Some(proto::response::Payload::Error(e)) => {
                Err(anyhow::anyhow!("Go Master error: {}", e))
            }
            _ => Err(anyhow::anyhow!("Unexpected response type from Go Master")),
        }
    }

    /// Sync blocks to local Go Master
    /// Used by SyncOnly nodes to write blocks received from peers
    pub async fn sync_blocks(&self, blocks: Vec<proto::BlockData>) -> Result<(u64, u64)> {
        if !self.is_enabled() {
            return Err(anyhow::anyhow!("Executor client is not enabled"));
        }

        if blocks.is_empty() {
            return Ok((0, 0));
        }

        let block_count = blocks.len();
        let first_block = blocks.first().map(|b| b.block_number).unwrap_or(0);
        let last_block = blocks.last().map(|b| b.block_number).unwrap_or(0);

        info!(
            "📤 [BLOCK SYNC] Syncing {} blocks ({} to {}) to Go Master",
            block_count, first_block, last_block
        );

        let request = proto::Request {
            payload: Some(proto::request::Payload::SyncBlocksRequest(
                proto::SyncBlocksRequest { blocks },
            )),
        };

        let request_bytes = request.encode_to_vec();

        let mut stream = SocketStream::connect(&self.request_socket_address, 5).await?;

        let len_bytes = (request_bytes.len() as u32).to_le_bytes();
        stream.write_all(&len_bytes).await?;
        stream.write_all(&request_bytes).await?;
        stream.flush().await?;

        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await?;
        let response_len = u32::from_le_bytes(len_buf) as usize;

        let mut response_buf = vec![0u8; response_len];
        stream.read_exact(&mut response_buf).await?;

        let response: proto::Response = proto::Response::decode(&*response_buf)?;

        match response.payload {
            Some(proto::response::Payload::SyncBlocksResponse(resp)) => {
                if !resp.error.is_empty() {
                    return Err(anyhow::anyhow!("Go returned error: {}", resp.error));
                }
                info!(
                    "✅ [BLOCK SYNC] Synced {} blocks (last: {})",
                    resp.synced_count, resp.last_synced_block
                );
                Ok((resp.synced_count, resp.last_synced_block))
            }
            Some(proto::response::Payload::Error(e)) => {
                Err(anyhow::anyhow!("Go Master error: {}", e))
            }
            _ => Err(anyhow::anyhow!("Unexpected response type from Go Master")),
        }
    }
}
