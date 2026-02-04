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
use consensus_core::storage::rocksdb_store::RocksDBStore;

// Declare submodules
pub mod catchup;
pub mod committee;
pub mod committee_source;
pub mod epoch_monitor;
pub mod executor_client;
pub mod notification_listener;
pub mod notification_server;
pub mod peer_go_client;
pub mod queue;
pub mod recovery;
pub mod rust_sync_node;
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

/// Pending epoch transition that is deferred until sync is complete
/// Used by SyncOnly nodes to ensure they don't advance epoch before syncing all blocks
#[derive(Clone, Debug)]
pub struct PendingEpochTransition {
    pub epoch: u64,
    pub timestamp_ms: u64,
    pub boundary_block: u64,
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
    /// Legacy epoch store manager - keeps RocksDB stores from previous epochs
    /// open for read-only sync access. Only stores (not full authorities) are kept.
    pub(crate) legacy_store_manager: Arc<consensus_core::LegacyEpochStoreManager>,
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
    pub(crate) epoch_transition_sender: tokio::sync::mpsc::UnboundedSender<(u64, u64, u64)>,

    // Handles for background tasks
    pub(crate) sync_task_handle: Option<crate::node::rust_sync_node::RustSyncHandle>,
    pub(crate) epoch_monitor_handle: Option<tokio::task::JoinHandle<()>>,
    pub(crate) notification_server_handle: Option<tokio::task::JoinHandle<Result<()>>>,
    pub(crate) executor_client: Option<Arc<ExecutorClient>>,
    /// Transactions submitted in current epoch that may need recovery during epoch transition
    pub(crate) epoch_pending_transactions: Arc<tokio::sync::Mutex<Vec<Vec<u8>>>>,
    /// Transaction hashes that have been committed in current epoch (for duplicate prevention)
    pub(crate) committed_transaction_hashes:
        Arc<tokio::sync::Mutex<std::collections::HashSet<Vec<u8>>>>,

    /// Queued epoch transitions waiting for sync to complete (for SyncOnly nodes)
    /// When a SyncOnly node receives AdvanceEpoch but hasn't synced to the boundary yet,
    /// the transition is queued here and processed after sync catches up
    pub(crate) pending_epoch_transitions: Arc<tokio::sync::Mutex<Vec<PendingEpochTransition>>>,

    /// Holds commit_consumer to prevent channel close for SyncOnly nodes
    /// When authority is None (SyncOnly), commit_consumer would be dropped causing
    /// commit_receiver to close immediately. This field keeps it alive.
    #[allow(dead_code)]
    pub(crate) _commit_consumer_holder: Option<CommitConsumerArgs>,
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
            Some(config.storage_path.clone()), // Enable persistence for readout client
        ));

        let latest_block_number = match executor_client.get_last_block_number().await {
            Ok(n) => n,
            Err(e) => {
                warn!("⚠️ [STARTUP] Failed to fetch latest block from Go: {}. Attempting to read persisted value.", e);
                // Fallback to persisted last block number
                executor_client::read_last_block_number(&config.storage_path)
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

        // Use helper from committee.rs
        let committee =
            committee::build_committee_from_validator_list(validators_to_use, current_epoch)?;
        info!(
            "✅ Loaded committee with {} authorities (from epoch boundary)",
            committee.size()
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
            executor_client::load_persisted_last_index(&storage_path).unwrap_or((0, 0))
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
        .with_epoch_transition_callback(epoch_transition_callback);

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
            legacy_store_manager: Arc::new(consensus_core::LegacyEpochStoreManager::new(2)), // Keep 2 previous epochs for cross-epoch sync
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
            epoch_monitor_handle: None,
            notification_server_handle: None,
            executor_client: Some(executor_client_for_proc),
            epoch_pending_transactions: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            committed_transaction_hashes,
            pending_epoch_transitions: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            _commit_consumer_holder: commit_consumer_holder,
        };

        // CRITICAL FIX: Load previous epoch's RocksDB stores into legacy_store_manager
        // This ensures SyncOnly nodes can fetch commits from previous epochs on startup
        // Without this, validators starting directly in epoch 1 can't serve epoch 0 blocks
        // load_legacy_epoch_stores(
        //     &node.legacy_store_manager,
        //     &config.storage_path,
        //     current_epoch,
        // );

        crate::consensus::epoch_transition::start_epoch_transition_handler(
            epoch_tx_receiver,
            node.system_transaction_provider.clone(),
            config.clone(),
        );

        node.check_and_update_node_mode(&committee, &config).await?;

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
        // FIX: Use protocol_key matching for consistent identity check
        let own_protocol_pubkey = self.protocol_keypair.public();
        let should_be_validator = committee
            .authorities()
            .any(|(_, authority)| authority.protocol_key == own_protocol_pubkey);

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
                    // =======================================================================
                    // CENTRALIZED ORDERING FIX: SyncOnly → Validator
                    // ORDER: 1. Stop sync 2. Update mode 3. Notify Go
                    // This prevents race conditions where sync continues after consensus starts
                    // =======================================================================

                    // STEP 1: Stop sync task FIRST (before mode change or Go notification)
                    info!("🛑 [TRANSITION] STEP 1: Stopping sync task before becoming Validator");
                    let _ = self.stop_sync_task().await;

                    // STEP 2: Update mode atomically
                    self.node_mode = new_mode.clone();

                    // MODE TRANSITION STATE LOG
                    info!(
                        "📊 [MODE TRANSITION] SyncOnly → Validator: epoch={}, last_global_exec_index={}, commit_index={}",
                        self.current_epoch,
                        self.last_global_exec_index,
                        self.current_commit_index.load(std::sync::atomic::Ordering::SeqCst)
                    );

                    // STEP 3: Notify Go AFTER sync is fully stopped
                    // Consensus will produce blocks starting from last_global_exec_index + 1
                    let consensus_start_block = self.last_global_exec_index + 1;
                    if let Some(ref executor_client) = self.executor_client {
                        match executor_client
                            .set_consensus_start_block(consensus_start_block)
                            .await
                        {
                            Ok((success, last_sync_block, msg)) => {
                                if success {
                                    info!(
                                        "✅ [TRANSITION HANDOFF] Go confirmed sync complete up to block {} (consensus starts at {})",
                                        last_sync_block, consensus_start_block
                                    );
                                } else {
                                    warn!(
                                        "⚠️ [TRANSITION HANDOFF] Go sync not ready: {} (last_sync_block={}, need={})",
                                        msg, last_sync_block, consensus_start_block - 1
                                    );
                                    // Optionally wait for sync to catch up
                                    if let Ok((reached, current, _)) = executor_client
                                        .wait_for_sync_to_block(consensus_start_block - 1, 30)
                                        .await
                                    {
                                        if reached {
                                            info!("✅ [TRANSITION HANDOFF] Go sync caught up to block {}", current);
                                        } else {
                                            warn!("⚠️ [TRANSITION HANDOFF] Timeout waiting for Go sync (current={})", current);
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("⚠️ [TRANSITION HANDOFF] Failed to notify Go of consensus start: {}", e);
                            }
                        }
                    }

                    // CRITICAL FIX 2026-02-01: Restart epoch monitor after becoming Validator
                    // The epoch monitor is designed to run continuously for ALL node modes.
                    // Previously, we only take() the handle without restarting, which left
                    // Validators without epoch monitoring backup when they miss EndOfEpoch txs.
                    let _ = self.epoch_monitor_handle.take(); // Take old handle first
                                                              // Start new epoch monitor for Validator mode
                    if let Ok(Some(handle)) =
                        epoch_monitor::start_unified_epoch_monitor(&self.executor_client, config)
                    {
                        info!("🔄 [EPOCH MONITOR] Restarted epoch monitor after promotion to Validator");
                        self.epoch_monitor_handle = Some(handle);
                    }
                }
                (NodeMode::Validator, NodeMode::SyncOnly) => {
                    // =======================================================================
                    // CENTRALIZED ORDERING FIX: Validator → SyncOnly
                    // ORDER: 1. Stop authority 2. Update mode 3. Notify Go 4. Start sync
                    // This prevents race conditions where consensus continues after sync starts
                    // =======================================================================

                    // STEP 1: Stop authority FIRST (before mode change or Go notification)
                    info!("🛑 [TRANSITION] STEP 1: Stopping consensus authority before becoming SyncOnly");
                    if let Some(auth) = self.authority.take() {
                        auth.stop().await;
                        info!("✅ [TRANSITION] Authority stopped successfully");
                    }

                    // STEP 2: Update mode atomically
                    self.node_mode = new_mode.clone();

                    // MODE TRANSITION STATE LOG
                    info!(
                        "📊 [MODE TRANSITION] Validator → SyncOnly: epoch={}, last_global_exec_index={}, commit_index={}",
                        self.current_epoch,
                        self.last_global_exec_index,
                        self.current_commit_index.load(std::sync::atomic::Ordering::SeqCst)
                    );

                    // STEP 3: Notify Go AFTER authority is fully stopped
                    let last_consensus_block = self.last_global_exec_index;
                    if let Some(ref executor_client) = self.executor_client {
                        match executor_client
                            .set_sync_start_block(last_consensus_block)
                            .await
                        {
                            Ok((success, sync_start_block, msg)) => {
                                if success {
                                    info!(
                                        "✅ [TRANSITION HANDOFF] Go will start sync from block {} (consensus ended at {})",
                                        sync_start_block, last_consensus_block
                                    );
                                } else {
                                    warn!("⚠️ [TRANSITION HANDOFF] Go sync start notification failed: {}", msg);
                                }
                            }
                            Err(e) => {
                                warn!(
                                    "⚠️ [TRANSITION HANDOFF] Failed to notify Go of sync start: {}",
                                    e
                                );
                            }
                        }
                    }

                    // STEP 4: Start sync task (now safe - authority is stopped)
                    let _ = self.start_sync_task(config).await;
                    // Start unified epoch monitor for demotion recovery
                    if let Ok(Some(handle)) =
                        epoch_monitor::start_unified_epoch_monitor(&self.executor_client, config)
                    {
                        info!("🔄 [EPOCH MONITOR] Started unified epoch monitor after demotion to SyncOnly");
                        self.epoch_monitor_handle = Some(handle);
                    }
                }
                _ => {
                    self.node_mode = new_mode.clone();
                }
            }
        }
        Ok(())
    }

    pub async fn start_sync_task(&mut self, config: &NodeConfig) -> Result<()> {
        // Start notification server for event-driven transitions
        if let Err(e) = self.start_notification_server(config).await {
            warn!("⚠️ Failed to start notification server: {}", e);
        }
        sync::start_sync_task(self, config).await
    }

    pub async fn stop_sync_task(&mut self) -> Result<()> {
        // Stop notification server as well when stopping sync
        if let Some(handle) = self.notification_server_handle.take() {
            info!("🛑 [NOTIFICATION SERVER] Stopping...");
            handle.abort();
        }
        sync::stop_sync_task(self).await
    }

    pub async fn start_notification_server(&mut self, config: &NodeConfig) -> Result<()> {
        // Only start if not already running
        if self.notification_server_handle.is_some() {
            return Ok(());
        }

        let socket_path = std::path::PathBuf::from(&config.executor_receive_socket_path)
            .parent()
            .unwrap()
            .join(format!("metanode-notification-{}.sock", config.node_id));
        let sender = self.epoch_transition_sender.clone();

        let server = notification_server::EpochNotificationServer::new(
            socket_path,
            move |epoch, timestamp, boundary| {
                // Forward to transition handler
                if let Err(e) = sender.send((epoch, timestamp, boundary)) {
                    return Err(anyhow::anyhow!(
                        "Failed to forward epoch notification: {}",
                        e
                    ));
                }
                Ok(())
            },
        );

        let handle = tokio::spawn(async move { server.start().await });

        self.notification_server_handle = Some(handle);
        info!("🚀 [NOTIFICATION SERVER] Started background task");
        Ok(())
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
        synced_global_exec_index: u64, // CHANGED: Use global_exec_index (u64) instead of commit_index (u32)
        config: &NodeConfig,
    ) -> Result<()> {
        transition::transition_to_epoch_from_system_tx(
            self,
            new_epoch,
            new_epoch_timestamp_ms,
            synced_global_exec_index,
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

/// Load previous epoch RocksDB stores into LegacyEpochStoreManager
/// This enables nodes to serve historical commits when starting directly in a later epoch
#[allow(dead_code)]
fn load_legacy_epoch_stores(
    legacy_manager: &std::sync::Arc<consensus_core::LegacyEpochStoreManager>,
    storage_path: &std::path::Path,
    current_epoch: u64,
) {
    if current_epoch == 0 {
        // No previous epochs to load
        return;
    }

    let epochs_dir = storage_path.join("epochs");
    if !epochs_dir.exists() {
        info!("📦 [LEGACY STORE] No epochs directory found, skipping legacy store loading");
        return;
    }

    // Load previous epochs (up to max_epochs in LegacyEpochStoreManager)
    let mut loaded_count = 0;
    for epoch in (0..current_epoch).rev() {
        let epoch_db_path = epochs_dir
            .join(format!("epoch_{}", epoch))
            .join("consensus_db");

        if epoch_db_path.exists() {
            info!(
                "📦 [LEGACY STORE] Found previous epoch {} database at {:?}",
                epoch, epoch_db_path
            );

            // Create read-write store for the legacy epoch
            // Note: RocksDB supports concurrent access from the same process
            let legacy_store =
                std::sync::Arc::new(RocksDBStore::new(epoch_db_path.to_str().unwrap_or("")));

            legacy_manager.add_store(epoch, legacy_store);
            loaded_count += 1;

            info!(
                "✅ [LEGACY STORE] Loaded epoch {} store for historical sync",
                epoch
            );

            // Only load max_epochs number of stores
            if loaded_count >= 1 {
                break;
            }
        } else {
            info!(
                "⚠️ [LEGACY STORE] Epoch {} database not found at {:?}",
                epoch, epoch_db_path
            );
        }
    }

    if loaded_count > 0 {
        info!(
            "📦 [LEGACY STORE] Successfully loaded {} previous epoch store(s) for sync",
            loaded_count
        );
    } else {
        warn!(
            "⚠️ [LEGACY STORE] No previous epoch stores found. SyncOnly nodes may not be able to fetch historical commits."
        );
    }
}
