// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

//! ConsensusNode constructors.
//!
//! Contains the struct definition and all `new*` constructors for ConsensusNode.
//! Extracted from `node/mod.rs` for readability — the main constructor is ~750 lines.

use crate::consensus::clock_sync::ClockSyncManager;
use crate::types::transaction::NoopTransactionVerifier;
use anyhow::Result;
use consensus_config::AuthorityIndex;
use consensus_core::{
    Clock, CommitConsumerArgs, ConsensusAuthority, DefaultSystemTransactionProvider, NetworkType,
    ReconfigState, SystemTransactionProvider,
};
use meta_protocol_config::ProtocolConfig;
use prometheus::Registry;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::config::NodeConfig;
use crate::node::executor_client::ExecutorClient;
use crate::node::tx_submitter::TransactionClientProxy;

use super::{detect_local_epoch, ConsensusNode, NodeMode};
use super::{epoch_monitor, epoch_transition_manager, recovery};

impl ConsensusNode {
    #[allow(dead_code)]
    pub async fn new(config: NodeConfig) -> Result<Self> {
        Self::new_with_registry(config, Registry::new()).await
    }

    #[allow(dead_code)]
    pub async fn new_with_registry(config: NodeConfig, registry: Registry) -> Result<Self> {
        Self::new_with_registry_and_service(config, registry).await
    }

    pub async fn new_with_registry_and_service(
        config: NodeConfig,
        registry: Registry,
    ) -> Result<Self> {
        info!("Initializing consensus node {}...", config.node_id);
        info!("🚀 [STARTUP] Loading latest block, epoch and committee from Go state...");

        let executor_client = Arc::new(ExecutorClient::new(
            true,
            false,
            config.executor_send_socket_path.clone(),
            config.executor_receive_socket_path.clone(),
            Some(config.storage_path.clone()), // Enable persistence for readout client
        ));

        let latest_block_number = match executor_client.get_last_block_number().await {
            Ok(n) => n,
            Err(e) => {
                warn!("⚠️ [STARTUP] Failed to fetch latest block from Go: {}. Attempting to read persisted value.", e);
                // Fallback to persisted last block number
                super::executor_client::read_last_block_number(&config.storage_path)
                    .await
                    .unwrap_or(0)
            }
        };

        // PEER EPOCH DISCOVERY: Query TCP peers to get correct epoch
        let (go_epoch, peer_last_block, best_socket) = if !config.peer_rpc_addresses.is_empty() {
            use crate::network::peer_rpc::query_peer_epochs_network;
            match query_peer_epochs_network(&config.peer_rpc_addresses).await {
                Ok(result) => {
                    info!(
                        "✅ [PEER EPOCH] Using epoch {} from TCP peer discovery (peer: {})",
                        result.0, result.2
                    );
                    (
                        result.0,
                        result.1,
                        config.executor_receive_socket_path.clone(),
                    )
                }
                Err(e) => {
                    warn!("⚠️ [PEER EPOCH] Failed to query TCP peers, falling back to local Go Master: {}", e);
                    let epoch = executor_client
                        .get_current_epoch()
                        .await
                        .map_err(|e| anyhow::anyhow!("Failed to fetch epoch: {}", e))?;
                    (
                        epoch,
                        latest_block_number,
                        config.executor_receive_socket_path.clone(),
                    )
                }
            }
        } else {
            // No peer addresses configured, use local Go Master
            let epoch = executor_client
                .get_current_epoch()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to fetch current epoch from Go: {}", e))?;
            (
                epoch,
                latest_block_number,
                config.executor_receive_socket_path.clone(),
            )
        };

        info!(
            "📊 [STARTUP] Go State: Block {}, Epoch {} (peer_block={})",
            latest_block_number, go_epoch, peer_last_block
        );

        // CATCHUP: Check if we need to sync epoch from local storage
        let storage_path = config.storage_path.clone();
        let local_epoch = detect_local_epoch(&storage_path);

        let current_epoch = if local_epoch < go_epoch {
            // Epoch mismatch detected - need to sync
            warn!(
                "🔄 [CATCHUP] Epoch mismatch detected: local={}, go={}. Syncing to epoch {}.",
                local_epoch, go_epoch, go_epoch
            );

            // Clear old epoch data that is stale (unless archive mode)
            if config.epochs_to_keep > 0 {
                for epoch in local_epoch..go_epoch {
                    let epoch_path = storage_path.join("epochs").join(format!("epoch_{}", epoch));
                    if epoch_path.exists() {
                        info!("🗑️ [CATCHUP] Clearing stale epoch {} data", epoch);
                        if let Err(e) = std::fs::remove_dir_all(&epoch_path) {
                            warn!("⚠️ [CATCHUP] Failed to clear epoch {} data: {}", epoch, e);
                        }
                    }
                }
            } else {
                info!("📦 [CATCHUP] Archive mode (epochs_to_keep=0): keeping all epoch data");
            }

            go_epoch
        } else if local_epoch > go_epoch {
            // Local epoch is higher than network - likely we are on a stale "future" chain (e.g. network reset)
            warn!(
                "🚨 [CATCHUP] Local epoch {} is AHEAD of network epoch {}! Detect stale chain.",
                local_epoch, go_epoch
            );
            warn!("🗑️ [CATCHUP] Clearing ALL local epochs to resync with network.");

            if let Ok(entries) = std::fs::read_dir(&storage_path.join("epochs")) {
                for entry in entries.flatten() {
                    if let Ok(path) = entry.path().canonicalize() {
                        info!("🗑️ [CATCHUP] Removing {:?}", path);
                        let _ = std::fs::remove_dir_all(path);
                    }
                }
            }
            go_epoch
        } else {
            go_epoch
        };

        info!(
            "📊 [STARTUP] Using epoch {} (synced with Go)",
            current_epoch
        );

        // ... existing committee loading code (unchanged) ...
        // CRITICAL: Fetch validators from the Go Master that has the correct epoch
        // If we synced epoch from a peer, we must get validators+timestamp from that same peer
        // to ensure genesis block hash matches the network
        let peer_executor_client = if best_socket != config.executor_receive_socket_path {
            info!(
                "🔄 [PEER SYNC] Using peer Go Master {} for validators (has correct epoch {})",
                best_socket, go_epoch
            );
            Arc::new(ExecutorClient::new(
                true,
                false,
                String::new(), // Send socket not needed for read
                best_socket.clone(),
                None, // Read-only client, no persistence needed
            ))
        } else {
            executor_client.clone()
        };

        // CRITICAL FIX: Use epoch boundary data instead of current validators
        // This ensures new validators only become active AFTER epoch transition.
        // get_validators_at_block() returns current state (includes newly registered validators)
        // get_epoch_boundary_data() returns validators at epoch boundary (actual committee)
        //
        // FALLBACK LOGIC: If local Go doesn't have epoch boundary for network epoch,
        // fall back to local Go's epoch (for late-joining nodes that need to sync first)
        let (current_epoch, epoch_timestamp_ms, boundary_block, validators) =
            match peer_executor_client
                .get_epoch_boundary_data(current_epoch)
                .await
            {
                Ok((epoch, timestamp, boundary, vals)) => {
                    info!(
                        "✅ [STARTUP] Got epoch boundary data for epoch {} from Go",
                        epoch
                    );
                    (epoch, timestamp, boundary, vals)
                }
                Err(e) => {
                    warn!(
                        "⚠️ [STARTUP] Failed to get epoch boundary for epoch {}: {}. Falling back to local Go's epoch.",
                        current_epoch, e
                    );
                    // Fall back to local Go's current epoch (which should have boundary data)
                    let local_epoch = executor_client.get_current_epoch().await.unwrap_or(0);
                    info!(
                        "📊 [STARTUP] Falling back to local Go epoch {} (network epoch was {})",
                        local_epoch, current_epoch
                    );

                    match executor_client.get_epoch_boundary_data(local_epoch).await {
                        Ok((epoch, timestamp, boundary, vals)) => {
                            info!(
                                "✅ [STARTUP] Got epoch boundary data for local epoch {}",
                                epoch
                            );
                            (epoch, timestamp, boundary, vals)
                        }
                        Err(e2) => {
                            // Last resort: use epoch 0 with genesis parameters
                            warn!(
                                "⚠️ [STARTUP] No epoch boundary available (local epoch {} error: {}). Using epoch 0 genesis.",
                                local_epoch, e2
                            );
                            // Try to get validators at block 0 for genesis
                            let (genesis_validators, _genesis_epoch) = executor_client
                                .get_validators_at_block(0)
                                .await
                                .map_err(|e| {
                                    anyhow::anyhow!("Failed to fetch genesis validators: {}", e)
                                })?;
                            (0u64, 0u64, 0u64, genesis_validators)
                        }
                    }
                }
            };

        info!(
            "📊 [STARTUP] Using epoch boundary data: epoch={}, boundary_block={}, epoch_timestamp={}ms, validators={}",
            current_epoch, boundary_block, epoch_timestamp_ms, validators.len()
        );

        if validators.is_empty() {
            anyhow::bail!("Go state returned empty validators list at epoch boundary");
        }

        // Filter validators for single node debug if needed
        let validators_to_use = if std::env::var("SINGLE_NODE_DEBUG").is_ok() {
            info!("🔧 SINGLE_NODE_DEBUG: Using only node 0");
            validators
                .into_iter()
                .filter(|v| v.name == "node-0")
                .collect::<Vec<_>>()
        } else {
            validators
        };

        // Use helper from committee.rs - also extract eth_addresses for leader lookup
        let (committee, validator_eth_addresses) =
            super::committee::build_committee_with_eth_addresses(validators_to_use, current_epoch)?;
        info!(
            "✅ Loaded committee with {} authorities and {} eth_addresses (from epoch boundary)",
            committee.size(),
            validator_eth_addresses.len()
        );

        // EXECUTION INDEX SYNC
        // GO-AUTHORITATIVE STRATEGY with local persistence fallback
        // Priority: Go's last_block_number (authoritative) with local persistence as safety net

        // 1. Get Local Go Tip
        let local_go_block = if config.executor_read_enabled {
            executor_client.get_last_block_number().await.unwrap_or(0)
        } else {
            0
        };

        // 2. Get Persisted Tip + Commit Index (for base calculation)
        let (persisted_index, persisted_commit) = if config.executor_read_enabled {
            super::executor_client::load_persisted_last_index(&storage_path).unwrap_or((0, 0))
        } else {
            (0, 0)
        };

        // 3. Get Peer Tip (if available)
        let peer_last_block = if config.executor_read_enabled
            && best_socket != config.executor_receive_socket_path
            && peer_last_block > 0
        {
            peer_last_block
        } else {
            0
        };

        // DECISION LOGIC: Determine "Effective Tip" (last_global_exec_index)
        // This is used for ExecutorClient's next_expected (Replay Protection)
        let last_global_exec_index = if config.executor_read_enabled {
            if peer_last_block > 0 {
                info!(
                    "📊 [STARTUP] Sync Check: LocalGo={}, Peer={}, Persisted=({}, commit={}) (from {})",
                    local_go_block, peer_last_block, persisted_index, persisted_commit, best_socket
                );

                // PRODUCTION SAFETY: Cross-validate all sources
                let sources_match = local_go_block == peer_last_block
                    || local_go_block.abs_diff(peer_last_block) <= 5;
                if !sources_match {
                    warn!("⚠️ [STARTUP] INDEX DISCREPANCY DETECTED:");
                    warn!(
                        "   LocalGo={}, Peer={}, Persisted={}",
                        local_go_block, peer_last_block, persisted_index
                    );
                    warn!("   This may indicate network partition or stale data.");
                }

                if local_go_block > peer_last_block + 5 {
                    warn!("🚨 [STARTUP] STALE CHAIN DETECTED: Local ({}) is ahead of Peer ({})! Forcing resync from Peer.", 
                           local_go_block, peer_last_block);
                    // Force use of Peer Index to abandon local fork
                    peer_last_block
                } else if local_go_block < peer_last_block.saturating_sub(5) {
                    // LOCAL IS BEHIND PEERS
                    // CRITICAL FIX: Use local_go_block as the baseline to ensure recovery backfills missing blocks.
                    // If we jump to peer_last_block, we skip local recovery and Go Master will buffer forever.
                    info!(
                        "ℹ️ [STARTUP] Local Go Master ({}) is behind Peer ({}) by {} blocks. Using Local {} to trigger recovery/backfill.",
                        local_go_block, peer_last_block, peer_last_block - local_go_block, local_go_block
                    );
                    local_go_block
                } else {
                    // Close enough or local is slightly ahead/behind within tolerance
                    // Trust Local Go as authoritative source for execution state
                    info!(
                        "✅ [STARTUP] Local and Peer are in sync (Local={}, Peer={}). Using Local Go as authoritative.",
                        local_go_block, peer_last_block
                    );
                    local_go_block
                }
            } else {
                // No peer reference
                if persisted_index > local_go_block {
                    warn!("⚠️ [STARTUP] Persisted Index {} > Local Go {}. Go is behind (possible rollback/crash). Using Local Go {} to force resync/replay.", 
                        persisted_index, local_go_block, local_go_block);
                }
                info!(
                    "📊 [STARTUP] No peer reference, using Local Go Last Block: {}",
                    local_go_block
                );
                local_go_block
            }
        } else {
            0
        };

        // GO-AUTHORITATIVE: Trust Go's reported last_block_number without arbitrary limits
        // Previously reset to 0 if > 10000, which broke long-running chains
        if last_global_exec_index > 100000 {
            // Log warning for visibility but don't reset - Go is authoritative
            warn!(
                "⚠️ [STARTUP] Very high last_global_exec_index={} - this is normal for long-running chains. Trusting Go's value.",
                last_global_exec_index
            );
        }

        // EPOCH BASE INDEX: Use Go's boundary_block as authoritative source
        // Go's GetEpochBoundaryData returns the boundary_block which is the last block of the previous epoch.
        // For Epoch N, boundary_block is the global_exec_index at which Epoch N started.
        // This is the CORRECT epoch base for calculating: global_exec_index = epoch_base + commit_index
        //
        // Previously, we tried to calculate this from (persisted_index - persisted_commit), but this fails
        // when persisted state is missing or stale after epoch transitions.
        let epoch_base_exec_index = boundary_block;
        info!(
            "✅ [STARTUP] Using epoch_base={} from Go boundary_block (epoch={}, persisted_global={}, persisted_commit={})",
            epoch_base_exec_index, current_epoch, persisted_index, persisted_commit
        );

        // Recovery check
        if config.executor_read_enabled && last_global_exec_index > 0 {
            recovery::perform_block_recovery_check(
                &executor_client,
                last_global_exec_index,
                epoch_base_exec_index,
                current_epoch,
                &config.storage_path,
                config.node_id as u32,
            )
            .await?;
        }

        let protocol_keypair = config.load_protocol_keypair()?;
        let network_keypair = config.load_network_keypair()?;

        // FIX: Use protocol_key matching instead of hostname for robust identity
        // This ensures node identity is derived from cryptographic key, not naming convention
        let own_protocol_pubkey = protocol_keypair.public();
        let own_index_opt = committee.authorities().find_map(|(idx, auth)| {
            if auth.protocol_key == own_protocol_pubkey {
                Some(idx)
            } else {
                None
            }
        });

        let is_in_committee = own_index_opt.is_some();
        let own_index = own_index_opt.unwrap_or(AuthorityIndex::ZERO);

        if is_in_committee {
            info!(
                "✅ [IDENTITY] Found self in committee at index {} using protocol_key match",
                own_index
            );
        } else {
            info!(
                "ℹ️ [IDENTITY] Not in committee (protocol_key not found in {} authorities)",
                committee.size()
            );
        }

        std::fs::create_dir_all(&config.storage_path)?;

        let clock = Arc::new(Clock::default());
        let transaction_verifier = Arc::new(NoopTransactionVerifier);
        let (commit_consumer, commit_receiver, mut block_receiver) = CommitConsumerArgs::new(0, 0);
        let current_commit_index = Arc::new(AtomicU32::new(0));
        let is_transitioning = Arc::new(AtomicBool::new(false));

        // Load persisted transaction queue
        let persisted_queue = super::queue::load_transaction_queue_static(&storage_path)
            .await
            .unwrap_or_default();
        if !persisted_queue.is_empty() {
            info!("💾 Loaded {} persisted transactions", persisted_queue.len());
        }
        let pending_transactions_queue = Arc::new(tokio::sync::Mutex::new(persisted_queue));

        // Load committed transaction hashes from current epoch for duplicate prevention
        let committed_hashes = crate::node::transition::load_committed_transaction_hashes(
            &storage_path,
            current_epoch,
        )
        .await;
        if !committed_hashes.is_empty() {
            info!(
                "💾 Loaded {} committed transaction hashes from epoch {}",
                committed_hashes.len(),
                current_epoch
            );
        }
        let committed_transaction_hashes = Arc::new(tokio::sync::Mutex::new(committed_hashes));

        let (epoch_tx_sender, epoch_tx_receiver) =
            tokio::sync::mpsc::unbounded_channel::<(u64, u64, u64)>(); // CHANGED: u32 -> u64
        let epoch_transition_callback =
            crate::consensus::commit_callbacks::create_epoch_transition_callback(
                epoch_tx_sender.clone(),
            );

        // CRITICAL: Initialize shared index with BASE for correct generation
        // But CommitProcessor updates it... is it Base or Tip?
        // checkpoint::calculate_global_exec_index(..., last) -> last + 1.
        // So we need to provide the Previous Global Index.
        // If we are replaying 1..N.
        // Block 1 needs Base.
        // Block 2 needs Base + 1.
        // So shared_last_global_exec_index must start at BASE.
        // BUT CommitProcessor updates it after each commit.
        // If we replay Commit 3813, it will update to Base + 3813.
        // Which matches Tip. Correct.
        let shared_last_global_exec_index =
            Arc::new(tokio::sync::Mutex::new(epoch_base_exec_index));

        // Note: We do NOT read initial_commit_index from DB (per user request).
        // CommitProcessor has "Auto-Jump" logic to detect stream start index.

        let mut commit_processor = crate::consensus::commit_processor::CommitProcessor::new(
            commit_receiver,
        )
        .with_commit_index_callback(
            crate::consensus::commit_callbacks::create_commit_index_callback(
                current_commit_index.clone(),
            ),
        )
        .with_global_exec_index_callback(
            crate::consensus::commit_callbacks::create_global_exec_index_callback(
                shared_last_global_exec_index.clone(),
            ),
        )
        .with_get_last_global_exec_index({
            let shared_index = shared_last_global_exec_index.clone();
            move || {
                if let Ok(_rt) = tokio::runtime::Handle::try_current() {
                    warn!("⚠️ get_last_global_exec_index called from async context, returning 0.");
                    0
                } else {
                    let shared_index_clone = shared_index.clone();
                    futures::executor::block_on(async { *shared_index_clone.lock().await })
                }
            }
        })
        .with_shared_last_global_exec_index(shared_last_global_exec_index.clone())
        .with_epoch_info(current_epoch, epoch_base_exec_index) // Start from BASE
        .with_is_transitioning(is_transitioning.clone())
        .with_pending_transactions_queue(pending_transactions_queue.clone())
        .with_epoch_transition_callback(epoch_transition_callback)
        .with_epoch_eth_addresses({
            // Create initial multi-epoch cache with current epoch's addresses
            let mut map = std::collections::HashMap::new();
            map.insert(current_epoch, validator_eth_addresses.clone());
            Arc::new(tokio::sync::Mutex::new(map))
        });

        // INITIAL_NEXT_EXPECTED: This is for Replay Protection
        // It should be TIP + 1 (what Go expects next)
        let initial_next_expected = if config.executor_read_enabled {
            last_global_exec_index + 1
        } else {
            1
        };

        let executor_client_for_proc = if config.executor_read_enabled {
            Arc::new(ExecutorClient::new_with_initial_index(
                true,
                config.executor_commit_enabled,
                config.executor_send_socket_path.clone(),
                config.executor_receive_socket_path.clone(),
                initial_next_expected,
                Some(config.storage_path.clone()), // Enable persistence for commit client
            ))
        } else {
            Arc::new(ExecutorClient::new(
                false,
                false,
                "".to_string(),
                "".to_string(),
                None, // Disabled client, no persistence needed
            ))
        };

        let executor_client_for_init = executor_client_for_proc.clone();
        tokio::spawn(async move {
            executor_client_for_init.initialize_from_go().await;
        });

        commit_processor = commit_processor.with_executor_client(executor_client_for_proc.clone());

        tokio::spawn(async move {
            if let Err(e) = commit_processor.run().await {
                tracing::error!("❌ [COMMIT PROCESSOR] Error: {}", e);
            }
        });

        tokio::spawn(async move {
            while let Some(output) = block_receiver.recv().await {
                tracing::debug!("Received {} certified blocks", output.blocks.len());
            }
        });

        let protocol_config = ProtocolConfig::get_for_max_version_UNSAFE();
        let mut parameters = consensus_config::Parameters::default();
        parameters.commit_sync_batch_size = config.commit_sync_batch_size;
        parameters.commit_sync_parallel_fetches = config.commit_sync_parallel_fetches;
        parameters.commit_sync_batches_ahead = config.commit_sync_batches_ahead;

        // Apply min_round_delay if configured
        if let Some(ms) = config.min_round_delay_ms {
            parameters.min_round_delay = Duration::from_millis(ms);
        }

        parameters.adaptive_delay_enabled = config.adaptive_delay_enabled;

        // Apply leader_timeout: explicit config takes precedence, otherwise calculate from speed_multiplier
        if let Some(ms) = config.leader_timeout_ms {
            parameters.leader_timeout = Duration::from_millis(ms);
        } else if config.speed_multiplier != 1.0 {
            info!("Applying speed multiplier: {}x", config.speed_multiplier);
            parameters.leader_timeout =
                Duration::from_millis((200.0 / config.speed_multiplier) as u64);
        }

        let db_path = config
            .storage_path
            .join("epochs")
            .join(format!("epoch_{}", current_epoch))
            .join("consensus_db");

        // persistence fix: Do NOT delete existing db_path!
        // if db_path.exists() {
        //     let _ = std::fs::remove_dir_all(&db_path);
        // }
        std::fs::create_dir_all(&db_path)?;
        parameters.db_path = db_path;

        let epoch_duration_seconds = config.epoch_duration_seconds.unwrap_or(180);
        let system_transaction_provider = Arc::new(DefaultSystemTransactionProvider::new(
            current_epoch,
            epoch_duration_seconds,
            epoch_timestamp_ms,
            config.time_based_epoch_change,
        ));

        let clock_sync_manager = Arc::new(RwLock::new(ClockSyncManager::new(
            config.ntp_servers.clone(),
            config.max_clock_drift_seconds * 1000,
            config.ntp_sync_interval_seconds,
            config.enable_ntp_sync,
        )));

        if config.enable_ntp_sync {
            let sync_manager_clone = clock_sync_manager.clone();
            let monitor_manager_clone = clock_sync_manager.clone();
            tokio::spawn(async move {
                let mut manager = sync_manager_clone.write().await;
                let _ = manager.sync_with_ntp().await;
            });
            ClockSyncManager::start_sync_task(clock_sync_manager.clone());
            ClockSyncManager::start_drift_monitor(monitor_manager_clone);
        }

        // For SyncOnly nodes, we need to keep commit_consumer alive to prevent channel close
        let (authority, commit_consumer_holder) = if is_in_committee {
            info!("🚀 Starting consensus authority node...");
            (
                Some(
                    ConsensusAuthority::start(
                        NetworkType::Tonic,
                        epoch_timestamp_ms,
                        epoch_base_exec_index,
                        own_index,
                        committee.clone(),
                        parameters.clone(),
                        protocol_config.clone(),
                        protocol_keypair.clone(),
                        network_keypair.clone(),
                        clock.clone(),
                        transaction_verifier.clone(),
                        commit_consumer,
                        registry.clone(),
                        0,
                        Some(system_transaction_provider.clone()
                            as Arc<dyn SystemTransactionProvider>),
                        None, // No legacy stores on initial startup
                    )
                    .await,
                ),
                None, // Authority owns commit_consumer
            )
        } else {
            info!("🔄 Starting as sync-only node");
            info!("📡 Keeping commit_consumer alive for SyncOnly mode to prevent channel close");
            (None, Some(commit_consumer)) // Keep commit_consumer alive
        };

        let transaction_client_proxy = if let Some(ref auth) = authority {
            Some(Arc::new(TransactionClientProxy::new(
                auth.transaction_client(),
            )))
        } else {
            None
        };

        // Initialize no-op epoch change handlers (required by core)
        use consensus_core::epoch_change_provider::{EpochChangeProcessor, EpochChangeProvider};
        struct NoOpProvider;
        impl EpochChangeProvider for NoOpProvider {
            fn get_proposal(&self) -> Option<Vec<u8>> {
                None
            }
            fn get_votes(&self) -> Vec<Vec<u8>> {
                Vec::new()
            }
        }
        struct NoOpProcessor;
        impl EpochChangeProcessor for NoOpProcessor {
            fn process_proposal(&self, _: &[u8]) {}
            fn process_vote(&self, _: &[u8]) {}
        }
        consensus_core::epoch_change_provider::init_epoch_change_provider(Box::new(NoOpProvider));
        consensus_core::epoch_change_provider::init_epoch_change_processor(Box::new(NoOpProcessor));

        let mut node = Self {
            authority,
            legacy_store_manager: Arc::new(consensus_core::LegacyEpochStoreManager::new(
                config.epochs_to_keep,
            )), // 0 = archive (keep all), N = keep N epochs
            node_mode: if is_in_committee {
                NodeMode::Validator
            } else {
                NodeMode::SyncOnly
            },
            execution_lock: Arc::new(tokio::sync::RwLock::new(current_epoch)),
            reconfig_state: Arc::new(tokio::sync::RwLock::new(ReconfigState::default())),
            transaction_client_proxy,
            clock_sync_manager,
            current_commit_index,
            storage_path,
            current_epoch,
            last_global_exec_index,
            shared_last_global_exec_index,
            protocol_keypair,
            network_keypair,
            protocol_config,
            clock,
            transaction_verifier,
            parameters,
            own_index,
            boot_counter: 0,
            last_transition_hash: None,
            current_registry_id: None,
            executor_commit_enabled: config.executor_commit_enabled,
            is_transitioning,
            pending_transactions_queue,
            system_transaction_provider,
            epoch_transition_sender: epoch_tx_sender,
            sync_task_handle: None,
            sync_controller: Arc::new(crate::node::sync_controller::SyncController::new()),
            epoch_monitor_handle: None,
            notification_server_handle: None,
            executor_client: Some(executor_client_for_proc),
            epoch_pending_transactions: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            committed_transaction_hashes,
            pending_epoch_transitions: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            _commit_consumer_holder: commit_consumer_holder,
            epoch_eth_addresses: {
                let mut map = std::collections::HashMap::new();
                map.insert(current_epoch, validator_eth_addresses.clone());
                Arc::new(tokio::sync::Mutex::new(map))
            },
            // BlockCoordinator initialized later when executor_client is ready
            block_coordinator: None,
        };

        // CRITICAL FIX: Load previous epoch's RocksDB stores into legacy_store_manager
        // This ensures SyncOnly nodes can fetch commits from previous epochs on startup
        // Without this, validators starting directly in epoch 1 can't serve epoch 0 blocks
        // load_legacy_epoch_stores(
        //     &node.legacy_store_manager,
        //     &config.storage_path,
        //     current_epoch,
        // );

        // Initialize the global StateTransitionManager
        // This MUST happen before epoch_transition_handler and epoch_monitor start
        // Pass is_in_committee to set initial mode
        epoch_transition_manager::init_state_manager(current_epoch, is_in_committee).await;
        info!(
            "🔧 [STARTUP] Initialized StateTransitionManager: epoch={}, mode={}",
            current_epoch,
            if is_in_committee {
                "Validator"
            } else {
                "SyncOnly"
            }
        );

        crate::consensus::epoch_transition::start_epoch_transition_handler(
            epoch_tx_receiver,
            node.system_transaction_provider.clone(),
            config.clone(),
        );

        node.check_and_update_node_mode(&committee, &config, false)
            .await?;

        // Start sync task for SyncOnly nodes
        if matches!(node.node_mode, NodeMode::SyncOnly) {
            let _ = node.start_sync_task(&config).await;
        }

        // UNIFIED EPOCH MONITOR: Runs for ALL node modes (SyncOnly and Validator)
        // This replaces the previous fragmented approach with a single, always-running monitor
        // Key improvement: Monitor never exits, handles both mode transitions and epoch catchup
        if let Ok(Some(handle)) =
            epoch_monitor::start_unified_epoch_monitor(&node.executor_client, &config)
        {
            node.epoch_monitor_handle = Some(handle);
            info!(
                "🔄 Started unified epoch monitor for {:?} mode at epoch={}",
                node.node_mode, node.current_epoch
            );
        }

        recovery::perform_fork_detection_check(&node).await?;

        Ok(node)
    }
}
