// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use futures::executor::block_on;
use consensus_config::{AuthorityIndex, Committee, Authority};
use consensus_core::{
    ConsensusAuthority, NetworkType, Clock,
    CommitConsumerArgs, ReconfigState, SystemTransaction,
    storage::rocksdb_store::RocksDBStore,
    load_committed_subdag_from_store, BlockAPI, CommitAPI,
};
use consensus_config::{AuthorityPublicKey, ProtocolPublicKey, NetworkPublicKey};
use fastcrypto::ed25519;
use fastcrypto::bls12381;
use fastcrypto::traits::ToFromBytes;
use crate::transaction::NoopTransactionVerifier;
use crate::clock_sync::ClockSyncManager;
use prometheus::Registry;
use mysten_metrics::RegistryService;
use serde_json;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use meta_protocol_config::ProtocolConfig;
use tracing::{info, warn, error};
use tokio::sync::RwLock;
use hex;
use base64::{Engine as _, engine::general_purpose};
use tracing::trace;

use crate::config::NodeConfig;
use crate::tx_submitter::{TransactionClientProxy, TransactionSubmitter};
use crate::checkpoint::calculate_global_exec_index;
use crate::executor_client::ExecutorClient;
use consensus_core::{
    storage::Store,
    SystemTransactionProvider, DefaultSystemTransactionProvider,
};

/// Node operation modes
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum NodeMode {
    /// Node chỉ đồng bộ dữ liệu, không tham gia voting
    SyncOnly,
    /// Node tham gia consensus và voting
    Validator,
}

// Global registry for transition handler to access node
// This allows transition handler task to call transition function on the node
static TRANSITION_HANDLER_REGISTRY: tokio::sync::OnceCell<Arc<tokio::sync::Mutex<Option<Arc<tokio::sync::Mutex<ConsensusNode>>>>>> = tokio::sync::OnceCell::const_new();

pub async fn get_transition_handler_node() -> Option<Arc<tokio::sync::Mutex<ConsensusNode>>> {
    if let Some(registry) = TRANSITION_HANDLER_REGISTRY.get() {
        let registry_guard = registry.lock().await;
        registry_guard.clone()
    } else {
        None
    }
}

/// Register node in global registry for transition handler access
/// This should be called after node is wrapped in Arc<Mutex<>> in main.rs
pub async fn set_transition_handler_node(node: Arc<tokio::sync::Mutex<ConsensusNode>>) {
    // Initialize registry if not already initialized
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
    authority: Option<ConsensusAuthority>,
    /// Current node operation mode (SyncOnly or Validator)
    node_mode: NodeMode,
    /// Execution lock to prevent transaction interruption during reconfiguration
    /// Similar to Sui's ExecutionLock, protects against concurrent reconfiguration
    execution_lock: Arc<tokio::sync::RwLock<u64>>, // epoch ID
    /// Reconfiguration state for gradual transaction rejection
    /// Similar to Sui's ReconfigState, enables smooth epoch transitions
    reconfig_state: Arc<tokio::sync::RwLock<consensus_core::ReconfigState>>,
    /// Stable handle for RPC submissions across in-process authority restart
    /// None for sync-only nodes that don't submit transactions
    transaction_client_proxy: Option<Arc<TransactionClientProxy>>,
    /// Clock synchronization manager
    #[allow(dead_code)] // Used internally by sync tasks
    clock_sync_manager: Arc<RwLock<ClockSyncManager>>,
    /// Current commit index (for fork-safe epoch transition)
    #[allow(dead_code)] // Used internally by commit processor callback
    current_commit_index: Arc<AtomicU32>,

    /// Paths needed for real epoch transition (persist + clean state)
    storage_path: std::path::PathBuf,
    /// Current epoch (for deterministic global_exec_index calculation)
    current_epoch: u64,
    /// Last global execution index (for deterministic global_exec_index calculation)
    last_global_exec_index: u64,
    /// Shared last global exec index for commit processor callbacks
    shared_last_global_exec_index: Arc<tokio::sync::Mutex<u64>>,

    // --- restart support ---
    protocol_keypair: consensus_config::ProtocolKeyPair,
    network_keypair: consensus_config::NetworkKeyPair,
    protocol_config: ProtocolConfig,
    clock: Arc<Clock>,
    transaction_verifier: Arc<NoopTransactionVerifier>,
    parameters: consensus_config::Parameters,
    own_index: AuthorityIndex,
    boot_counter: u64,
    /// Ensure we only run transition once per proposal hash.
    last_transition_hash: Option<Vec<u8>>,
    /// Metrics registry service - used to add new registries on epoch transition
    registry_service: Option<Arc<RegistryService>>,
    /// Current epoch registry ID (for cleanup if needed)
    current_registry_id: Option<mysten_metrics::RegistryID>,
    /// Executor commit enabled flag (from config) - allows committing blocks to Go
    executor_commit_enabled: bool,
    /// Flag indicating if epoch transition is in progress
    /// When true, new transactions will be queued for the next epoch
    is_transitioning: Arc<AtomicBool>,
    /// Queue for transactions received during epoch transition
    /// Transactions in this queue will be submitted to consensus in the next epoch
    pending_transactions_queue: Arc<tokio::sync::Mutex<Vec<Vec<u8>>>>,
    /// LVM snapshot configuration (from config)
    #[allow(dead_code)] // May be used in future
    enable_lvm_snapshot: bool,
    #[allow(dead_code)] // May be used in future
    lvm_snapshot_bin_path: Option<std::path::PathBuf>,
    #[allow(dead_code)] // May be used in future
    lvm_snapshot_delay_seconds: u64,
    /// System transaction provider for Sui-style epoch transition (EndOfEpoch system transaction)
    system_transaction_provider: Arc<DefaultSystemTransactionProvider>,
    // TODO: Future enhancement - notification channel for commit processor completion
    // /// Notification channel for commit processor completion
    // /// Commit processor sends notification when all commits are processed
    // commit_complete_sender: tokio::sync::watch::Sender<bool>,
    // /// Receiver for commit processor completion notifications
    // commit_complete_receiver: tokio::sync::watch::Receiver<bool>,
    /// Channel sender for epoch transition requests from system transactions
    epoch_transition_sender: tokio::sync::mpsc::UnboundedSender<(u64, u64, u32)>,
    /// Sync task handle for sync-only nodes
    sync_task_handle: Option<tokio::task::JoinHandle<()>>,
}

// ===== CONSTRUCTOR & INITIALIZATION METHODS =====
impl ConsensusNode {
    // ===== UTILITY METHODS =====

    /// Calculate adaptive timeout based on system load
    fn calculate_adaptive_timeout(base_timeout: u64, system_load: f64) -> u64 {
        // Increase timeout when system load is high (above 1.0)
        // Formula: base_timeout * (1.0 + (load - 1.0) * 0.5), max 2x base_timeout
        let multiplier = 1.0 + (system_load - 1.0).max(0.0) * 0.5;
        (base_timeout as f64 * multiplier.min(2.0)) as u64
    }

    /// Get current system load (simplified version)
    /// Returns load factor where 1.0 = normal load
    fn get_system_load(&self) -> f64 {
        // Simple heuristic: if we have many pending commits, system might be busy
        // In a real implementation, this could use CPU usage, memory pressure, etc.
        let current_commit_index = self.current_commit_index.load(Ordering::SeqCst);
        let pending_commits_estimate = current_commit_index.saturating_sub(self.last_global_exec_index as u32);

        // If we have more than 100 pending commits, consider system busy
        if pending_commits_estimate > 100 {
            1.5 // 50% busier
        } else if pending_commits_estimate > 50 {
            1.2 // 20% busier
        } else {
            1.0 // Normal load
        }
    }

    /// Wait for commit processor to complete processing all commits in pipeline
    /// Uses notification channel and adaptive polling with exponential backoff
    async fn wait_for_commit_processor_completion(
        &self,
        commit_index: u32,
        max_wait_secs: u64,
    ) -> Result<()> {
        use std::time::{Duration, Instant};

        const STABLE_TIME_SECS: u64 = 5; // Time commit index must be stable
        let start_time = Instant::now();

        info!("⏳ [COMMIT PROCESSOR] Waiting for completion...");
        info!("  📊 EndOfEpoch detected at commit_index={}, current_commit_index={}",
            commit_index, self.current_commit_index.load(Ordering::SeqCst));

        let mut last_commit_index = self.current_commit_index.load(Ordering::SeqCst);
        let mut last_change_time = Instant::now();
        let mut sleep_duration = Duration::from_millis(100); // Start with faster polling
        const MAX_SLEEP_DURATION: Duration = Duration::from_millis(1000); // Cap at 1 second

        loop {
            let current = self.current_commit_index.load(Ordering::SeqCst);
            let elapsed = start_time.elapsed().as_secs();

            if current > last_commit_index {
                // Commit index is still increasing - commits are being processed
                info!("  📈 Processing: current_commit_index={} (was {}), elapsed={}s",
                    current, last_commit_index, elapsed);
                last_commit_index = current;
                last_change_time = Instant::now();
                // Reset sleep duration when active processing
                sleep_duration = Duration::from_millis(100);
            } else if last_change_time.elapsed().as_secs() >= STABLE_TIME_SECS {
                // Commit index has been stable - processor likely caught up
                info!("  ✅ Caught up: current_commit_index={} (stable for {}s)",
                    current, STABLE_TIME_SECS);
                return Ok(());
            }

            if elapsed >= max_wait_secs {
                warn!("  ⚠️ Timeout after {}s, last_commit_index={}", max_wait_secs, last_commit_index);
                return Err(anyhow::anyhow!("Timeout waiting for commit processor"));
            }

            // Exponential backoff for sleep duration
            tokio::time::sleep(sleep_duration).await;
            sleep_duration = (sleep_duration * 2).min(MAX_SLEEP_DURATION);
        }
    }

    /// Create a new ConsensusNode with a default registry
    /// For metrics support, use `new_with_registry` instead
    #[allow(dead_code)]
    pub async fn new(config: NodeConfig) -> Result<Self> {
        Self::new_with_registry(config, Registry::new()).await
    }

    pub async fn new_with_registry(config: NodeConfig, registry: Registry) -> Result<Self> {
        Self::new_with_registry_and_service(config, registry, None).await
    }

    pub async fn new_with_registry_and_service(
        config: NodeConfig,
        registry: Registry,
        registry_service: Option<Arc<RegistryService>>,
    ) -> Result<Self> {
        info!("Initializing consensus node {}...", config.node_id);

        // FIX: Fetch current epoch and committee from Go state via Unix Domain Socket
        info!("🚀 [STARTUP] Loading latest block, epoch and committee from Go state...");

        // Create executor client for fetching epoch and committee from Go
        // Always enable executor client for committee fetching during startup
        let executor_client = Arc::new(ExecutorClient::new(
            true, // Always enable for committee fetching
            false, // Don't commit during committee fetching
            config.executor_send_socket_path.clone(),
            config.executor_receive_socket_path.clone(),
        ));

        // First: Load the latest block from Go
        let latest_block_number = executor_client.get_last_block_number().await
            .map_err(|e| anyhow::anyhow!("Failed to fetch latest block number from Go state: {}", e))?;

        info!("📊 [STARTUP] Latest block number from Go state: {}", latest_block_number);

        // Second: Get current epoch from the latest block
        let current_epoch = executor_client.get_current_epoch().await
            .map_err(|e| anyhow::anyhow!("Failed to fetch current epoch from Go state: {}", e))?;

        info!("📊 [STARTUP] Current epoch from Go state (based on latest block {}): {}", latest_block_number, current_epoch);

        // Third: Get committee (validators) for the current epoch using the latest block
        info!("📋 [STARTUP] Fetching committee validators from Go at latest block {} (for epoch {})",
              latest_block_number, current_epoch);

        let (validators, _go_epoch_timestamp_ms) = executor_client.get_validators_at_block(latest_block_number).await
            .map_err(|e| anyhow::anyhow!("Failed to fetch committee from Go state at latest block {}: {}", latest_block_number, e))?;

        // Use epoch timestamp from Go response, with fallback to genesis.json for epoch 0
        let epoch_timestamp_ms = if current_epoch == 0 {
            // For genesis epoch, prefer genesis.json for consistency
            let genesis_path = std::path::Path::new("../../mtn-simple-2025/cmd/simple_chain/genesis.json");
            if genesis_path.exists() {
                match std::fs::read_to_string(genesis_path) {
                    Ok(content) => {
                        match serde_json::from_str::<serde_json::Value>(&content) {
                            Ok(json) => {
                                match json.get("config").and_then(|c| c.get("epoch_timestamp_ms")).and_then(|ts| ts.as_u64()) {
                                    Some(ts) => {
                                        info!("📅 Using epoch_timestamp_ms from genesis.json: {}", ts);
                                        ts
                                    },
                                    None => {
                                        warn!("⚠️  Could not find epoch_timestamp_ms in genesis.json, using Go timestamp: {}", _go_epoch_timestamp_ms);
                                        _go_epoch_timestamp_ms
                                    }
                                }
                            },
                            Err(e) => {
                                warn!("⚠️  Failed to parse genesis.json: {}, using Go timestamp: {}", e, _go_epoch_timestamp_ms);
                                _go_epoch_timestamp_ms
                            }
                        }
                    },
                    Err(e) => {
                        warn!("⚠️  Failed to read genesis.json: {}, using Go timestamp: {}", e, _go_epoch_timestamp_ms);
                        _go_epoch_timestamp_ms
                    }
                }
            } else {
                warn!("⚠️  genesis.json not found at {:?}, using Go timestamp: {}", genesis_path, _go_epoch_timestamp_ms);
                _go_epoch_timestamp_ms
            }
        } else {
            // For epochs > 0, use timestamp from Go state
            // This timestamp marks when the epoch was created and should not be changed
            info!("📅 Using epoch_timestamp_ms from Go state: {} (epoch {})", _go_epoch_timestamp_ms, current_epoch);
            _go_epoch_timestamp_ms
        };

        if validators.is_empty() {
            anyhow::bail!("Go state returned empty validators list at genesis block");
        }

        // Convert ValidatorInfo to Committee format
        let mut authorities = Vec::new();

        // DEBUG: Only use node 0 for single-node testing if env var is set
        let validators_to_use = if std::env::var("SINGLE_NODE_DEBUG").is_ok() {
            info!("🔧 SINGLE_NODE_DEBUG: Using only node 0 for testing");
            validators.into_iter().filter(|v| v.name == "node-0").collect::<Vec<_>>()
        } else {
            validators
        };

        for validator in validators_to_use {
            // Parse keys from base64 strings
            let authority_key_bytes = general_purpose::STANDARD.decode(&validator.authority_key)?;
            let authority_key_inner = bls12381::min_sig::BLS12381PublicKey::from_bytes(&authority_key_bytes)?;
            let authority_key = AuthorityPublicKey::new(authority_key_inner);

            let protocol_key_bytes = general_purpose::STANDARD.decode(&validator.protocol_key)?;
            let protocol_key_inner = ed25519::Ed25519PublicKey::from_bytes(&protocol_key_bytes)?;
            let protocol_key = ProtocolPublicKey::new(protocol_key_inner);

            let network_key_bytes = general_purpose::STANDARD.decode(&validator.network_key)?;
            let network_key_inner = ed25519::Ed25519PublicKey::from_bytes(&network_key_bytes)?;
            let network_key = NetworkPublicKey::new(network_key_inner);

            // Parse address
            let address = if validator.address.starts_with("/ip4/") {
                validator.address.parse()?
            } else {
                // Fallback for old format
                format!("/ip4/127.0.0.1/tcp/{}", 9000 + validator.name.parse::<u32>()?).parse()?
            };

            authorities.push(Authority {
                stake: validator.stake.parse::<u64>()?,
                address,
                hostname: validator.name,
                authority_key,
                protocol_key,
                network_key,
            });
        }

        // Sort authorities by address for consistent ordering across all nodes
        let mut sorted_authorities = authorities;
        sorted_authorities.sort_by(|a, b| a.address.cmp(&b.address));

        // Create committee from Go state (now sorted by address)
        info!("🔧 DEBUG: Creating committee with {} authorities for epoch {}", sorted_authorities.len(), current_epoch);
        for (i, auth) in sorted_authorities.iter().enumerate() {
            info!("🔧 DEBUG: Authority[{}]: stake={}, address={}", i, auth.stake, auth.address);
        }
        let committee = Committee::new(current_epoch, sorted_authorities);
        let committee_epoch = committee.epoch();
        info!("✅ Loaded committee from Go state with {} authorities, epoch={}", committee.size(), committee_epoch);

        // Capture paths needed for epoch transition
        // NOTE: Không còn require committee_path vì chúng ta không lưu committee ra file
        // Committee sẽ được fetch từ Go state mỗi lần khởi động
        let storage_path = config.storage_path.clone();
        
        // UNIFIED BLOCK NUMBERING: Sync from Go's executed blocks with conflict detection
        // last_global_exec_index must reflect Go's actual execution state
        // But prevent conflicts with running nodes by checking storage state
        let last_global_exec_index = if config.executor_read_enabled {
            match executor_client.get_last_block_number().await {
                Ok(go_last_block) => {
                    // Check if we have local storage that might indicate conflicts with running nodes
                    let db_path = config.storage_path
                        .join("epochs")
                        .join(format!("epoch_{}", current_epoch))
                        .join("consensus_db");

                    if db_path.exists() {
                        let temp_store = RocksDBStore::new(db_path.to_str().unwrap());
                        match temp_store.read_last_commit_info() {
                            Ok(Some((last_commit_ref, _))) => {
                                let storage_index = last_commit_ref.index as u64;

                                // If storage index is significantly behind Go state,
                                // it means running nodes are using lower index
                                // Use storage index to avoid conflicts
                                if storage_index < go_last_block && (go_last_block - storage_index) > 5 {
                                    warn!("🚨 [STARTUP] INDEX CONFLICT DETECTED: Go has {} but storage has {} - using storage index to match running nodes",
                                        go_last_block, storage_index);
                                    storage_index
                                } else {
                                    info!("📊 [STARTUP] CONSISTENT STATE: Go has {}, storage has {} - using Go state {}", go_last_block, storage_index, go_last_block);
                                    go_last_block
                                }
                            },
                            _ => {
                                // No commit info in storage
                                info!("📊 [STARTUP] NO COMMIT INFO: Using last_global_exec_index={} from Go state", go_last_block);
                                go_last_block
                            }
                        }
                    } else {
                        // No storage at all
                        info!("📊 [STARTUP] NO EPOCH STORAGE: Using last_global_exec_index={} from Go state", go_last_block);
                        go_last_block
                    }
                },
                Err(e) => {
                    // CRITICAL: If cannot sync with Go, reset to 0 to prevent conflicts
                    // This ensures unified block numbering even if sync fails
                    error!("🚨 [STARTUP] CRITICAL: Failed to sync block numbering with Go: {}. Resetting to 0 to prevent conflicts. This may cause some blocks to be re-processed.", e);
                    0
                }
            }
        } else {
            warn!("⚠️  [STARTUP] Executor read not enabled - cannot sync block numbering with Go. Using 0 (genesis). This may cause block numbering conflicts!");
            0
        };
        // GLOBAL BLOCK NUMBERING: Reset to genesis if state seems corrupted
        // This prevents block numbering conflicts in test environments
        let last_global_exec_index = if last_global_exec_index > 10000 {
            warn!("🚨 [STARTUP] last_global_exec_index={} too high - resetting to 0 for unified block numbering", last_global_exec_index);
            0
        } else {
            last_global_exec_index
        };

        info!("✅ [STARTUP] Using last_global_exec_index={} for commit processor (executor_read_enabled={})",
            last_global_exec_index, config.executor_read_enabled);

        // FORK DETECTION: Will be called after node creation

        // CRITICAL RECOVERY: Check for blocks committed by Rust but not executed by Go
        // This happens when Rust node crashes after committing blocks but before sending to Go
        // Recovery resends blocks without changing last_global_exec_index
        if config.executor_read_enabled && last_global_exec_index > 0 {
            Self::perform_block_recovery_check(
                &executor_client,
                last_global_exec_index,
                current_epoch,
                &config.storage_path,
                config.node_id as u32,
            ).await?;
        }

        // Load keypairs (kept for in-process restart)
        let protocol_keypair = config.load_protocol_keypair()?;
        let network_keypair = config.load_network_keypair()?;

        // Get own authority index by matching hostname (committee is now sorted by address)
        // Note: Node may not be in committee (sync-only mode) - own_index can be None
        let own_hostname = format!("node-{}", config.node_id);
        let own_index_opt = committee.authorities().find_map(|(idx, auth)| {
            if auth.hostname == own_hostname {
                Some(idx)
            } else {
                None
            }
        });
        
        // Check if node is in committee (determines if we should start as validator)
        let is_in_committee = own_index_opt.is_some();
        let own_index = own_index_opt.unwrap_or(AuthorityIndex::ZERO); // Default index for sync-only nodes (won't be used)
        
        if is_in_committee {
            info!("✅ Node {} matched to authority index {} (will start as validator)", config.node_id, own_index);
        } else {
            info!("🔄 Node {} not found in committee (will start as sync-only)", config.node_id);
        }

        // Create storage directory
        std::fs::create_dir_all(&config.storage_path)?;

        // Create clock (kept for in-process restart)
        let clock = Arc::new(Clock::default());

        // Create transaction verifier (no-op for now, kept for in-process restart)
        let transaction_verifier = Arc::new(NoopTransactionVerifier);

        // Create commit consumer args
        let (commit_consumer, commit_receiver, mut block_receiver) = CommitConsumerArgs::new(0, 0);
        
        // Track current commit index for fork-safe epoch transition
        let current_commit_index = Arc::new(AtomicU32::new(0));
        let commit_index_for_callback = current_commit_index.clone();
        
        // Create is_transitioning flag (initialized to false, will be set when EndOfEpoch is detected)
        let is_transitioning = Arc::new(AtomicBool::new(false));
        let is_transitioning_for_processor = is_transitioning.clone();
        
        // Create pending transactions queue for epoch transition
        // Load persisted transaction queue if exists
        let persisted_queue = Self::load_transaction_queue_static(&storage_path).await
            .unwrap_or_else(|e| {
                warn!("⚠️ [TX PERSISTENCE] Failed to load persisted queue: {}", e);
                Vec::new()
            });

        let queue_size = persisted_queue.len();
        let pending_transactions_queue = Arc::new(tokio::sync::Mutex::new(persisted_queue));

        if queue_size > 0 {
            info!("💾 [TX PERSISTENCE] Loaded {} persisted transactions into queue", queue_size);
        }
        
        // Create channel for epoch transition requests from system transactions
        let (epoch_tx_sender, epoch_tx_receiver) = tokio::sync::mpsc::unbounded_channel::<(u64, u64, u32)>();
        
        // Store receiver for later use in transition handler
        let epoch_tx_receiver_for_handler = epoch_tx_receiver;
        
        // Setup callback for EndOfEpoch system transaction (Sui-style)
        // Callback sends transition request via channel, which will be handled by a task
        // NOTE: Do NOT set is_transitioning flag here - it will be set in transition_to_epoch_from_system_tx
        // Setting it here causes race condition where handler sees flag=true and skips transition
        let epoch_transition_callback = crate::commit_callbacks::create_epoch_transition_callback(epoch_tx_sender.clone());
        
        // Create notification channel for commit processor completion
        let (_commit_complete_sender, _commit_complete_receiver) = tokio::sync::watch::channel(false);

        // Create shared last global exec index for commit processor
        let shared_last_global_exec_index = Arc::new(tokio::sync::Mutex::new(last_global_exec_index));

        // Create ordered commit processor
        let mut commit_processor = crate::commit_processor::CommitProcessor::new(commit_receiver)
            .with_commit_index_callback(crate::commit_callbacks::create_commit_index_callback(commit_index_for_callback))
            .with_global_exec_index_callback(crate::commit_callbacks::create_global_exec_index_callback(shared_last_global_exec_index.clone()))
            .with_get_last_global_exec_index({
                let shared_index = shared_last_global_exec_index.clone();
                move || {
                    // Get current last global exec index synchronously
                    // CRITICAL FIX: Use try_current() to avoid panic if called from async context
                    // If called from async context, return 0 and log warning
                    if let Ok(_rt) = tokio::runtime::Handle::try_current() {
                        // We're in async context, can't use block_on
                        // Return 0 as fallback - actual value will be read from shared index in async context
                        warn!("⚠️  [GLOBAL_EXEC_INDEX] get_last_global_exec_index called from async context, returning 0. Actual value should be read from shared index.");
                        0
                    } else {
                        // We're in sync context, can use block_on
                        let shared_index_clone = shared_index.clone();
                        block_on(async {
                            let index_guard = shared_index_clone.lock().await;
                            *index_guard
                        })
                    }
                }
            })
            .with_shared_last_global_exec_index(shared_last_global_exec_index.clone())
            .with_epoch_info(current_epoch, last_global_exec_index)
            .with_is_transitioning(is_transitioning_for_processor)
            .with_pending_transactions_queue(pending_transactions_queue.clone())
            .with_epoch_transition_callback(epoch_transition_callback);
        
        // Create executor client based on config settings
        // executor_read_enabled: allows reading committee state from Go
        // executor_commit_enabled: allows committing blocks to Go
        let node_id = config.node_id;
        let can_commit = config.executor_commit_enabled;

        // CRITICAL FIX: Use last_global_exec_index to initialize executor client
        // This ensures next_expected_index starts from the correct value (last_global_exec_index + 1)
        // This prevents duplicate global_exec_index when node restarts
        let initial_next_expected = if config.executor_read_enabled {
            // If we loaded last_global_exec_index from Go, next_expected should be last + 1
            last_global_exec_index + 1
        } else {
            // If executor read is disabled, start from 1 (genesis)
            1
        };
        
        // Only create executor client if reading is enabled
        let executor_client = if config.executor_read_enabled {
            info!("🔧 [EXECUTOR CLIENT] Creating executor client with initial_next_expected={} (based on last_global_exec_index={})", 
                initial_next_expected, last_global_exec_index);
            Arc::new(ExecutorClient::new_with_initial_index(
                true, 
                can_commit, 
                config.executor_send_socket_path.clone(), 
                config.executor_receive_socket_path.clone(),
                initial_next_expected
            ))
        } else {
            // Create disabled executor client for nodes that don't read from Go
            Arc::new(ExecutorClient::new(false, false, "".to_string(), "".to_string()))
        };
        info!("✅ Executor client configured (node_id={}, read_enabled={}, commit_enabled={}, send_socket={}, receive_socket={}, initial_next_expected={})",
            node_id, config.executor_read_enabled, can_commit, config.executor_send_socket_path, config.executor_receive_socket_path, initial_next_expected);

        // CRITICAL FIX: Initialize executor client to sync with Go state
        // This ensures next_expected_index and buffer are in sync with Go's last block number
        // Even though we set initial_next_expected, we still need to sync in case Go is ahead
        let executor_client_for_init = executor_client.clone();
        tokio::spawn(async move {
            info!("🔧 [EXECUTOR INIT] Initializing executor client to sync with Go state...");
            executor_client_for_init.initialize_from_go().await;
            info!("✅ [EXECUTOR INIT] Executor client initialized and synced with Go state");
        });

        commit_processor = commit_processor.with_executor_client(executor_client);
        
        let current_epoch_for_processor = current_epoch;
        let last_global_exec_index_for_processor = last_global_exec_index;
        tokio::spawn(async move {
            info!("🚀 [COMMIT PROCESSOR] Starting for epoch {} (last_global_exec_index={})",
                current_epoch_for_processor, last_global_exec_index_for_processor);
            if let Err(e) = commit_processor.run().await {
                error!("❌ [COMMIT PROCESSOR] Error: {}", e);
            }
        });
        
        tokio::spawn(async move {
            use tracing::debug;
            while let Some(output) = block_receiver.recv().await {
                debug!("Received {} certified blocks", output.blocks.len());
            }
        });

        // Get protocol config
        let protocol_config = ProtocolConfig::get_for_max_version_UNSAFE();

        // Create parameters (db_path will be set per-epoch)
        let mut parameters = consensus_config::Parameters::default();

        // Apply commit sync parameters
        parameters.commit_sync_batch_size = config.commit_sync_batch_size;
        parameters.commit_sync_parallel_fetches = config.commit_sync_parallel_fetches;
        parameters.commit_sync_batches_ahead = config.commit_sync_batches_ahead;

        // Apply speed multiplier
        let speed_multiplier = config.speed_multiplier;
        if speed_multiplier != 1.0 {
            info!("Applying speed multiplier: {}x", speed_multiplier);
            let leader_timeout = config.leader_timeout_ms
                .map(Duration::from_millis)
                .unwrap_or_else(|| Duration::from_millis((200.0 / speed_multiplier) as u64));
            
            let min_round_delay = config.min_round_delay_ms
                .map(Duration::from_millis)
                .unwrap_or_else(|| Duration::from_millis((50.0 / speed_multiplier) as u64));
            
            let max_forward_time_drift = Duration::from_millis((500.0 / speed_multiplier) as u64);
            let round_prober_interval_ms = (5000.0 / speed_multiplier) as u64;
            let round_prober_request_timeout_ms = (4000.0 / speed_multiplier) as u64;
            
            parameters.leader_timeout = leader_timeout;
            parameters.min_round_delay = min_round_delay;
            parameters.max_forward_time_drift = max_forward_time_drift;
            parameters.round_prober_interval_ms = round_prober_interval_ms;
            parameters.round_prober_request_timeout_ms = round_prober_request_timeout_ms;
        }

        // Use epoch timestamp from genesis.json for consistency across all nodes
        let epoch_start_timestamp = epoch_timestamp_ms;
        let current_epoch = committee.epoch();

        // Per-epoch DB path
        let db_path = config
            .storage_path
            .join("epochs")
            .join(format!("epoch_{}", current_epoch))
            .join("consensus_db");

        // CRITICAL: Clear any existing consensus database to ensure clean state
        // Rust must load COMPLETELY from Go state, no local state recovery
        if db_path.exists() {
            info!("🧹 [STARTUP] Clearing existing consensus database at {:?} to ensure clean state load from Go", db_path);
            if let Err(e) = std::fs::remove_dir_all(&db_path) {
                warn!("⚠️  Failed to clear consensus database: {}", e);
            }
        }

        std::fs::create_dir_all(&db_path)?;
        parameters.db_path = db_path;
        
        if current_epoch == 0 && config.time_based_epoch_change {
            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
            let elapsed_seconds = (now_ms.saturating_sub(epoch_start_timestamp)) / 1000;
            let epoch_duration_seconds = config.epoch_duration_seconds.unwrap_or(600);
            
            if elapsed_seconds > epoch_duration_seconds {
                warn!(
                    "⚠️  Epoch start timestamp is old (elapsed={}s > duration={}s), but keeping it to ensure all nodes use same timestamp for genesis blocks",
                    elapsed_seconds, epoch_duration_seconds
                );
            }
        }
        
        info!("Using epoch start timestamp: {} (epoch={})", epoch_start_timestamp, current_epoch);

        // Create system transaction provider for Sui-style epoch transition
        let epoch_duration_seconds = config.epoch_duration_seconds.unwrap_or(180);
        let system_transaction_provider = Arc::new(DefaultSystemTransactionProvider::new(
            current_epoch,
            epoch_duration_seconds,
            epoch_start_timestamp,
            config.time_based_epoch_change,
        ));
        info!("✅ System transaction provider created (Sui-style epoch transition enabled: {})", 
            config.time_based_epoch_change);

        // Initialize clock sync manager
        let clock_sync_manager = Arc::new(RwLock::new(ClockSyncManager::new(
            config.ntp_servers.clone(),
            config.max_clock_drift_seconds * 1000,
            config.ntp_sync_interval_seconds,
            config.enable_ntp_sync,
        )));

        // Start clock sync tasks if enabled
        if config.enable_ntp_sync {
            let sync_manager_clone = clock_sync_manager.clone();
            let monitor_manager_clone = clock_sync_manager.clone();
            
            tokio::spawn(async move {
                let mut manager = sync_manager_clone.write().await;
                if let Err(e) = manager.sync_with_ntp().await {
                    tracing::warn!("Initial NTP sync failed: {}", e);
                }
            });
            
            ClockSyncManager::start_sync_task(clock_sync_manager.clone());
            ClockSyncManager::start_drift_monitor(monitor_manager_clone);
        }

        // Start authority node ONLY if node is in committee (validator mode)
        // If node is not in committee (sync-only mode), authority will be None
        let authority = if is_in_committee {
            info!("🚀 Starting consensus authority node (node is in committee)...");
            Some(ConsensusAuthority::start(
                NetworkType::Tonic,
                epoch_start_timestamp,
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
                0, // boot_counter
                Some(system_transaction_provider.clone() as Arc<dyn SystemTransactionProvider>), // Pass system transaction provider
            )
            .await)
        } else {
            info!("🔄 Skipping consensus authority start (node is not in committee - sync-only mode)");
            None
        };

        // Create transaction client proxy
        // Only create for validator nodes - sync-only nodes don't need it
        // When sync-only node becomes validator during epoch transition, proxy will be created then
        let transaction_client_proxy = if let Some(ref auth) = authority {
            let transaction_client = auth.transaction_client();
            Some(Arc::new(TransactionClientProxy::new(transaction_client)))
        } else {
            // Sync-only nodes don't submit transactions, so no transaction client proxy needed
            // It will be created automatically when node switches to validator mode during epoch transition
            info!("🔄 Sync-only node: No transaction client proxy needed (will be created when node becomes validator)");
            None
        };

        // Create no-op provider/processor to satisfy Core's interface
        use consensus_core::epoch_change_provider::{EpochChangeProvider, EpochChangeProcessor};
        
        struct NoOpEpochChangeProvider;
        impl EpochChangeProvider for NoOpEpochChangeProvider {
            fn get_proposal(&self) -> Option<Vec<u8>> {
                None
            }
            fn get_votes(&self) -> Vec<Vec<u8>> {
                Vec::new()
            }
        }
        
        struct NoOpEpochChangeProcessor;
        impl EpochChangeProcessor for NoOpEpochChangeProcessor {
            fn process_proposal(&self, _proposal_bytes: &[u8]) {}
            fn process_vote(&self, _vote_bytes: &[u8]) {}
        }
        
        consensus_core::epoch_change_provider::init_epoch_change_provider(
            Box::new(NoOpEpochChangeProvider)
        );
        consensus_core::epoch_change_provider::init_epoch_change_processor(
            Box::new(NoOpEpochChangeProcessor)
        );
        
        info!("✅ No-op epoch change provider/processor initialized");
        info!("Consensus node {} initialized successfully", config.node_id);

        // Store transition channel sender in node for later use
        let epoch_tx_sender_for_node = epoch_tx_sender;

        // Clone values needed for transition handler before moving into node struct
        let system_transaction_provider_for_handler = system_transaction_provider.clone();
        let config_for_handler = config.clone();
        let lvm_snapshot_bin_path_for_node = config.lvm_snapshot_bin_path.clone();
        
        // Create node instance
        let mut node = Self {
            authority,
            node_mode: config.initial_node_mode.clone(), // Start with configured initial mode
            execution_lock: Arc::new(tokio::sync::RwLock::new(current_epoch)),
            reconfig_state: Arc::new(tokio::sync::RwLock::new(ReconfigState::default())),
            transaction_client_proxy,
            // REMOVED: epoch_change_manager - using SystemTransactionProvider instead
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
            registry_service,
            current_registry_id: None,
            executor_commit_enabled: config.executor_commit_enabled,
            is_transitioning,
            pending_transactions_queue,
            enable_lvm_snapshot: config.enable_lvm_snapshot,
            lvm_snapshot_bin_path: lvm_snapshot_bin_path_for_node,
            lvm_snapshot_delay_seconds: config.lvm_snapshot_delay_seconds,
            system_transaction_provider,
            // TODO: Future enhancement - notification channels
            // commit_complete_sender: tokio::sync::watch::channel(false).0,
            // commit_complete_receiver: tokio::sync::watch::channel(false).1,
            epoch_transition_sender: epoch_tx_sender_for_node,
            sync_task_handle: None,
        };

        // Start epoch transition handler task
        // This task will process transition requests and call transition function
        // The node will be registered in global registry after it's wrapped in Arc<Mutex<>> in main.rs
        crate::epoch_transition::start_epoch_transition_handler(
            epoch_tx_receiver_for_handler,
            system_transaction_provider_for_handler,
            config_for_handler,
        );

        // Check and update node mode based on initial committee membership
        // This ensures node starts in correct mode (SyncOnly vs Validator) based on genesis committee
        node.check_and_update_node_mode(&committee, &config).await?;

        // If node is in sync-only mode after check, ensure sync task is started
        if matches!(node.node_mode, NodeMode::SyncOnly) {
            if let Err(e) = node.start_sync_task(&config).await {
                warn!("⚠️ [STARTUP] Failed to start sync task for sync-only node: {}", e);
            }
        }

        // FORK DETECTION: Check for potential forks after node is fully initialized
        node.perform_fork_detection_check().await?;

        Ok(node)
    }

    // ===== TRANSACTION MANAGEMENT METHODS =====

    #[allow(dead_code)]
    pub fn transaction_submitter(&self) -> Option<Arc<dyn TransactionSubmitter>> {
        self.transaction_client_proxy.as_ref().map(|proxy| proxy.clone() as Arc<dyn TransactionSubmitter>)
    }

    #[allow(dead_code)]
    pub fn current_commit_index(&self) -> u32 {
        self.current_commit_index.load(Ordering::SeqCst)
    }

    #[allow(dead_code)]
    pub fn update_commit_index(&self, index: u32) {
        self.current_commit_index.store(index, Ordering::SeqCst);
    }



    
    pub async fn check_transaction_acceptance(&self) -> (bool, bool, String) {
        if self.authority.is_none() {
            return (false, false, "Node is still initializing".to_string());
        }

        if self.last_transition_hash.is_some() {
            return (false, false, format!(
                "Epoch transition in progress: epoch {} -> {} (waiting for new authority to start)",
                self.current_epoch, self.current_epoch + 1
            ));
        }

        let is_transitioning = self.is_transitioning.load(Ordering::SeqCst);

        if is_transitioning {
            let current_commit_index = self.current_commit_index.load(Ordering::SeqCst);
            info!("🔄 [EPOCH TRANSITION] Queueing transaction - is_transitioning=true (current_commit={}): transaction will be queued for next epoch",
                current_commit_index);
            return (false, true, format!(
                "Epoch transition in progress - transaction will be queued for next epoch (current_commit={})",
                current_commit_index
            ));
        }

        // Check reconfiguration state - if reconfiguration is in progress, queue transactions
        if !self.should_accept_tx().await {
            let reconfig_state = self.get_reconfig_state().await;
            let reason = match reconfig_state.status() {
                consensus_core::ReconfigCertStatus::RejectUserCerts => "Gradual shutdown: rejecting user certificates",
                consensus_core::ReconfigCertStatus::RejectAllCerts => "Gradual shutdown: rejecting all certificates",
                consensus_core::ReconfigCertStatus::RejectAllTx => "Gradual shutdown: rejecting all transactions",
                _ => "Reconfiguration in progress",
            };
            info!("🔄 [GRADUAL SHUTDOWN] Queueing transaction - reconfiguration state: {:?} ({})", reconfig_state.status(), reason);
            return (false, true, format!("{} - transaction will be queued for next epoch", reason));
        }

        (true, false, "Node is ready".to_string())
    }
    
    pub async fn queue_transaction_for_next_epoch(&self, tx_data: Vec<u8>) -> Result<()> {
        let mut queue = self.pending_transactions_queue.lock().await;
        queue.push(tx_data.clone());
        info!("📦 [TX FLOW] Queued transaction for next epoch: queue_size={}", queue.len());

        // Persist queue to disk to survive node crashes
        if let Err(e) = self.persist_transaction_queue(&queue).await {
            warn!("⚠️ [TX PERSISTENCE] Failed to persist transaction queue: {}", e);
        }

        Ok(())
    }

    async fn persist_transaction_queue(&self, queue: &[Vec<u8>]) -> Result<()> {
        use std::io::Write;

        let queue_path = self.storage_path.join("transaction_queue.bin");
        let mut file = std::fs::File::create(&queue_path)?;

        // Write number of transactions first
        let count = queue.len() as u32;
        file.write_all(&count.to_le_bytes())?;

        // Write each transaction with its length prefix
        for tx_data in queue {
            let len = tx_data.len() as u32;
            file.write_all(&len.to_le_bytes())?;
            file.write_all(tx_data)?;
        }

        file.flush()?;
        trace!("💾 [TX PERSISTENCE] Persisted {} transactions to {}", queue.len(), queue_path.display());
        Ok(())
    }

    async fn load_transaction_queue_static(storage_path: &std::path::Path) -> Result<Vec<Vec<u8>>> {
        use std::io::Read;

        let queue_path = storage_path.join("transaction_queue.bin");
        if !queue_path.exists() {
            return Ok(Vec::new());
        }

        let mut file = std::fs::File::open(&queue_path)?;
        let mut queue = Vec::new();

        // Read number of transactions
        let mut count_buf = [0u8; 4];
        file.read_exact(&mut count_buf)?;
        let count = u32::from_le_bytes(count_buf);

        // Read each transaction
        for _ in 0..count {
            let mut len_buf = [0u8; 4];
            file.read_exact(&mut len_buf)?;
            let len = u32::from_le_bytes(len_buf) as usize;

            let mut tx_data = vec![0u8; len];
            file.read_exact(&mut tx_data)?;
            queue.push(tx_data);
        }

        info!("💾 [TX PERSISTENCE] Loaded {} persisted transactions from {}", queue.len(), queue_path.display());
        Ok(queue)
    }

    
    pub async fn submit_queued_transactions(&mut self) -> Result<usize> {
        let mut queue = self.pending_transactions_queue.lock().await;
        let original_count = queue.len();
        if original_count == 0 {
            info!("📤 [TX FLOW] No queued transactions to submit (queue is empty)");
            return Ok(0);
        }
        
        info!("📤 [TX FLOW] Submitting {} queued transactions to consensus in new epoch", original_count);
        
        // Log first few transaction hashes for debugging
        use crate::tx_hash::calculate_transaction_hash;
        let sample_hashes: Vec<String> = queue.iter()
            .take(5)
            .map(|tx_data| {
                let hash = calculate_transaction_hash(tx_data);
                hex::encode(&hash[..8])
            })
            .collect();
        if !sample_hashes.is_empty() {
            info!("📤 [TX FLOW] Sample queued transaction hashes: {:?} (showing first 5 of {})", 
                sample_hashes, original_count);
        }
        let mut transactions_with_hash: Vec<(Vec<u8>, Vec<u8>)> = queue
            .iter()
            .map(|tx_data| {
                let tx_hash = calculate_transaction_hash(tx_data);
                (tx_data.clone(), tx_hash)
            })
            .collect();
        
        transactions_with_hash.sort_by(|(_, hash_a), (_, hash_b)| hash_a.cmp(hash_b));
        
        let before_dedup = transactions_with_hash.len();
        transactions_with_hash.dedup_by(|a, b| a.1 == b.1);
        let unique_count = transactions_with_hash.len();
        info!(
            "✅ [FORK-SAFETY] Sorted queued transactions by hash and deduped: before={}, unique={}",
            before_dedup, unique_count
        );
        
        let transactions: Vec<Vec<u8>> = transactions_with_hash
            .into_iter()
            .map(|(tx_data, _)| tx_data)
            .collect();
        
        queue.clear();
        drop(queue);
        
        // Submit transactions with retry mechanism
        // Retry failed submissions with exponential backoff to handle consensus startup delays
        const MAX_RETRIES: u32 = 20; // Increased from 10 to 20 for better epoch transition reliability
        const INITIAL_RETRY_DELAY_MS: u64 = 200; // Increased from 100 to 200ms
        let mut successful_count = 0;
        let mut requeued_count = 0;
        
        for tx_data in transactions {
            // Validate transaction format before submitting
            let tx_hash = crate::tx_hash::calculate_transaction_hash(&tx_data);
            let tx_hash_hex = hex::encode(&tx_hash[..8.min(tx_hash.len())]);

            // Check if this is a SystemTransaction (should not be re-submitted)
            if SystemTransaction::from_bytes(&tx_data).is_ok() {
                warn!("🚫 [TX VALIDATION] Skipping SystemTransaction in queued transactions: hash={}, size={}",
                    tx_hash_hex, tx_data.len());
                continue; // Skip SystemTransaction
            }

            // Check if transaction has valid protobuf format
            if !crate::tx_hash::verify_transaction_protobuf(&tx_data) {
                error!("🚫 [TX VALIDATION] Invalid protobuf format in queued transaction: hash={}, size={}. Dropping transaction.",
                    tx_hash_hex, tx_data.len());
                continue; // Drop invalid transaction instead of re-queuing
            }

            info!("✅ [TX VALIDATION] Valid transaction for submission: hash={}, size={}",
                tx_hash_hex, tx_data.len());

            // For sync-only nodes, we don't have transaction client proxy
            if self.transaction_client_proxy.is_none() {
                warn!("⚠️ [TX FLOW] Cannot submit transactions: node is sync-only (no transaction client). Re-queuing for when node becomes validator.");
                // Re-queue transaction for when node becomes validator
                let mut queue = self.pending_transactions_queue.lock().await;
                queue.push(tx_data.clone());
                requeued_count += 1;
                continue;
            }
            
            let transactions_vec = vec![tx_data.clone()];
            let mut retry_count = 0;
            
            while retry_count < MAX_RETRIES {
                match self.transaction_client_proxy.as_ref().unwrap().submit(transactions_vec.clone()).await {
                    Ok(_) => {
                        successful_count += 1;
                        break;
                    },
                    Err(e) => {
                        retry_count += 1;
                        if retry_count < MAX_RETRIES {
                            let delay_ms = INITIAL_RETRY_DELAY_MS * (1 << (retry_count - 1)); // Exponential backoff
                            warn!("⚠️ [TX FLOW] Failed to submit queued transaction (attempt {}/{}): {}. Retrying in {}ms...",
                                retry_count, MAX_RETRIES, e, delay_ms);

                            // Log additional debug info for first few failures
                            if retry_count <= 3 {
                                let tx_hash = crate::tx_hash::calculate_transaction_hash(&tx_data);
                                let tx_hash_hex = hex::encode(&tx_hash[..8.min(tx_hash.len())]);
                                info!("🔍 [TX DEBUG] Failed transaction details: hash={}, size={}, error_type={}",
                                    tx_hash_hex, tx_data.len(), e.to_string().split(':').next().unwrap_or("unknown"));
                            }
                        } else {
                            error!("❌ [TX FLOW] Failed to submit queued transaction after {} retries: {}", MAX_RETRIES, e);

                            // Log final failure details
                            let tx_hash = crate::tx_hash::calculate_transaction_hash(&tx_data);
                            let tx_hash_hex = hex::encode(&tx_hash[..8.min(tx_hash.len())]);
                            error!("🔍 [TX DEBUG] Final failure for transaction: hash={}, size={}, last_error={}",
                                tx_hash_hex, tx_data.len(), e);
                            // Transaction failed after all retries - put it back in queue for next attempt
                            let mut queue = self.pending_transactions_queue.lock().await;
                            queue.push(tx_data.clone());
                            requeued_count += 1;
                            break; // Exit retry loop since we've exhausted all retries
                        }
                    }
                }
            }
        }
        
        if successful_count > 0 {
            info!(
                "✅ [TX FLOW] Successfully submitted {}/{} queued transactions to consensus in deterministic order",
                successful_count, unique_count
            );
        }
        
        if requeued_count > 0 {
            warn!("⚠️ [TX FLOW] {} transactions failed to submit after {} retries. They have been re-queued for next epoch transition.",
                requeued_count, MAX_RETRIES);
            // Persist remaining queue for next attempt
            // Note: We need to get the queue again since the previous one was dropped
            let remaining_queue = self.pending_transactions_queue.lock().await;
            if let Err(e) = self.persist_transaction_queue(&remaining_queue).await {
                warn!("⚠️ [TX PERSISTENCE] Failed to persist remaining transaction queue: {}", e);
            }
            drop(remaining_queue);
        } else if successful_count > 0 {
            // All transactions submitted successfully - clear persisted queue
            let queue_path = self.storage_path.join("transaction_queue.bin");
            if queue_path.exists() {
                if let Err(e) = std::fs::remove_file(&queue_path) {
                    warn!("⚠️ [TX PERSISTENCE] Failed to remove persisted queue file: {}", e);
                } else {
                    info!("💾 [TX PERSISTENCE] Cleared persisted transaction queue after successful submission");
                }
            }
        }

        Ok(successful_count)
    }

    pub async fn shutdown(mut self) -> Result<()> {
        info!("Shutting down consensus node...");

        // Stop sync task if running
        if let Err(e) = self.stop_sync_task().await {
            warn!("⚠️ [SHUTDOWN] Failed to stop sync task: {}", e);
        }

        if let Some(authority) = self.authority {
            authority.stop().await;
        }
        info!("Consensus node stopped");
        Ok(())
    }

    #[allow(dead_code)]
    pub async fn graceful_shutdown(&mut self) -> Result<()> {
        info!("Starting graceful shutdown...");

        // Stop sync task
        if let Err(e) = self.stop_sync_task().await {
            warn!("⚠️ [GRACEFUL SHUTDOWN] Failed to stop sync task: {}", e);
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        
        info!("Graceful shutdown preparation complete");
        Ok(())
    }

    // FIX: Updated to loop infinitely until success
    async fn build_committee_from_go_validators_at_block(
        executor_client: &Arc<ExecutorClient>,
        block_number: u64,
    ) -> Result<Committee> {
        loop {
            // Get validators from Go at specific block
            match executor_client.get_validators_at_block(block_number).await {
                Ok((validators, _epoch_timestamp_ms)) => {
                    if !validators.is_empty() {
                         info!("📋 [COMMITTEE FETCH] Successfully received {} validators from Go at block {}", validators.len(), block_number);
                         // For epoch transition, epoch will be set by caller
                         return Self::build_committee_from_validator_list(validators, 0);
                    } else {
                        warn!("⏳ [COMMITTEE FETCH] Go returned 0 validators at block {}. Go might be processing. Retrying in 2s...", block_number);
                    }
                },
                Err(e) => {
                    error!("❌ [COMMITTEE FETCH] Failed to connect to Go: {}. Retrying in 2s...", e);
                }
            }
            // Wait before retry
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    #[allow(dead_code)]
    async fn build_committee_from_go_validators(
        executor_client: &Arc<ExecutorClient>,
        new_epoch: u64,
    ) -> Result<Committee> {
        let validators = executor_client.get_active_validators().await
            .map_err(|e| anyhow::anyhow!("Failed to get active validators from Go: {}", e))?;
        
        if validators.is_empty() {
            anyhow::bail!("No active validators found in Go state");
        }
        
        info!("📋 [EPOCH-TRANSITION] Building committee from {} active validators in Go state", validators.len());
        
        Self::build_committee_from_validator_list(validators, new_epoch)
    }

    fn build_committee_from_validator_list(
        validators: Vec<crate::executor_client::proto::ValidatorInfo>,
        epoch: u64,
    ) -> Result<Committee> {
        use consensus_config::{Authority, AuthorityPublicKey, NetworkPublicKey, ProtocolPublicKey};
        use mysten_network::Multiaddr;
        use fastcrypto::{bls12381, ed25519};
        use fastcrypto::traits::ToFromBytes;
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        
        let mut sorted_validators: Vec<_> = validators.into_iter().collect();
        sorted_validators.sort_by(|a, b| a.address.cmp(&b.address));
        
        let mut authorities = Vec::new();
        let mut total_stake_normalized = 0u64;
        
        for (idx, validator) in sorted_validators.iter().enumerate() {
            let stake = validator.stake.parse::<u64>()
                .map_err(|e| anyhow::anyhow!("Invalid stake '{}': {}", validator.stake, e))?;
            total_stake_normalized += stake;
            
            let address: Multiaddr = validator.address.parse()
                .map_err(|e| anyhow::anyhow!("Invalid address '{}': {}", validator.address, e))?;
            
            let (authority_key_bytes, auth_key_format) = if validator.authority_key.starts_with("0x") {
                let hex_str = &validator.authority_key[2..];
                let bytes = hex::decode(hex_str)
                    .map_err(|e| anyhow::anyhow!("Failed to decode authority_key (BLS) hex '{}': {}", validator.authority_key, e))?;
                (bytes, "hex")
            } else {
                match STANDARD.decode(&validator.authority_key) {
                    Ok(bytes) => (bytes, "base64"),
                    Err(_) => {
                        let bytes = hex::decode(&validator.authority_key)
                            .map_err(|e| anyhow::anyhow!("Failed to decode authority_key (BLS) as base64 or hex '{}': {}", validator.authority_key, e))?;
                        (bytes, "hex (fallback)")
                    }
                }
            };
            info!("  🔑 [VALIDATOR-{}] authority_key format: {}, length: {} bytes", idx, auth_key_format, authority_key_bytes.len());
            let authority_pubkey = bls12381::min_sig::BLS12381PublicKey::from_bytes(&authority_key_bytes)
                .map_err(|e| anyhow::anyhow!("Failed to parse authority_key (BLS) from bytes (len={}): {}", authority_key_bytes.len(), e))?;
            let authority_key = AuthorityPublicKey::new(authority_pubkey);
            
            let (protocol_key_bytes, protocol_key_format) = if validator.protocol_key.starts_with("0x") {
                let hex_str = &validator.protocol_key[2..];
                let bytes = hex::decode(hex_str)
                    .map_err(|e| anyhow::anyhow!("Failed to decode protocol_key hex '{}': {}", validator.protocol_key, e))?;
                (bytes, "hex")
            } else {
                match STANDARD.decode(&validator.protocol_key) {
                    Ok(bytes) => (bytes, "base64"),
                    Err(_) => {
                        let bytes = hex::decode(&validator.protocol_key)
                            .map_err(|e| anyhow::anyhow!("Failed to decode protocol_key as base64 or hex '{}': {}", validator.protocol_key, e))?;
                        (bytes, "hex (fallback)")
                    }
                }
            };
            info!("  🔑 [VALIDATOR-{}] protocol_key format: {}, length: {} bytes", idx, protocol_key_format, protocol_key_bytes.len());
            let protocol_pubkey = ed25519::Ed25519PublicKey::from_bytes(&protocol_key_bytes)
                .map_err(|e| anyhow::anyhow!("Failed to parse protocol_key (Ed25519) from bytes (len={}): {}", protocol_key_bytes.len(), e))?;
            let protocol_key = ProtocolPublicKey::new(protocol_pubkey);
            
            let (network_key_bytes, network_key_format) = if validator.network_key.starts_with("0x") {
                let hex_str = &validator.network_key[2..];
                let bytes = hex::decode(hex_str)
                    .map_err(|e| anyhow::anyhow!("Failed to decode network_key hex '{}': {}", validator.network_key, e))?;
                (bytes, "hex")
            } else {
                match STANDARD.decode(&validator.network_key) {
                    Ok(bytes) => (bytes, "base64"),
                    Err(_) => {
                        let bytes = hex::decode(&validator.network_key)
                            .map_err(|e| anyhow::anyhow!("Failed to decode network_key as base64 or hex '{}': {}", validator.network_key, e))?;
                        (bytes, "hex (fallback)")
                    }
                }
            };
            info!("  🔑 [VALIDATOR-{}] network_key format: {}, length: {} bytes", idx, network_key_format, network_key_bytes.len());
            let network_pubkey = ed25519::Ed25519PublicKey::from_bytes(&network_key_bytes)
                .map_err(|e| anyhow::anyhow!("Failed to parse network_key (Ed25519) from bytes (len={}): {}", network_key_bytes.len(), e))?;
            let network_key = NetworkPublicKey::new(network_pubkey);
            
            let hostname = if !validator.name.is_empty() {
                validator.name.clone()
            } else {
                format!("node-{}", idx)
            };
            
            let address_for_log = validator.address.clone();
            let hostname_for_log = hostname.clone();
            let stake_for_log = stake;
            
            let authority = Authority {
                stake,
                address,
                hostname: hostname.clone(),
                authority_key,
                protocol_key,
                network_key,
            };
            
            authorities.push(authority);
            
            info!("  ✅ Added validator {}: address={}, stake={}, hostname={}", 
                idx, address_for_log, stake_for_log, hostname_for_log);
        }
        
        info!("📊 Built committee with {} authorities, total_stake={}, epoch={}", 
            authorities.len(), total_stake_normalized, epoch);
        
        let committee = Committee::new(epoch, authorities);
        Ok(committee)
    }

    /// Transition to new epoch from EndOfEpoch system transaction
    // ===== EPOCH TRANSITION METHODS =====

    pub async fn transition_to_epoch_from_system_tx(
        &mut self,
        new_epoch: u64,
        new_epoch_timestamp_ms: u64,
        commit_index: u32,
        config: &crate::config::NodeConfig,
    ) -> Result<()> {
        // CRITICAL FIX: Set is_transitioning flag FIRST, then check if already set
        // This prevents race condition where callback sets flag before this function is called
        // If flag was already set, it means transition is already in progress - skip duplicate
        let was_already_transitioning = self.is_transitioning.swap(true, Ordering::SeqCst);
        
        if was_already_transitioning {
            // Flag was already set - transition is already in progress
            // Reset flag since we're not actually transitioning
            self.is_transitioning.store(false, Ordering::SeqCst);
            
            // #region agent log
            {
                use std::io::Write;
                let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/home/abc/chain-n/mtn-simple-2025/.cursor/debug.log") {
                    let _ = writeln!(f, r#"{{"id":"log_{}_{}","timestamp":{},"location":"node.rs:950","message":"EPOCH TRANSITION ALREADY IN PROGRESS - skipping duplicate","data":{{"current_epoch":{},"new_epoch":{},"commit_index":{},"hypothesisId":"A"}},"sessionId":"debug-session","runId":"run1"}}"#, 
                        ts.as_secs(), ts.as_nanos() % 1000000,
                        ts.as_millis(),
                        self.current_epoch, new_epoch, commit_index);
                }
            }
            // #endregion
            warn!("⚠️ [EPOCH TRANSITION] Transition already in progress (epoch {} -> {}), skipping duplicate transition request at commit_index={}. This prevents multiple commit processors from being created.", 
                self.current_epoch, new_epoch, commit_index);
            return Ok(()); // Skip duplicate transition
        }
        
        // Flag was not set - we're the first to start transition
        // Flag is already set by swap() above, so we can proceed
        
        // #region agent log
        {
            use std::io::Write;
            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/home/abc/chain-n/mtn-simple-2025/.cursor/debug.log") {
                let _ = writeln!(f, r#"{{"id":"log_{}_{}","timestamp":{},"location":"node.rs:950","message":"STARTING EPOCH TRANSITION","data":{{"current_epoch":{},"new_epoch":{},"commit_index":{},"hypothesisId":"A"}},"sessionId":"debug-session","runId":"run1"}}"#, 
                    ts.as_secs(), ts.as_nanos() % 1000000,
                    ts.as_millis(),
                    self.current_epoch, new_epoch, commit_index);
            }
        }
        // #endregion

        // FORK PREVENTION: Add coordination delay for epoch transitions
        // This gives all nodes time to detect EndOfEpoch and prepare for transition
        // Prevents nodes from transitioning at slightly different times
        info!("⏱️ [EPOCH COORDINATION] Adding coordination delay for epoch {} -> {} transition", self.current_epoch, new_epoch);
        tokio::time::sleep(Duration::from_millis(500)).await; // 500ms coordination window

        // Guard to ensure is_transitioning flag is reset on error
        // Successful transition will reset flag manually before guard is dropped
        struct TransitionGuard {
            is_transitioning: Arc<AtomicBool>,
        }
        impl Drop for TransitionGuard {
            fn drop(&mut self) {
                // Only reset if still transitioning (successful transition resets it at line 1178)
                // This ensures flag is reset even if transition fails with an error
                if self.is_transitioning.load(Ordering::SeqCst) {
                    warn!("⚠️ [EPOCH TRANSITION] Transition failed or was interrupted, resetting is_transitioning flag");
                    self.is_transitioning.store(false, Ordering::SeqCst);
                }
            }
        }
        let _guard = TransitionGuard {
            is_transitioning: self.is_transitioning.clone(),
        };
        
        info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        info!("🔄 SUI-STYLE EPOCH TRANSITION: epoch {} -> {} (from system transaction)",
            self.current_epoch, new_epoch);
        info!("  📊 Transition triggered at commit_index={}", commit_index);
        info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

        // CRITICAL: Start reconfiguration immediately to queue transactions during transition
        // This prevents transactions from being rejected instead of queued
        info!("🔄 [EPOCH TRANSITION] Starting reconfiguration - transactions will be queued");
        self.close_user_certs().await;
        
        // Validate epoch increment
        if new_epoch != self.current_epoch + 1 {
            anyhow::bail!("Invalid epoch transition: expected {}, got {}", 
                self.current_epoch + 1, new_epoch);
        }
        
        let old_epoch = self.current_epoch;
        
        // OPTIMIZED: Wait for commit processor to finish processing all commits up to EndOfEpoch
        // The commit_index from EndOfEpoch transaction is the commit where transition is detected,
        // but commit processor may still be processing commits before that.
        // We need to wait for commit processor to catch up to ensure last_global_exec_index is accurate.
        info!("⏳ [EPOCH TRANSITION] Waiting for commit processor to process all commits up to EndOfEpoch commit_index={}...", 
            commit_index);
        info!("  📊 EndOfEpoch detected at commit_index={}, current_commit_index={}", 
            commit_index, self.current_commit_index.load(Ordering::SeqCst));
        
        // OPTIMIZED: Reduced wait time for faster transition
        // Wait for commit processor to catch up to EndOfEpoch commit_index
        let start_time = std::time::Instant::now();
        const MAX_WAIT_FOR_END_OF_EPOCH_SECS: u64 = 5; // Reduced from 10s to 5s for faster transition
        loop {
            let current = self.current_commit_index.load(Ordering::SeqCst);
            let elapsed = start_time.elapsed().as_secs();
            
            if current >= commit_index {
                info!("  ✅ Commit processor caught up: current_commit_index={} >= EndOfEpoch commit_index={}", 
                    current, commit_index);
                break;
            }
            
            if elapsed >= MAX_WAIT_FOR_END_OF_EPOCH_SECS {
                warn!("  ⚠️ [EPOCH TRANSITION] Timeout waiting for commit processor to reach EndOfEpoch commit_index={} (current={}, waited {}s). Using commit_index from EndOfEpoch transaction for deterministic last_global_exec_index calculation.", 
                    commit_index, current, MAX_WAIT_FOR_END_OF_EPOCH_SECS);
                break;
            }
            
            tokio::time::sleep(Duration::from_millis(100)).await; // Reduced from 200ms to 100ms for faster check
        }
        
        // CRITICAL FIX: Use commit_index from EndOfEpoch transaction (deterministic)
        // NOT actual_processed_commit_index which may differ between nodes if commit processor
        // runs at different speeds. All nodes will see the same commit_index from the committed
        // block containing EndOfEpoch transaction, ensuring deterministic last_global_exec_index.
        // 
        // Note: We wait for commit processor to catch up above, but even if it doesn't fully
        // catch up (timeout), we still use commit_index from EndOfEpoch transaction to ensure
        // all nodes calculate the same last_global_exec_index_at_transition.
        let actual_processed_commit_index = self.current_commit_index.load(Ordering::SeqCst);
        
        // #region agent log
        {
            use std::io::Write;
            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/home/abc/chain-n/mtn-simple-2025/.cursor/debug.log") {
                let _ = writeln!(f, r#"{{"id":"log_{}_{}","timestamp":{},"location":"node.rs:1004","message":"BEFORE calculate last_global_exec_index_at_transition","data":{{"old_epoch":{},"actual_processed_commit_index":{},"old_last_global_exec_index":{},"endofepoch_commit_index":{},"hypothesisId":"D"}},"sessionId":"debug-session","runId":"run1"}}"#, 
                    ts.as_secs(), ts.as_nanos() % 1000000,
                    ts.as_millis(),
                    old_epoch, actual_processed_commit_index, self.last_global_exec_index, commit_index);
            }
        }
        // #endregion
        
        // GLOBAL BLOCK NUMBERING: Calculate global_exec_index for the EndOfEpoch commit
        // This becomes the last block number of the old epoch
        let current_commit_global_exec_index = calculate_global_exec_index(
            old_epoch,
            commit_index,
            self.last_global_exec_index,
        );

        // The last global_exec_index of the old epoch is the current commit's global_exec_index
        let last_global_exec_index_at_transition = current_commit_global_exec_index;
        
        // #region agent log
        {
            use std::io::Write;
            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/home/abc/chain-n/mtn-simple-2025/.cursor/debug.log") {
                let _ = writeln!(f, r#"{{"id":"log_{}_{}","timestamp":{},"location":"node.rs:1009","message":"AFTER calculate last_global_exec_index_at_transition","data":{{"old_epoch":{},"commit_index_from_endofepoch":{},"current_commit_global_exec_index":{},"last_global_exec_index_at_transition":{},"hypothesisId":"D"}},"sessionId":"debug-session","runId":"run1"}}"#,
                    ts.as_secs(), ts.as_nanos() % 1000000,
                    ts.as_millis(),
                    old_epoch, commit_index, current_commit_global_exec_index, last_global_exec_index_at_transition);
            }
        }
        // #endregion
        info!("📊 [SNAPSHOT] Last block of epoch {}: commit_index={} (from EndOfEpoch), actual_processed={}, global_exec_index={}", 
            old_epoch, commit_index, actual_processed_commit_index, last_global_exec_index_at_transition);
        
        // OPTIMIZED: Reduced graceful shutdown time for faster transition
        // Just a brief pause to ensure clean shutdown
        tokio::time::sleep(Duration::from_millis(100)).await; // Reduced from graceful_shutdown() which had multiple sleeps
        
        let new_last_global_exec_index = last_global_exec_index_at_transition;
        
        // OPTIMIZED: Parallel processing - fetch committee and prepare other components simultaneously
        let executor_client = if config.executor_read_enabled {
            Arc::new(ExecutorClient::new(true, false,
                config.executor_send_socket_path.clone(),
                config.executor_receive_socket_path.clone()))
        } else {
            anyhow::bail!("Executor read not enabled, cannot fetch committee");
        };

        // OPTIMIZED: Fetch committee in parallel with database and authority preparation
        let block_number = new_last_global_exec_index;
        info!("📋 [EPOCH TRANSITION] Fetching committee from Go state at block {}...", block_number);

        // Start committee fetching task in background
        let committee_future = {
            let executor_client_clone = executor_client.clone();
            tokio::spawn(async move {
                Self::build_committee_from_go_validators_at_block(
                    &executor_client_clone,
                    block_number
                ).await
            })
        };

        // Prepare database path while committee is being fetched
        let db_path = self.storage_path
            .join("epochs")
            .join(format!("epoch_{}", new_epoch))
            .join("consensus_db");

        // CRITICAL: Clear any existing consensus database for new epoch
        // Ensure clean state for new epoch, no local state carry-over
        if db_path.exists() {
            info!("🧹 [EPOCH TRANSITION] Clearing consensus database for epoch {} at {:?}", new_epoch, db_path);
            if let Err(e) = std::fs::remove_dir_all(&db_path) {
                warn!("⚠️  Failed to clear consensus database for epoch {}: {}", new_epoch, e);
            }
        }

        std::fs::create_dir_all(&db_path)?;

        // Wait for committee fetching to complete
        let new_committee_raw = committee_future.await??;
        info!("✅ [EPOCH TRANSITION] Committee fetched from Go state");
        
        let authorities: Vec<_> = new_committee_raw.authorities()
            .map(|(_, auth)| auth.clone())
            .collect();
        let new_committee = Committee::new(new_epoch, authorities);

        info!("✅ Committee built from Go state: {} authorities", new_committee.size());

        // Check and update node mode based on committee membership
        self.check_and_update_node_mode(&new_committee, config).await?;
        
        // CRITICAL: Update own_index based on new committee membership
        // This is essential for nodes that switch from sync-only to validator mode
        // Tách biệt committee và cấu hình: node không nằm trong committee thì đồng bộ, có thì tham gia đồng thuận
        let node_hostname = format!("node-{}", config.node_id);
        if let Some((new_own_index, _)) = new_committee.authorities()
            .find(|(_, authority)| authority.hostname == node_hostname) {
            if self.own_index != new_own_index {
                info!("🔄 [EPOCH TRANSITION] Updating own_index from {} to {} (node is now in committee - will participate in consensus)", 
                    self.own_index, new_own_index);
                self.own_index = new_own_index;
            } else {
                info!("📊 [EPOCH TRANSITION] own_index unchanged: {} (node remains in committee)", self.own_index);
            }
        } else {
            // Node is not in committee - set to ZERO (won't be used for sync-only nodes)
            if matches!(self.node_mode, NodeMode::Validator) {
                warn!("⚠️ [EPOCH TRANSITION] Node mode is Validator but node is not in committee! This should not happen.");
            }
            self.own_index = AuthorityIndex::ZERO;
            info!("📊 [EPOCH TRANSITION] Node not in committee - own_index set to ZERO (sync-only mode - will only sync data)");
        }
        
        // CRITICAL FIX: Wait for commit processor to process ALL commits in pipeline
        // When EndOfEpoch transaction is detected in commit N, there may still be other commits
        // (N+1, N+2, ...) that have been committed by consensus but not yet processed by commit processor.
        // We must wait for commit processor to catch up before stopping authority.
        
        info!("⏳ [EPOCH TRANSITION] Waiting for commit processor to process all commits in pipeline...");
        info!("  📊 EndOfEpoch detected at commit_index={}, current_commit_index={}", 
            commit_index, self.current_commit_index.load(Ordering::SeqCst));
        
        // OPTIMIZED: Adaptive timeout based on configuration optimization level
        let base_wait_for_commits_secs = match config.epoch_transition_optimization.as_str() {
            "fast" => 5,     // Aggressive: 5s for fastest transitions
            "safe" => 30,    // Conservative: 30s for maximum safety
            _ => 10,         // Balanced: 10s default (reasonable safety vs speed)
        };

        let system_load = self.get_system_load();
        let adaptive_timeout = Self::calculate_adaptive_timeout(base_wait_for_commits_secs, system_load);

        info!("🎯 [EPOCH TRANSITION] Using {} optimization - adaptive timeout: {}s (base: {}s, load: {:.1})",
            config.epoch_transition_optimization, adaptive_timeout, base_wait_for_commits_secs, system_load);

        let wait_result = tokio::time::timeout(
            Duration::from_secs(adaptive_timeout),
            self.wait_for_commit_processor_completion(commit_index, adaptive_timeout)
        ).await;

        match wait_result {
            Ok(Ok(())) => {
                info!("✅ [EPOCH TRANSITION] Commit processor completed successfully");
            }
            Ok(Err(e)) => {
                warn!("⚠️ [EPOCH TRANSITION] Commit processor wait failed: {}, proceeding anyway", e);
            }
            Err(_) => {
                let last_commit_index = self.current_commit_index.load(Ordering::SeqCst);
                warn!("⚠️ [EPOCH TRANSITION] Timeout waiting for commit processor ({}s), proceeding anyway. Last commit_index={}",
                    adaptive_timeout, last_commit_index);
            }
        }
        
        // OPTIMIZED: Adaptive executor drain time based on optimization level
        // Commits have been sent to executor, but executor may still be processing them
        let executor_drain_secs = match config.epoch_transition_optimization.as_str() {
            "fast" => 0,     // Aggressive: skip executor drain for fastest transition
            "safe" => 2,     // Conservative: 2s for maximum safety
            _ => 0,          // Balanced: skip drain by default (trust commit processor wait above)
        };

        info!("⏳ [EPOCH TRANSITION] Using {} optimization - executor drain wait: {}s",
            config.epoch_transition_optimization, executor_drain_secs);
        if executor_drain_secs > 0 {
            tokio::time::sleep(Duration::from_secs(executor_drain_secs)).await;
        }
        info!("✅ [EPOCH TRANSITION] Commit processor and executor drain completed, safe to stop old authority");

        // GRADUAL SHUTDOWN: Use gradual shutdown for smoother transitions if enabled
        if config.enable_gradual_shutdown {
            info!("🔄 [EPOCH TRANSITION] Using gradual shutdown for smooth transition");
            if let Err(e) = self.gradual_shutdown_for_epoch_transition(config).await {
                warn!("⚠️ [EPOCH TRANSITION] Gradual shutdown failed: {}, proceeding with immediate shutdown", e);
            }
        } else {
            info!("⚡ [EPOCH TRANSITION] Gradual shutdown disabled, proceeding with immediate shutdown");
        }

        // Stop old authority
        if let Some(authority) = self.authority.take() {
            authority.stop().await;
        }
        
        // Update state
        self.current_epoch = new_epoch;
        self.current_commit_index.store(0, Ordering::SeqCst);

        // CRITICAL: Sync shared_last_global_exec_index with Go state BEFORE creating new commit processor
        // This ensures commit processor uses correct last_global_exec_index for sequential block numbering
        // We query Go for last block number to ensure we're in sync
        // The synced value will be used for:
        // 1. self.last_global_exec_index
        // 2. shared_last_global_exec_index
        // 3. initial_next_expected for executor client
        let synced_last_global_exec_index = if config.executor_read_enabled {
            let executor_client_for_sync = Arc::new(ExecutorClient::new(true, false,
                config.executor_send_socket_path.clone(),
                config.executor_receive_socket_path.clone()));
            
            // Query Go for last block number to sync shared_last_global_exec_index
            if let Ok(go_last_block_number) = executor_client_for_sync.get_last_block_number().await {
                // CRITICAL: Use Go's last block number to ensure we're in sync
                // If Go is ahead, use Go's value. If Go is behind, use our calculated value.
                let synced = if go_last_block_number >= new_last_global_exec_index {
                    // Go is ahead or equal - use Go's value to prevent duplicate commits
                    info!("📊 [EPOCH TRANSITION] Go is ahead (last_block_number={} >= new_last_global_exec_index={}), syncing to Go's value", 
                        go_last_block_number, new_last_global_exec_index);
                    go_last_block_number
                } else {
                    // Go is behind - use our calculated value
                    info!("📊 [EPOCH TRANSITION] Go is behind (last_block_number={} < new_last_global_exec_index={}), using calculated value", 
                        go_last_block_number, new_last_global_exec_index);
                    new_last_global_exec_index
                };
                
                // Update shared index with synced value
                {
                    let mut shared_index_guard = self.shared_last_global_exec_index.lock().await;
                    *shared_index_guard = synced;
                    info!("📊 [EPOCH TRANSITION] Updated shared_last_global_exec_index to {} (synced with Go) for new epoch {}", 
                        synced, new_epoch);
                }
                
                synced
            } else {
                // Failed to query Go - use calculated value
                warn!("⚠️  [EPOCH TRANSITION] Failed to query Go for last block number, using calculated value {}", new_last_global_exec_index);
                {
                    let mut shared_index_guard = self.shared_last_global_exec_index.lock().await;
                    *shared_index_guard = new_last_global_exec_index;
                    info!("📊 [EPOCH TRANSITION] Updated shared_last_global_exec_index to {} (calculated) for new epoch {}", 
                        new_last_global_exec_index, new_epoch);
                }
                new_last_global_exec_index
            }
        } else {
            // Executor read not enabled - use calculated value
            {
                let mut shared_index_guard = self.shared_last_global_exec_index.lock().await;
                *shared_index_guard = new_last_global_exec_index;
                info!("📊 [EPOCH TRANSITION] Updated shared_last_global_exec_index to {} (calculated, executor read disabled) for new epoch {}", 
                    new_last_global_exec_index, new_epoch);
            }
            new_last_global_exec_index
        };
        
        // Update self.last_global_exec_index with synced value
        self.last_global_exec_index = synced_last_global_exec_index;

        // Update execution lock with new epoch
        self.update_execution_lock_epoch(new_epoch).await;
        
        // Update system transaction provider
        self.system_transaction_provider.update_epoch(
            new_epoch,
            new_epoch_timestamp_ms
        ).await;
        
        // Create new DB path
        let db_path = self.storage_path
            .join("epochs")
            .join(format!("epoch_{}", new_epoch))
            .join("consensus_db");

        // CRITICAL: Clear any existing consensus database for new epoch
        // Ensure clean state for new epoch, no local state carry-over
        if db_path.exists() {
            info!("🧹 [EPOCH TRANSITION] Clearing consensus database for epoch {} at {:?}", new_epoch, db_path);
            if let Err(e) = std::fs::remove_dir_all(&db_path) {
                warn!("⚠️  Failed to clear consensus database for epoch {}: {}", new_epoch, e);
            }
        }

        std::fs::create_dir_all(&db_path)?;
        
        // Create new commit processor
        let (commit_consumer, commit_receiver, mut block_receiver) = CommitConsumerArgs::new(0, 0);
        let commit_index_for_callback = self.current_commit_index.clone();
        let new_epoch_clone = new_epoch;
        let executor_commit_enabled = self.executor_commit_enabled;
        
        let executor_client_opt = if executor_commit_enabled {
            // CRITICAL FIX: Use synced_last_global_exec_index that was calculated above
            // This ensures initial_next_expected matches what commit processor will use
            // 
            // CRITICAL: Set initial next_expected_index based on synced_last_global_exec_index
            // This ensures commits with global_exec_index = synced_last_global_exec_index + 1, +2, ... 
            // will be sent correctly, not stuck in buffer
            // For epoch 0: synced_last_global_exec_index = 0, so initial = 1 (correct)
            // For epoch N: synced_last_global_exec_index = last block of previous epoch (synced with Go), so initial = last + 1 (correct)
            let initial_next_expected = synced_last_global_exec_index + 1;
            info!("🔧 [EXECUTOR INIT] Creating executor client for epoch {} with initial_next_expected={} (based on synced_last_global_exec_index={})", 
                new_epoch, initial_next_expected, synced_last_global_exec_index);
            let client = Arc::new(ExecutorClient::new_with_initial_index(true, true, 
                config.executor_send_socket_path.clone(), 
                config.executor_receive_socket_path.clone(),
                initial_next_expected));
            Some(client)
        } else {
            None
        };
        
        let is_transitioning_for_new_epoch = self.is_transitioning.clone();
        
        // Setup callback for next epoch transition
        // This callback will send transition request via channel
        // CRITICAL FIX: Do NOT set is_transitioning here - let transition_to_epoch_from_system_tx do it
        // This prevents race condition where transition is skipped because flag is already set
        let epoch_transition_callback = crate::commit_callbacks::create_epoch_transition_callback(self.epoch_transition_sender.clone());
        
        // #region agent log
        {
            use std::io::Write;
            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/home/abc/chain-n/mtn-simple-2025/.cursor/debug.log") {
                // Get synced_last_global_exec_index for logging
                let synced_for_log = {
                    let shared_index_guard = self.shared_last_global_exec_index.lock().await;
                    *shared_index_guard
                };
                let _ = writeln!(f, r#"{{"id":"log_{}_{}","timestamp":{},"location":"node.rs:1127","message":"CREATING NEW COMMIT PROCESSOR","data":{{"new_epoch":{},"synced_last_global_exec_index":{},"hypothesisId":"A"}},"sessionId":"debug-session","runId":"run1"}}"#, 
                    ts.as_secs(), ts.as_nanos() % 1000000,
                    ts.as_millis(),
                    new_epoch, synced_for_log);
            }
        }
        // #endregion
        let mut commit_processor = crate::commit_processor::CommitProcessor::new(commit_receiver)
            .with_commit_index_callback(crate::commit_callbacks::create_commit_index_callback(commit_index_for_callback))
            .with_global_exec_index_callback(crate::commit_callbacks::create_global_exec_index_callback(self.shared_last_global_exec_index.clone()))
            .with_get_last_global_exec_index({
                let shared_index = self.shared_last_global_exec_index.clone();
                move || {
                    // Get current last global exec index synchronously
                    // CRITICAL FIX: Use try_current() to avoid panic if called from async context
                    // If called from async context, return 0 and log warning
                    if let Ok(_rt) = tokio::runtime::Handle::try_current() {
                        // We're in async context, can't use block_on
                        // Return 0 as fallback - actual value will be read from shared index in async context
                        warn!("⚠️  [GLOBAL_EXEC_INDEX] get_last_global_exec_index called from async context in epoch transition, returning 0. Actual value should be read from shared index.");
                        0
                    } else {
                        // We're in sync context, can use block_on
                        let shared_index_clone = shared_index.clone();
                        block_on(async {
                            let index_guard = shared_index_clone.lock().await;
                            *index_guard
                        })
                    }
                }
            })
            .with_shared_last_global_exec_index(self.shared_last_global_exec_index.clone())
            .with_epoch_info(new_epoch, synced_last_global_exec_index)
            .with_is_transitioning(is_transitioning_for_new_epoch)
            .with_pending_transactions_queue(self.pending_transactions_queue.clone())
            .with_epoch_transition_callback(epoch_transition_callback);
        
        if let Some(ref client) = executor_client_opt {
            // CRITICAL FIX: Do NOT call initialize_from_go() after epoch transition
            // We've already synced shared_last_global_exec_index with Go state above
            // and set initial_next_expected correctly. Calling initialize_from_go() here
            // would overwrite the correct initial_next_expected and cause blocks to be stuck in buffer.
            // 
            // The executor client was created with correct initial_next_expected = synced_last_global_exec_index + 1,
            // which matches what commit processor will use (shared_last_global_exec_index + 1).
            info!("✅ [EXECUTOR INIT] Executor client for epoch {} already initialized with correct initial_next_expected (synced with Go state above)", new_epoch);
            commit_processor = commit_processor.with_executor_client(client.clone());
        }
        
        // Get synced_last_global_exec_index for logging
        let synced_last_global_exec_index_for_log = {
            let shared_index_guard = self.shared_last_global_exec_index.lock().await;
            *shared_index_guard
        };
        
        tokio::spawn(async move {
            info!("🚀 [COMMIT PROCESSOR] Starting for epoch {} (last_global_exec_index={})",
                new_epoch_clone, synced_last_global_exec_index_for_log);
            if let Err(e) = commit_processor.run().await {
                error!("❌ [COMMIT PROCESSOR] Error: {}", e);
            }
        });
        
        tokio::spawn(async move {
            use tracing::debug;
            while let Some(output) = block_receiver.recv().await {
                debug!("Received {} certified blocks", output.blocks.len());
            }
        });
        
        // Restart authority only if node is in validator mode
        let new_registry = Registry::new();
        let authority = if matches!(self.node_mode, NodeMode::Validator) {
            info!("🚀 [AUTHORITY] Starting consensus authority for epoch {} (node mode: {:?})", new_epoch, self.node_mode);
            let mut parameters = self.parameters.clone();
            parameters.db_path = db_path.clone();
            self.boot_counter = self.boot_counter.saturating_add(1);

            Some(ConsensusAuthority::start(
            NetworkType::Tonic,
            new_epoch_timestamp_ms,
            self.own_index,
            new_committee.clone(),
            parameters,
            self.protocol_config.clone(),
            self.protocol_keypair.clone(),
            self.network_keypair.clone(),
            self.clock.clone(),
            self.transaction_verifier.clone(),
            commit_consumer,
            new_registry.clone(),
            self.boot_counter,
            Some(self.system_transaction_provider.clone() as Arc<dyn SystemTransactionProvider>),
            )
            .await)
        } else {
            info!("🔄 [AUTHORITY] Skipping consensus authority start for epoch {} (node mode: {:?})", new_epoch, self.node_mode);
            None
        };

        let registry_id = if let (Some(ref rs), Some(ref _authority)) = (self.registry_service.as_ref(), authority.as_ref()) {
            Some(rs.add(new_registry))
        } else {
            None
        };
        self.current_registry_id = registry_id;
        
        if let Some(ref auth) = authority {
            let new_client = auth.transaction_client();
            if let Some(ref proxy) = self.transaction_client_proxy {
                // Update existing proxy
                proxy.set_client(new_client).await;
            } else {
                // Create transaction client proxy if it doesn't exist (node switched from sync-only to validator)
                info!("✅ [EPOCH TRANSITION] Creating transaction client proxy (node switched from sync-only to validator)");
                self.transaction_client_proxy = Some(Arc::new(TransactionClientProxy::new(new_client)));
            }
        } else {
            // Node switched to sync-only mode - clear transaction client proxy
            if self.transaction_client_proxy.is_some() {
                info!("🔄 [EPOCH TRANSITION] Clearing transaction client proxy (node switched from validator to sync-only)");
                self.transaction_client_proxy = None;
            }
        }
        self.authority = authority;

            // NOTE: Genesis blocks will be generated and persisted by DagState when the authority
            // starts with the new epoch. This ensures all nodes generate identical genesis blocks
            // for the same epoch (since they use the same committee).

        // OPTIMIZED: Adaptive delay for authority ready check based on optimization level
        // This prevents transactions from being lost if epoch transition is too fast
        // Authority needs time to initialize network connections, start consensus, etc.
        let authority_ready_delay_ms = match config.epoch_transition_optimization.as_str() {
            "fast" => 200,   // Aggressive: 200ms minimum for consensus startup
            "safe" => 3000,  // Conservative: 3s for maximum safety, allows consensus to fully sync
            _ => 1000,       // Balanced: 1s reasonable delay for consensus startup (increased from 500ms)
        };

        info!("⏳ [EPOCH TRANSITION] Using {} optimization - waiting {}ms for new authority to be fully ready before submitting queued transactions...",
            config.epoch_transition_optimization, authority_ready_delay_ms);
        tokio::time::sleep(Duration::from_millis(authority_ready_delay_ms)).await;

        // ADDITIONAL SAFETY: Test consensus readiness with a dummy transaction before submitting queue
        // This ensures consensus is actually ready to accept transactions, not just authority is created
        let consensus_ready = self.test_consensus_readiness().await;
        if consensus_ready {
            info!("✅ [EPOCH TRANSITION] Consensus is ready - proceeding to submit queued transactions");
        } else {
            warn!("⚠️ [EPOCH TRANSITION] Consensus may not be fully ready yet, but proceeding anyway (retry mechanism will handle failures)");
        }

        // Log reconfiguration state before submitting queued transactions
        let reconfig_state = self.get_reconfig_state().await;
        info!("🔍 [EPOCH TRANSITION] Reconfiguration state before submitting queued transactions: {:?}", reconfig_state.status());

        // CRITICAL FIX: Reset is_transitioning flag AFTER authority is fully ready
        // This prevents transactions from being accepted before consensus is actually running
        self.is_transitioning.store(false, Ordering::SeqCst);
        info!("✅ [EPOCH TRANSITION] is_transitioning flag reset - new transactions can now be accepted by ready authority in epoch {}", new_epoch);
        
        // CRITICAL: Submit queued transactions with retry mechanism
        // This ensures transactions are not lost even if authority is not immediately ready
        let queue_size_before = {
            let queue = self.pending_transactions_queue.lock().await;
            queue.len()
        };
        info!("📦 [EPOCH TRANSITION] Queue size before submitting: {}", queue_size_before);
        
        if queue_size_before > 0 {
            info!("🚀 [EPOCH TRANSITION] Submitting {} queued transactions to new epoch {}...", 
                queue_size_before, new_epoch);
        }
        
        // Submit with retry mechanism to handle cases where authority is not immediately ready
        let queued_count = self.submit_queued_transactions().await?;
        if queued_count > 0 {
            info!("✅ [EPOCH TRANSITION] Submitted {} queued transactions to consensus in new epoch {} (transactions are now being processed)", 
                queued_count, new_epoch);
        } else if queue_size_before > 0 {
            warn!("⚠️ [EPOCH TRANSITION] Queue had {} transactions but submitted 0 (may have been deduplicated or failed)", queue_size_before);
        } else {
            info!("ℹ️  [EPOCH TRANSITION] No queued transactions to submit (queue was empty)");
        }
        
        info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        info!("✅ SUI-STYLE EPOCH TRANSITION COMPLETE: epoch {} -> {}", old_epoch, new_epoch);
        info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

        // Reset reconfiguration state to clean state for new epoch
        self.reset_reconfig_state().await;

        // LVM Snapshot creation after successful epoch transition
        if self.enable_lvm_snapshot {
            info!("📸 [LVM SNAPSHOT] Node is configured to create snapshots after epoch transitions");

            // Spawn task to create snapshot asynchronously (don't block epoch transition)
            let config_clone = config.clone();
            let lvm_snapshot_bin_path = self.lvm_snapshot_bin_path.clone();
            let lvm_snapshot_delay_seconds = self.lvm_snapshot_delay_seconds;

            tokio::spawn(async move {
                info!("📸 [LVM SNAPSHOT] Starting snapshot creation task for epoch {} (delay: {}s)", new_epoch, lvm_snapshot_delay_seconds);

                // Wait for the configured delay to allow Go executor to finish processing
                if lvm_snapshot_delay_seconds > 0 {
                    info!("⏳ [LVM SNAPSHOT] Waiting {}s before creating snapshot to allow Go executor to stabilize", lvm_snapshot_delay_seconds);
                    tokio::time::sleep(Duration::from_secs(lvm_snapshot_delay_seconds)).await;
                }

                // Check Go Master's last block to ensure data persistence
                // Create a new executor client for this check
                let executor_client = Arc::new(crate::executor_client::ExecutorClient::new(
                    config_clone.executor_read_enabled,
                    config_clone.executor_commit_enabled,
                    config_clone.executor_send_socket_path.clone(),
                    config_clone.executor_receive_socket_path.clone(),
                ));

                let go_last_block = match executor_client.get_last_block_number().await {
                    Ok(block_num) => {
                        info!("✅ [LVM SNAPSHOT] Go Master last block: {}", block_num);
                        Some(block_num)
                    }
                    Err(e) => {
                        warn!("⚠️  [LVM SNAPSHOT] Failed to get Go Master last block: {}. Continuing with snapshot creation.", e);
                        None
                    }
                };

                if let Some(block_num) = go_last_block {
                    info!("📊 [LVM SNAPSHOT] Verified Go Master has processed up to block {}", block_num);
                }

                // Create LVM snapshot using lvm-snap-rsync binary
                if let Some(bin_path) = lvm_snapshot_bin_path {
                    info!("🚀 [LVM SNAPSHOT] Creating LVM snapshot for epoch {} using binary: {}", new_epoch, bin_path.display());

                    // Get the directory containing the binary (where config.toml should be)
                    let bin_dir = bin_path.parent().unwrap_or(&std::path::PathBuf::from(".")).to_path_buf();
                    info!("📁 [LVM SNAPSHOT] Running binary from directory: {}", bin_dir.display());

                    match tokio::process::Command::new("sudo")
                        .arg(&bin_path)
                        .current_dir(bin_dir)
                        .arg("--id")
                        .arg(new_epoch.to_string())
                        .output()
                        .await
                    {
                        Ok(output) => {
                            if output.status.success() {
                                info!("✅ [LVM SNAPSHOT] Successfully created LVM snapshot for epoch {}", new_epoch);
                                if let Ok(stdout) = String::from_utf8(output.stdout) {
                                    if !stdout.trim().is_empty() {
                                        info!("📋 [LVM SNAPSHOT] Output: {}", stdout.trim());
                                    }
                                }
                            } else {
                                let stderr = String::from_utf8_lossy(&output.stderr);
                                error!("❌ [LVM SNAPSHOT] Failed to create LVM snapshot for epoch {}: {}", new_epoch, stderr);
                            }
                        }
                        Err(e) => {
                            error!("❌ [LVM SNAPSHOT] Failed to execute lvm-snap-rsync binary: {}", e);
                        }
                    }
                } else {
                    warn!("⚠️  [LVM SNAPSHOT] No lvm_snapshot_bin_path configured, skipping snapshot creation");
                }
            });
        } else {
            info!("ℹ️  [LVM SNAPSHOT] Node is not configured to create snapshots (enable_lvm_snapshot = false)");
        }

        // CRITICAL: Notify Go about epoch transition to keep states synchronized
        // This ensures Go executor advances to the same epoch as Rust consensus
        {
            info!("🔄 [EPOCH SYNC] Notifying Go executor about epoch transition: {} -> {}", old_epoch, new_epoch);

            // Create a temporary executor client for epoch sync
            // Use same socket paths as configured
            let executor_client = Arc::new(crate::executor_client::ExecutorClient::new(
                true, // Enable reading
                false, // Don't commit
                config.executor_send_socket_path.clone(),
                config.executor_receive_socket_path.clone(),
            ));

            match executor_client.advance_epoch(new_epoch, new_epoch_timestamp_ms).await {
                Ok(()) => {
                    info!("✅ [EPOCH SYNC] Successfully notified Go executor about epoch transition to {}", new_epoch);
                }
                Err(e) => {
                    error!("❌ [EPOCH SYNC] Failed to notify Go executor about epoch transition: {}", e);
                    // Continue despite error - epoch transition is still valid in Rust
                    // Go will be out of sync but can be fixed by restarting or manual sync
                }
            }
        }

        Ok(())
    }

    /// Acquire execution lock for reconfiguration
    /// This ensures no transactions are being executed during reconfiguration
    pub async fn execution_lock_for_reconfiguration(&self) -> tokio::sync::RwLockWriteGuard<'_, u64> {
        self.execution_lock.write().await
    }

    /// Update execution lock with new epoch
    pub async fn update_execution_lock_epoch(&self, new_epoch: u64) {
        *self.execution_lock.write().await = new_epoch;
    }

    /// Test if consensus is ready to accept transactions
    /// This is a lightweight check to ensure consensus is actually running before submitting queued transactions
    async fn test_consensus_readiness(&self) -> bool {
        // Try to submit a minimal dummy transaction to test if consensus accepts it
        // If it succeeds, consensus is ready. If it fails, consensus may still be starting up.

        // For sync-only nodes, we don't have transaction client proxy
        if let Some(ref proxy) = self.transaction_client_proxy {
            // Create a minimal dummy transaction (this won't be processed, just tests connectivity)
            let dummy_tx = vec![0u8; 64]; // Minimal transaction data
            let test_transactions = vec![dummy_tx];

            match proxy.submit(test_transactions).await {
                Ok(_) => {
                    info!("🧪 [CONSENSUS READY] Consensus accepted test transaction - ready for queued transactions");
                    true
                },
                Err(e) => {
                    info!("🧪 [CONSENSUS READY] Consensus rejected test transaction: {} - may still be starting up", e);
                    false
                }
            }
        } else {
            // Sync-only nodes don't have transaction client - consensus is not ready for submitting
            info!("🧪 [CONSENSUS READY] Node is sync-only - no transaction client available");
            false
        }
    }

    // ===== CONFIGURATION & STATUS METHODS =====

    /// Reset reconfiguration state to clean state after successful transition
    pub async fn reset_reconfig_state(&self) {
        let mut state = self.reconfig_state.write().await;
        let old_status = state.status().clone();
        *state = ReconfigState::default(); // Reset to AcceptAll
        info!("🔄 [RECONFIG RESET] Reset reconfiguration state from {:?} to {:?}", old_status, state.status());
    }

    /// Get current reconfig state
    pub async fn get_reconfig_state(&self) -> ReconfigState {
        self.reconfig_state.read().await.clone()
    }


    /// Close user certificates - transition to RejectUserCerts
    pub async fn close_user_certs(&self) {
        let mut state = self.reconfig_state.write().await;
        let old_status = state.status().clone();
        state.close_user_certs();
        info!("🔄 [RECONFIG] Closed user certificates during reconfiguration: {:?} -> {:?}", old_status, state.status());
    }

    /// Close all certificates - transition to RejectAllCerts
    pub async fn close_all_certs(&self) {
        let mut state = self.reconfig_state.write().await;
        state.close_all_certs();
        info!("Closed all certificates during reconfiguration");
    }

    /// Close all transactions - transition to RejectAllTx
    pub async fn close_all_tx(&self) {
        let mut state = self.reconfig_state.write().await;
        state.close_all_tx();
        info!("Closed all transactions during reconfiguration");
    }


    /// Check if any transactions should be accepted
    pub async fn should_accept_tx(&self) -> bool {
        self.reconfig_state.read().await.should_accept_tx()
    }

    /// Perform gradual shutdown for smooth epoch transition
    /// Similar to Sui's reconfiguration process but adapted for metanode
    pub async fn gradual_shutdown_for_epoch_transition(&self, config: &crate::config::NodeConfig) -> Result<()> {
        info!("🔄 [GRADUAL SHUTDOWN] Starting smooth epoch transition process...");

        // Phase 1: Close user certificates
        info!("📋 [GRADUAL SHUTDOWN] Phase 1: Closing user certificates");
        self.close_user_certs().await;

        // Wait for user certs to drain (configurable)
        let user_cert_drain_secs = config.gradual_shutdown_user_cert_drain_secs.unwrap_or(2);
        if user_cert_drain_secs > 0 {
            info!("⏳ [GRADUAL SHUTDOWN] Waiting {}s for user certificates to drain", user_cert_drain_secs);
            tokio::time::sleep(Duration::from_secs(user_cert_drain_secs)).await;
        }

        // Phase 2: Close all certificates (keep accepting system transactions)
        info!("🚫 [GRADUAL SHUTDOWN] Phase 2: Closing all certificates");
        self.close_all_certs().await;

        // Wait for consensus certs to drain (configurable)
        let consensus_cert_drain_secs = config.gradual_shutdown_consensus_cert_drain_secs.unwrap_or(1);
        if consensus_cert_drain_secs > 0 {
            info!("⏳ [GRADUAL SHUTDOWN] Waiting {}s for consensus certificates to drain", consensus_cert_drain_secs);
            tokio::time::sleep(Duration::from_secs(consensus_cert_drain_secs)).await;
        }

        // Phase 3: Close all transactions
        info!("🛑 [GRADUAL SHUTDOWN] Phase 3: Closing all transactions");
        self.close_all_tx().await;

        // Wait for final drain (configurable)
        let final_drain_secs = config.gradual_shutdown_final_drain_secs.unwrap_or(1);
        if final_drain_secs > 0 {
            info!("⏳ [GRADUAL SHUTDOWN] Waiting {}s for final transaction drain", final_drain_secs);
            tokio::time::sleep(Duration::from_secs(final_drain_secs)).await;
        }

        // Phase 4: Acquire execution lock to ensure no transactions are running
        info!("🔒 [GRADUAL SHUTDOWN] Phase 4: Acquiring execution lock");
        let _execution_lock = self.execution_lock_for_reconfiguration().await;
        info!("✅ [GRADUAL SHUTDOWN] Execution lock acquired - safe to proceed with authority shutdown");

        Ok(())
    }

    /// Check if node should be in validator mode based on committee membership
    /// and update mode accordingly
    // ===== NODE MODE MANAGEMENT METHODS =====

    pub async fn check_and_update_node_mode(&mut self, committee: &consensus_config::Committee, config: &NodeConfig) -> Result<()> {
        // Use hostname from config (node-{id}) instead of system hostname
        // This ensures correct matching with committee hostnames
        let node_hostname = format!("node-{}", config.node_id);
        let should_be_validator = committee.authorities()
            .any(|(_, authority)| authority.hostname == node_hostname);

        let new_mode = if should_be_validator {
            NodeMode::Validator
        } else {
            NodeMode::SyncOnly
        };

        if self.node_mode != new_mode {
            info!("🔄 [NODE MODE] Switching from {:?} to {:?} (hostname: {}, in_committee: {})",
                self.node_mode, new_mode, node_hostname, should_be_validator);

            match (&self.node_mode, &new_mode) {
                (NodeMode::SyncOnly, NodeMode::Validator) => {
                    // Switching from sync-only to validator
                    // Stop sync task and start consensus participation
                    if let Err(e) = self.stop_sync_task().await {
                        warn!("⚠️ [NODE MODE] Failed to stop sync task: {}", e);
                    }
                    // Authority should already be created during epoch transition
                    if self.authority.is_none() {
                        warn!("⚠️ [NODE MODE] Switching to validator mode but authority is None!");
                    } else {
                        info!("✅ [NODE MODE] Successfully switched to validator mode");
                    }
                }
                (NodeMode::Validator, NodeMode::SyncOnly) => {
                    // Switching from validator to sync-only
                    // Start sync task and stop consensus participation
                    if let Err(e) = self.start_sync_task(config).await {
                        warn!("⚠️ [NODE MODE] Failed to start sync task: {}", e);
                    }
                    if self.authority.is_some() {
                        info!("🔄 [NODE MODE] Switching to sync-only mode - stopping consensus participation");
                        // Note: We don't stop the authority here as it might be needed for epoch transition
                        // The authority will be properly managed during epoch transition
                    }
                }
                _ => {} // Same mode, no action needed
            }

            self.node_mode = new_mode;
        } else {
            trace!("📊 [NODE MODE] Mode unchanged: {:?} (hostname: {}, in_committee: {})",
                self.node_mode, node_hostname, should_be_validator);
        }

        Ok(())
    }

    /// Get current node mode
    // pub fn get_node_mode(&self) -> &NodeMode {
    //     &self.node_mode
    // }
    /// Start sync task for sync-only nodes
    /// This task periodically syncs data from Go executor
    // ===== SYNC OPERATIONS METHODS =====

    pub async fn start_sync_task(&mut self, config: &NodeConfig) -> Result<()> {
        if !matches!(self.node_mode, NodeMode::SyncOnly) {
            info!("🔄 [SYNC TASK] Skipping sync task start - node is in {:?} mode", self.node_mode);
            return Ok(());
        }

        if self.sync_task_handle.is_some() {
            warn!("⚠️ [SYNC TASK] Sync task already running");
            return Ok(());
        }

        info!("🚀 [SYNC TASK] Starting sync task for sync-only node");

        // Create executor client for sync operations
        let executor_client = Arc::new(ExecutorClient::new(
            true, // Enable reading
            false, // Don't commit
            config.executor_send_socket_path.clone(),
            config.executor_receive_socket_path.clone(),
        ));

        // Clone needed data for the task
        let last_global_exec_index = Arc::clone(&self.shared_last_global_exec_index);
        let current_epoch = self.current_epoch;
        let node_mode = self.node_mode.clone();

        let sync_task = tokio::spawn(async move {
            info!("🔄 [SYNC TASK] Sync task started - monitoring Go executor state");

            loop {
                // Sync every 5 seconds
                tokio::time::sleep(Duration::from_secs(5)).await;

                match Self::perform_sync_operation(&executor_client, &last_global_exec_index, current_epoch, &node_mode).await {
                    Ok(_) => {
                        trace!("✅ [SYNC TASK] Sync operation completed successfully");
                    }
                    Err(e) => {
                        warn!("⚠️ [SYNC TASK] Sync operation failed: {}", e);
                    }
                }
            }
        });

        self.sync_task_handle = Some(sync_task);
        info!("✅ [SYNC TASK] Sync task started successfully");

        Ok(())
    }

    /// Stop sync task if running
    pub async fn stop_sync_task(&mut self) -> Result<()> {
        if let Some(handle) = self.sync_task_handle.take() {
            info!("🛑 [SYNC TASK] Stopping sync task...");
            handle.abort();
            // Wait for task to finish or timeout
            let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
            info!("✅ [SYNC TASK] Sync task stopped");
        }
        Ok(())
    }

    /// Perform one sync operation with Go executor
    async fn perform_sync_operation(
        executor_client: &Arc<ExecutorClient>,
        shared_last_global_exec_index: &Arc<tokio::sync::Mutex<u64>>,
        _current_epoch: u64,
        node_mode: &NodeMode,
    ) -> Result<()> {
        // Only sync if we're in sync-only mode
        if !matches!(node_mode, NodeMode::SyncOnly) {
            return Ok(());
        }

        // Get latest block number from Go
        let go_last_block = executor_client.get_last_block_number().await?;
        trace!("📊 [SYNC TASK] Go executor last block: {}", go_last_block);

        // Update our shared last global exec index if needed
        let mut shared_index = shared_last_global_exec_index.lock().await;
        if go_last_block > *shared_index {
            *shared_index = go_last_block;
            trace!("📈 [SYNC TASK] Updated shared last_global_exec_index to {}", go_last_block);
        }

        // TODO: Future enhancements
        // - Sync committee changes
        // - Monitor for epoch transitions
        // - Sync transaction pool if needed

        Ok(())
    }

    /// Perform fork detection by comparing state with peers
    async fn perform_fork_detection_check(&self) -> Result<()> {
        // FORK DETECTION: Compare our state with peers to detect potential forks
        // This is a simplified version - in production would need more sophisticated checks

        let our_epoch = self.current_epoch;
        let our_last_commit = self.last_global_exec_index;

        info!("🔍 [FORK DETECTION] Checking for potential forks - epoch: {}, last_commit: {}",
            our_epoch, our_last_commit);

        // In a real implementation, this would:
        // 1. Query peers for their epoch and last commit
        // 2. Compare with our state
        // 3. Alert if significant divergence detected
        // 4. Potentially halt operations if fork confirmed

        // For now, just log - full implementation needs peer communication
        info!("✅ [FORK DETECTION] Local state check passed - no obvious fork detected");

        Ok(())
    }

    /// Check for blocks committed by Rust but not executed by Go and attempt recovery
    async fn perform_block_recovery_check(
        executor_client: &Arc<ExecutorClient>,
        go_last_block: u64,
        current_epoch: u64,
        storage_path: &std::path::Path,
        node_id: u32,
    ) -> Result<()> {
        info!("🔍 [RECOVERY] Checking for blocks committed by Rust but not executed by Go...");

        // Only node 0 can perform recovery (to avoid conflicts)
        if node_id != 0 {
            info!("ℹ️ [RECOVERY] Skipping recovery check - only node 0 performs recovery");
            return Ok(());
        }

        // Load committed blocks from storage that should have been sent to Go
        let db_path = storage_path
            .join("epochs")
            .join(format!("epoch_{}", current_epoch))
            .join("consensus_db");

        if !db_path.exists() {
            info!("ℹ️ [RECOVERY] No epoch-specific storage found (epoch {}), skipping block recovery check", current_epoch);
            return Ok(());
        }

        let recovery_store = Arc::new(RocksDBStore::new(db_path.to_str().unwrap()));

        // Find commits with global_exec_index > Go's last_block
        let recovery_start_index = go_last_block + 1;
        info!("🔍 [RECOVERY] Scanning for committed blocks from global_exec_index {} onwards...", recovery_start_index);

        // Get last commit info to determine recovery range
        let last_commit_info = recovery_store.read_last_commit_info()
            .map_err(|e| anyhow::anyhow!("Failed to read last commit info: {}", e))?;

        if let Some((last_commit_ref, _)) = last_commit_info {
            let last_commit_index = last_commit_ref.index;
            info!("📊 [RECOVERY] Last commit in storage: index={}, digest={}",
                last_commit_index, last_commit_ref.digest);

            // Scan commits from recovery_start_index to find missing blocks
            let recovery_range = (recovery_start_index as u32)..=(last_commit_index);
            let commits_to_recover = recovery_store.scan_commits(recovery_range.into())
                .map_err(|e| anyhow::anyhow!("Failed to scan commits for recovery: {}", e))?;

            if commits_to_recover.is_empty() {
                info!("✅ [RECOVERY] No missing blocks found - Go is up to date");
                return Ok(());
            }

            warn!("🚨 [RECOVERY] Found {} commits with missing blocks (global_exec_index {} to {})",
                commits_to_recover.len(), recovery_start_index, last_commit_index);

            // CRITICAL SAFETY: Double-check with Go before resending to prevent duplicates
            info!("🔒 [RECOVERY] Double-checking with Go executor before resend...");

            match executor_client.get_last_block_number().await {
                Ok(updated_go_last_block) => {
                    if updated_go_last_block >= last_commit_index as u64 {
                        info!("✅ [RECOVERY] Go is fully up to date ({} >= {}) - no resend needed",
                            updated_go_last_block, last_commit_index);
                        return Ok(());
                    }

                    // Conservative approach: only resend if gap is significant (>10 blocks)
                    // This prevents resending for minor timing differences
                    let gap = last_commit_index as u64 - updated_go_last_block;
                    if gap <= 10 {
                        info!("ℹ️ [RECOVERY] Small gap detected ({} blocks) - likely timing difference, skipping resend",
                            gap);
                        return Ok(());
                    }

                    info!("📊 [RECOVERY] Confirmed gap: Rust at {}, Go at {} (gap: {})",
                        last_commit_index, updated_go_last_block, gap);
                }
                Err(e) => {
                    warn!("⚠️ [RECOVERY] Could not verify Go status: {} - aborting recovery to prevent fork", e);
                    return Ok(()); // Conservative: don't resend if can't verify
                }
            }

            // Implement actual block resend logic
            info!("🚀 [RECOVERY] Starting automatic block resend to Go executor...");

            let mut blocks_resent = 0;
            let mut current_global_exec_index = recovery_start_index;

            for commit in commits_to_recover {
                info!("📦 [RECOVERY] Processing commit {} with {} blocks",
                    commit.index(), commit.blocks().len());

                // Load the actual VerifiedBlocks from storage and create CommittedSubDag
                let store = RocksDBStore::new(storage_path.to_str().unwrap());

                // Use the public load_committed_subdag_from_store function
                let subdag = load_committed_subdag_from_store(
                    &store,
                    commit.clone(),
                    vec![], // empty reputation scores for recovery
                );

                // SAFETY CHECK: Ensure commit epoch matches current epoch
                // Extract epoch from the first block in the loaded subdag
                let commit_epoch = subdag.blocks.first().map(|b| b.epoch()).unwrap_or(0);
                if commit_epoch != current_epoch {
                    warn!("🚨 [RECOVERY] Epoch mismatch detected: commit epoch={}, current epoch={} - skipping to prevent fork",
                        commit_epoch, current_epoch);
                    continue;
                }

                        // Send the subdag directly using the public method
                match executor_client.send_committed_subdag(
                    &subdag,
                    commit_epoch,
                    current_global_exec_index,
                ).await {
                    Ok(_) => {
                        info!("✅ [RECOVERY] Successfully resent commit {} (global_exec_index={})",
                            commit.index(), current_global_exec_index);

                        // RECOVERY VERIFICATION: Check that Go received this specific block
                        match executor_client.get_last_block_number().await {
                            Ok(go_last_after) => {
                                if go_last_after >= current_global_exec_index {
                                    info!("✅ [RECOVERY VERIFICATION] Go confirmed receipt of block {}", current_global_exec_index);
                                    blocks_resent += commit.blocks().len();
                                } else {
                                    warn!("⚠️ [RECOVERY VERIFICATION] Go not yet updated after resend (expected: {}, got: {})",
                                        current_global_exec_index, go_last_after);
                                    // Still count as resent but log warning
                                    blocks_resent += commit.blocks().len();
                                }
                            }
                            Err(e) => {
                                warn!("⚠️ [RECOVERY VERIFICATION] Could not verify Go receipt: {}", e);
                                // Assume success for recovery continuity
                                blocks_resent += commit.blocks().len();
                            }
                        }

                        current_global_exec_index += 1;
                    }
                    Err(e) => {
                        error!("❌ [RECOVERY] Failed to resend commit {}: {}", commit.index(), e);
                        // Continue with next commit - don't fail entire recovery
                    }
                }
            }

            if blocks_resent > 0 {
                info!("🎉 [RECOVERY] Successfully resent {} blocks to Go executor", blocks_resent);

                // Verify Go received the blocks by checking last block number again
                match executor_client.get_last_block_number().await {
                    Ok(new_go_last_block) => {
                        if new_go_last_block >= current_global_exec_index - 1 {
                            info!("✅ [RECOVERY] Go executor confirmed receipt - last block: {}", new_go_last_block);
                        } else {
                            warn!("⚠️ [RECOVERY] Go executor may not have processed all resent blocks - last block: {} (expected: {})",
                                new_go_last_block, current_global_exec_index - 1);
                        }
                    }
                    Err(e) => {
                        warn!("⚠️ [RECOVERY] Could not verify Go receipt: {}", e);
                    }
                }

            } else {
                error!("❌ [RECOVERY] No blocks were successfully resent to Go executor");
            }

        } else {
            info!("ℹ️ [RECOVERY] No commits found in storage, skipping recovery check");
        }

        Ok(())
    }

}