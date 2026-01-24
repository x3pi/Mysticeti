// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use consensus_config::{AuthorityIndex, Committee};
use consensus_core::{
    Clock,
    CommitConsumerArgs,
    ConsensusAuthority,
    DefaultSystemTransactionProvider,
    NetworkType,
    ReconfigState,
    SystemTransactionProvider, // Added SystemTransactionProvider trait
};
// Removed unused imports ProtocolKeyPair, NetworkKeyPair
use crate::consensus::clock_sync::ClockSyncManager;
use crate::types::transaction::NoopTransactionVerifier;
use meta_protocol_config::ProtocolConfig;
use prometheus::Registry;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration; // Removed SystemTime, UNIX_EPOCH
use tokio::sync::RwLock;
use tracing::{info, warn}; // Added error

use crate::config::NodeConfig;
use crate::node::executor_client::ExecutorClient;
use crate::node::tx_submitter::{TransactionClientProxy, TransactionSubmitter};
// use consensus_core::storage::rocksdb_store::RocksDBStore;
// use consensus_core::storage::Store; // Added Store trait

// Declare submodules
pub mod catchup;
pub mod committee;
pub mod executor_client;
pub mod queue;
pub mod recovery;
pub mod startup;
pub mod sync;
pub mod transition;
pub mod tx_submitter;

/// Node operation modes
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum NodeMode {
    /// Node only syncs data, does not participate in voting
    SyncOnly,
    /// Node participates in consensus and voting
    Validator,
    /// Node is catching up with the network (syncing epoch/commits)
    SyncingUp,
}

// Global registry for transition handler to access node
static TRANSITION_HANDLER_REGISTRY: tokio::sync::OnceCell<
    Arc<tokio::sync::Mutex<Option<Arc<tokio::sync::Mutex<ConsensusNode>>>>>,
> = tokio::sync::OnceCell::const_new();

pub async fn get_transition_handler_node() -> Option<Arc<tokio::sync::Mutex<ConsensusNode>>> {
    if let Some(registry) = TRANSITION_HANDLER_REGISTRY.get() {
        let registry_guard = registry.lock().await;
        registry_guard.clone()
    } else {
        None
    }
}

pub async fn set_transition_handler_node(node: Arc<tokio::sync::Mutex<ConsensusNode>>) {
    if TRANSITION_HANDLER_REGISTRY.get().is_none() {
        let _ = TRANSITION_HANDLER_REGISTRY.set(Arc::new(tokio::sync::Mutex::new(None)));
    }

    if let Some(registry) = TRANSITION_HANDLER_REGISTRY.get() {
        let mut registry_guard = registry.lock().await;
        *registry_guard = Some(node);
        drop(registry_guard);
        info!("✅ Registered node in global transition handler registry");
    }
}

pub struct ConsensusNode {
    // Made fields pub(crate) so submodules can access them
    pub(crate) authority: Option<ConsensusAuthority>,
    pub(crate) node_mode: NodeMode,
    pub(crate) execution_lock: Arc<tokio::sync::RwLock<u64>>,
    pub(crate) reconfig_state: Arc<tokio::sync::RwLock<consensus_core::ReconfigState>>,
    pub(crate) transaction_client_proxy: Option<Arc<TransactionClientProxy>>,
    #[allow(dead_code)]
    pub(crate) clock_sync_manager: Arc<RwLock<ClockSyncManager>>,
    pub(crate) current_commit_index: Arc<AtomicU32>,

    pub(crate) storage_path: std::path::PathBuf,
    pub(crate) current_epoch: u64,
    pub(crate) last_global_exec_index: u64,
    pub(crate) shared_last_global_exec_index: Arc<tokio::sync::Mutex<u64>>,

    pub(crate) protocol_keypair: consensus_config::ProtocolKeyPair,
    pub(crate) network_keypair: consensus_config::NetworkKeyPair,
    pub(crate) protocol_config: ProtocolConfig,
    pub(crate) clock: Arc<Clock>,
    pub(crate) transaction_verifier: Arc<NoopTransactionVerifier>,
    pub(crate) parameters: consensus_config::Parameters,
    pub(crate) own_index: AuthorityIndex,
    pub(crate) boot_counter: u64,
    pub(crate) last_transition_hash: Option<Vec<u8>>,
    #[allow(dead_code)]
    pub(crate) current_registry_id: Option<mysten_metrics::RegistryID>,
    pub(crate) executor_commit_enabled: bool,
    pub(crate) is_transitioning: Arc<AtomicBool>,
    pub(crate) pending_transactions_queue: Arc<tokio::sync::Mutex<Vec<Vec<u8>>>>,
    pub(crate) system_transaction_provider: Arc<DefaultSystemTransactionProvider>,
    pub(crate) epoch_transition_sender: tokio::sync::mpsc::UnboundedSender<(u64, u64, u32)>,
    pub(crate) sync_task_handle: Option<tokio::task::JoinHandle<()>>,
    pub(crate) executor_client: Option<Arc<ExecutorClient>>,
    /// Transactions submitted in current epoch that may need recovery during epoch transition
    pub(crate) epoch_pending_transactions: Arc<tokio::sync::Mutex<Vec<Vec<u8>>>>,
    /// Transaction hashes that have been committed in current epoch (for duplicate prevention)
    pub(crate) committed_transaction_hashes:
        Arc<tokio::sync::Mutex<std::collections::HashSet<Vec<u8>>>>,
}

impl ConsensusNode {
    #[allow(dead_code)]
    pub async fn new(config: NodeConfig) -> Result<Self> {
        Self::new_with_registry(config, Registry::new()).await
    }

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
            None, // Read-only client, no persistence needed
        ));

        let latest_block_number = executor_client
            .get_last_block_number()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to fetch latest block from Go: {}", e))?;

        // PEER EPOCH DISCOVERY: Query multiple Go Masters to get correct epoch
        // This handles cases where the local Go Master has stale data after restart
        let (go_epoch, peer_last_block, best_socket) = if !config.peer_go_master_sockets.is_empty()
        {
            match catchup::query_peer_epochs(
                &config.peer_go_master_sockets,
                &config.executor_receive_socket_path,
            )
            .await
            {
                Ok(result) => {
                    info!(
                        "✅ [PEER EPOCH] Using epoch {} from peer discovery",
                        result.0
                    );
                    result
                }
                Err(e) => {
                    warn!("⚠️ [PEER EPOCH] Failed to query peers, falling back to local Go Master: {}", e);
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
            // No peer sockets configured, use local Go Master
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

            // Clear old epoch data that is stale
            for epoch in local_epoch..go_epoch {
                let epoch_path = storage_path.join("epochs").join(format!("epoch_{}", epoch));
                if epoch_path.exists() {
                    info!("🗑️ [CATCHUP] Clearing stale epoch {} data", epoch);
                    if let Err(e) = std::fs::remove_dir_all(&epoch_path) {
                        warn!("⚠️ [CATCHUP] Failed to clear epoch {} data: {}", epoch, e);
                    }
                }
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

        let (validators, _go_epoch_timestamp_ms) = peer_executor_client
            .get_validators_at_block(peer_last_block)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to fetch committee from Go: {}", e))?;

        // Use epoch timestamp from the peer that has correct epoch
        let epoch_timestamp_ms = _go_epoch_timestamp_ms;
        info!(
            "📊 [STARTUP] Epoch timestamp: {}ms (from {})",
            epoch_timestamp_ms,
            if best_socket != config.executor_receive_socket_path {
                "peer"
            } else {
                "local"
            }
        );

        if validators.is_empty() {
            anyhow::bail!("Go state returned empty validators list");
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

        // Use helper from committee.rs
        let committee =
            committee::build_committee_from_validator_list(validators_to_use, current_epoch)?;
        info!("✅ Loaded committee with {} authorities", committee.size());

        // EXECUTION INDEX SYNC
        // GO-AUTHORITATIVE STRATEGY with local persistence fallback
        // Priority: Go's last_block_number (authoritative) with local persistence as safety net
        let last_global_exec_index = if config.executor_read_enabled {
            let local_go_block = executor_client.get_last_block_number().await.unwrap_or(0);

            // Also check for persisted index from previous run
            let persisted_index =
                executor_client::load_persisted_last_index(&storage_path).unwrap_or(0);

            // Check if we have a valid peer reference from startup scan
            if best_socket != config.executor_receive_socket_path && peer_last_block > 0 {
                info!(
                    "📊 [STARTUP] Sync Check: LocalGo={}, Peer={}, Persisted={} (from {})",
                    local_go_block, peer_last_block, persisted_index, best_socket
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
                } else if local_go_block < peer_last_block.saturating_sub(1000) {
                    // Way behind? Use peer index as target?
                    // No, if we are behind, we should use local index and let catchup handle it?
                    // But catchup is for consensus.
                    // If we tell consensus we are at 1272 (peer), but we are at 0, consensus starts at 1273.
                    // This creates gap 0..1272.
                    // BUT Go execution sync should fill it?
                    // Let's rely on Local if behind, but Peer if Forked.
                    info!(
                        "ℹ️ [STARTUP] Local is behind Peer. Using Local {} to ensure continuity.",
                        local_go_block
                    );
                    local_go_block
                } else {
                    // Close enough, trust Peer as it's the network tip we synced epoch from
                    if peer_last_block > local_go_block {
                        info!(
                            "✅ [STARTUP] Using Peer Index {} (ahead of local {})",
                            peer_last_block, local_go_block
                        );
                        peer_last_block
                    } else {
                        local_go_block
                    }
                }
            } else {
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

        // Recovery check
        if config.executor_read_enabled && last_global_exec_index > 0 {
            recovery::perform_block_recovery_check(
                &executor_client,
                last_global_exec_index,
                current_epoch,
                &config.storage_path,
                config.node_id as u32,
            )
            .await?;
        }

        let protocol_keypair = config.load_protocol_keypair()?;
        let network_keypair = config.load_network_keypair()?;

        let own_hostname = format!("node-{}", config.node_id);
        let own_index_opt = committee.authorities().find_map(|(idx, auth)| {
            if auth.hostname == own_hostname {
                Some(idx)
            } else {
                None
            }
        });

        let is_in_committee = own_index_opt.is_some();
        let own_index = own_index_opt.unwrap_or(AuthorityIndex::ZERO);

        std::fs::create_dir_all(&config.storage_path)?;

        let clock = Arc::new(Clock::default());
        let transaction_verifier = Arc::new(NoopTransactionVerifier);
        let (commit_consumer, commit_receiver, mut block_receiver) = CommitConsumerArgs::new(0, 0);
        let current_commit_index = Arc::new(AtomicU32::new(0));
        let is_transitioning = Arc::new(AtomicBool::new(false));

        // Load persisted transaction queue
        let persisted_queue = queue::load_transaction_queue_static(&storage_path)
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
            tokio::sync::mpsc::unbounded_channel::<(u64, u64, u32)>();
        let epoch_transition_callback =
            crate::consensus::commit_callbacks::create_epoch_transition_callback(
                epoch_tx_sender.clone(),
            );

        let shared_last_global_exec_index =
            Arc::new(tokio::sync::Mutex::new(last_global_exec_index));

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
        .with_epoch_info(current_epoch, last_global_exec_index)
        .with_is_transitioning(is_transitioning.clone())
        .with_pending_transactions_queue(pending_transactions_queue.clone())
        .with_epoch_transition_callback(epoch_transition_callback);

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

        if config.speed_multiplier != 1.0 {
            info!("Applying speed multiplier: {}x", config.speed_multiplier);
            let leader_timeout = config
                .leader_timeout_ms
                .map(Duration::from_millis)
                .unwrap_or_else(|| Duration::from_millis((200.0 / config.speed_multiplier) as u64));
            parameters.leader_timeout = leader_timeout;
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

        let authority = if is_in_committee {
            info!("🚀 Starting consensus authority node...");
            Some(
                ConsensusAuthority::start(
                    NetworkType::Tonic,
                    epoch_timestamp_ms,
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
                    Some(system_transaction_provider.clone() as Arc<dyn SystemTransactionProvider>),
                )
                .await,
            )
        } else {
            info!("🔄 Starting as sync-only node");
            None
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
            node_mode: config.initial_node_mode.clone(),
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
            executor_client: Some(executor_client_for_proc),
            epoch_pending_transactions: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            committed_transaction_hashes,
        };

        crate::consensus::epoch_transition::start_epoch_transition_handler(
            epoch_tx_receiver,
            node.system_transaction_provider.clone(),
            config.clone(),
        );

        node.check_and_update_node_mode(&committee, &config).await?;

        if matches!(node.node_mode, NodeMode::SyncOnly) {
            let _ = node.start_sync_task(&config).await;
        }

        recovery::perform_fork_detection_check(&node).await?;

        Ok(node)
    }

    pub fn transaction_submitter(&self) -> Option<Arc<dyn TransactionSubmitter>> {
        self.transaction_client_proxy
            .as_ref()
            .map(|proxy| proxy.clone() as Arc<dyn TransactionSubmitter>)
    }

    pub async fn check_transaction_acceptance(&self) -> (bool, bool, String) {
        if self.authority.is_none() {
            return (false, false, "Node is still initializing".to_string());
        }
        if self.last_transition_hash.is_some() {
            return (
                false,
                false,
                format!(
                    "Epoch transition in progress: epoch {} -> {}",
                    self.current_epoch,
                    self.current_epoch + 1
                ),
            );
        }
        if self.is_transitioning.load(Ordering::SeqCst) {
            return (false, true, "Epoch transition in progress".to_string());
        }
        if !self.should_accept_tx().await {
            return (false, true, "Reconfiguration in progress".to_string());
        }
        (true, false, "Node is ready".to_string())
    }

    pub async fn queue_transaction_for_next_epoch(&self, tx_data: Vec<u8>) -> Result<()> {
        queue::queue_transaction(
            &self.pending_transactions_queue,
            &self.storage_path,
            tx_data,
        )
        .await
    }

    pub async fn submit_queued_transactions(&mut self) -> Result<usize> {
        queue::submit_queued_transactions(self).await
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        info!("Shutting down consensus node...");
        let _ = self.stop_sync_task().await;
        if let Some(authority) = self.authority.take() {
            authority.stop().await;
        }
        info!("Consensus node stopped");
        Ok(())
    }

    // Delegated methods
    pub async fn check_and_update_node_mode(
        &mut self,
        committee: &Committee,
        config: &NodeConfig,
    ) -> Result<()> {
        let node_hostname = format!("node-{}", config.node_id);
        let should_be_validator = committee
            .authorities()
            .any(|(_, authority)| authority.hostname == node_hostname);

        let new_mode = if should_be_validator {
            NodeMode::Validator
        } else {
            NodeMode::SyncOnly
        };

        if self.node_mode != new_mode {
            info!(
                "🔄 [NODE MODE] Switching from {:?} to {:?}",
                self.node_mode, new_mode
            );
            match (&self.node_mode, &new_mode) {
                (NodeMode::SyncOnly, NodeMode::Validator) => {
                    let _ = self.stop_sync_task().await;
                }
                (NodeMode::Validator, NodeMode::SyncOnly) => {
                    let _ = self.start_sync_task(config).await;
                }
                _ => {}
            }
            self.node_mode = new_mode;
        }
        Ok(())
    }

    pub async fn start_sync_task(&mut self, config: &NodeConfig) -> Result<()> {
        sync::start_sync_task(self, config).await
    }

    pub async fn stop_sync_task(&mut self) -> Result<()> {
        sync::stop_sync_task(self).await
    }

    /// Flush all buffered blocks to Go Master before shutdown
    /// This ensures no blocks are lost during shutdown
    pub async fn flush_blocks_to_go_master(&self) -> Result<()> {
        if let Some(ref executor_client) = self.executor_client {
            info!("🔄 [SHUTDOWN] Flushing buffered blocks to Go Master...");
            match executor_client.flush_buffer().await {
                Ok(_) => {
                    info!("✅ [SHUTDOWN] Successfully flushed all blocks to Go Master");
                    Ok(())
                }
                Err(e) => {
                    warn!("⚠️  [SHUTDOWN] Failed to flush blocks to Go Master: {}", e);
                    // Don't fail shutdown if flush fails - Go will buffer and process sequentially
                    Ok(())
                }
            }
        } else {
            info!("ℹ️  [SHUTDOWN] No executor client configured, skipping block flush");
            Ok(())
        }
    }

    pub async fn transition_to_epoch_from_system_tx(
        &mut self,
        new_epoch: u64,
        new_epoch_timestamp_ms: u64,
        commit_index: u32,
        config: &NodeConfig,
    ) -> Result<()> {
        transition::transition_to_epoch_from_system_tx(
            self,
            new_epoch,
            new_epoch_timestamp_ms,
            commit_index,
            config,
        )
        .await
    }

    // Reconfiguration delegators

    pub async fn update_execution_lock_epoch(&self, new_epoch: u64) {
        *self.execution_lock.write().await = new_epoch;
    }

    pub async fn reset_reconfig_state(&self) {
        *self.reconfig_state.write().await = ReconfigState::default();
    }

    pub async fn close_user_certs(&self) {
        self.reconfig_state.write().await.close_user_certs();
    }

    pub async fn should_accept_tx(&self) -> bool {
        self.reconfig_state.read().await.should_accept_tx()
    }
}

/// Detect the highest epoch stored locally
/// Returns 0 if no epoch data found
fn detect_local_epoch(storage_path: &std::path::Path) -> u64 {
    let epochs_dir = storage_path.join("epochs");
    if !epochs_dir.exists() {
        return 0;
    }

    let mut max_epoch = 0u64;
    if let Ok(entries) = std::fs::read_dir(&epochs_dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if let Some(epoch_str) = name.strip_prefix("epoch_") {
                    if let Ok(epoch) = epoch_str.parse::<u64>() {
                        max_epoch = max_epoch.max(epoch);
                    }
                }
            }
        }
    }

    max_epoch
}
