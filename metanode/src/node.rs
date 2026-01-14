// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use consensus_config::{AuthorityIndex, Committee, Authority};
use consensus_core::{
    ConsensusAuthority, NetworkType, Clock,
    CommitConsumerArgs,
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

use crate::config::NodeConfig;
use crate::tx_submitter::{TransactionClientProxy, TransactionSubmitter};
use crate::checkpoint::calculate_global_exec_index;
use crate::executor_client::ExecutorClient;
use consensus_core::{SystemTransactionProvider, DefaultSystemTransactionProvider};

// Global registry for transition handler to access node
// This allows transition handler task to call transition function on the node
static TRANSITION_HANDLER_REGISTRY: tokio::sync::OnceCell<Arc<tokio::sync::Mutex<Option<Arc<tokio::sync::Mutex<ConsensusNode>>>>>> = tokio::sync::OnceCell::const_new();

async fn get_transition_handler_node() -> Option<Arc<tokio::sync::Mutex<ConsensusNode>>> {
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
    /// Stable handle for RPC submissions across in-process authority restart
    transaction_client_proxy: Arc<TransactionClientProxy>,
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
    /// Channel sender for epoch transition requests from system transactions
    epoch_transition_sender: tokio::sync::mpsc::UnboundedSender<(u64, u64, u32)>,
}

impl ConsensusNode {
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

        // FIX: Always fetch committee from Go state via Unix Domain Socket. Do NOT load from file.
        info!("🚀 [STARTUP] Loading committee from Go state via Unix Domain Socket (block 0/genesis)...");

        // Create executor client for fetching committee from Go
        // Always enable executor client for committee fetching during startup
        let executor_client = Arc::new(ExecutorClient::new(
            true, // Always enable for committee fetching
            false, // Don't commit during committee fetching
            config.executor_send_socket_path.clone(),
            config.executor_receive_socket_path.clone(),
        ));

        // Fetch validators from Go at block 0 (genesis)
        let (validators, _go_epoch_timestamp_ms) = executor_client.get_validators_at_block(0).await
            .map_err(|e| anyhow::anyhow!("Failed to fetch committee from Go state: {}", e))?;

        // Load epoch_timestamp_ms from genesis.json instead of Go state for consistency
        let genesis_path = std::path::Path::new("../../mtn-simple-2025/cmd/simple_chain/genesis.json");
        let epoch_timestamp_ms = if genesis_path.exists() {
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
        info!("🔧 DEBUG: Creating committee with {} authorities", sorted_authorities.len());
        for (i, auth) in sorted_authorities.iter().enumerate() {
            info!("🔧 DEBUG: Authority[{}]: stake={}, address={}", i, auth.stake, auth.address);
        }
        let committee = Committee::new(0, sorted_authorities); // epoch 0 for genesis
        let current_epoch = committee.epoch();
        info!("✅ Loaded committee from Go state with {} authorities, epoch={}", committee.size(), current_epoch);

        // Capture paths needed for epoch transition
        // NOTE: Không còn require committee_path vì chúng ta không lưu committee ra file
        // Committee sẽ được fetch từ Go state mỗi lần khởi động
        let storage_path = config.storage_path.clone();
        
        // CRITICAL FIX: Load last_global_exec_index from Go state instead of hardcoding 0
        // This prevents duplicate global_exec_index when node restarts
        // If Go has processed blocks, we need to start from the correct last_global_exec_index
        let last_global_exec_index = if config.executor_read_enabled {
            match executor_client.get_last_block_number().await {
                Ok(last_block_number) => {
                    info!("📊 [STARTUP] Loaded last_global_exec_index={} from Go state (last_block_number)", last_block_number);
                    last_block_number
                },
                Err(e) => {
                    warn!("⚠️  [STARTUP] Failed to get last_block_number from Go state: {}. Using 0 (genesis).", e);
                    0
                }
            }
        } else {
            info!("📊 [STARTUP] Executor read not enabled, using last_global_exec_index=0 (genesis)");
            0
        };
        info!("✅ [STARTUP] Using last_global_exec_index={} for commit processor", last_global_exec_index);

        // Load keypairs (kept for in-process restart)
        let protocol_keypair = config.load_protocol_keypair()?;
        let network_keypair = config.load_network_keypair()?;

        // Get own authority index by matching hostname (committee is now sorted by address)
        let own_hostname = format!("node-{}", config.node_id);
        let own_index = committee.authorities().find_map(|(idx, auth)| {
            if auth.hostname == own_hostname {
                Some(idx)
            } else {
                None
            }
        }).ok_or_else(|| {
            anyhow::anyhow!("Cannot find authority with hostname '{}' in committee", own_hostname)
        })?;
        info!("Node {} matched to authority index {}", config.node_id, own_index);

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
        let pending_transactions_queue = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        
        // Create channel for epoch transition requests from system transactions
        let (epoch_tx_sender, epoch_tx_receiver) = tokio::sync::mpsc::unbounded_channel::<(u64, u64, u32)>();
        
        // Store receiver for later use in transition handler
        let epoch_tx_receiver_for_handler = epoch_tx_receiver;
        
        // Setup callback for EndOfEpoch system transaction (Sui-style)
        // Callback sends transition request via channel, which will be handled by a task
        // NOTE: Do NOT set is_transitioning flag here - it will be set in transition_to_epoch_from_system_tx
        // Setting it here causes race condition where handler sees flag=true and skips transition
        let epoch_transition_callback = {
            let tx_clone = epoch_tx_sender.clone();
            move |new_epoch, new_epoch_timestamp_ms, commit_index| {
                info!("🎯 [SYSTEM TX CALLBACK] EndOfEpoch detected: epoch={}, timestamp={}, commit_index={}",
                    new_epoch, new_epoch_timestamp_ms, commit_index);
                
                // Send transition request via channel
                // is_transitioning flag will be set in transition_to_epoch_from_system_tx when transition actually starts
                if let Err(e) = tx_clone.send((new_epoch, new_epoch_timestamp_ms, commit_index)) {
                    error!("Failed to send epoch transition request: {}", e);
                    return Err(anyhow::anyhow!("Failed to send epoch transition request: {}", e));
                }
                Ok(())
            }
        };
        
        // Create ordered commit processor
        let mut commit_processor = crate::commit_processor::CommitProcessor::new(commit_receiver)
            .with_commit_index_callback(move |index| {
                commit_index_for_callback.store(index, Ordering::SeqCst);
            })
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
                .map(|ms| Duration::from_millis(ms))
                .unwrap_or_else(|| Duration::from_millis((200.0 / speed_multiplier) as u64));
            
            let min_round_delay = config.min_round_delay_ms
                .map(|ms| Duration::from_millis(ms))
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

        // Start authority node
        info!("Starting consensus authority node...");
        let authority = ConsensusAuthority::start(
            NetworkType::Tonic,
            epoch_start_timestamp,
            own_index,
            committee,
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
        .await;

        let transaction_client = authority.transaction_client();
        let transaction_client_proxy = Arc::new(TransactionClientProxy::new(transaction_client));

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
        let node = Self {
            authority: Some(authority),
            transaction_client_proxy,
            // REMOVED: epoch_change_manager - using SystemTransactionProvider instead
            clock_sync_manager,
            current_commit_index,
            storage_path,
            current_epoch,
            last_global_exec_index,
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
            epoch_transition_sender: epoch_tx_sender_for_node,
        };

        // Spawn transition handler task
        // This task will process transition requests and call transition function
        // The node will be registered in global registry after it's wrapped in Arc<Mutex<>> in main.rs
        
        tokio::spawn(async move {
            let mut receiver = epoch_tx_receiver_for_handler;
            while let Some((new_epoch, new_epoch_timestamp_ms, commit_index)) = receiver.recv().await {
                info!("🚀 [EPOCH TRANSITION HANDLER] Processing transition request: epoch={}, timestamp={}, commit_index={}",
                    new_epoch, new_epoch_timestamp_ms, commit_index);
                
                // Update system transaction provider
                system_transaction_provider_for_handler.update_epoch(
                    new_epoch,
                    new_epoch_timestamp_ms
                ).await;
                
                // Try to get node from global registry and call transition function
                if let Some(node_arc) = get_transition_handler_node().await {
                    let mut node_guard = node_arc.lock().await;
                    if let Err(e) = node_guard.transition_to_epoch_from_system_tx(
                        new_epoch,
                        new_epoch_timestamp_ms,
                        commit_index,
                        &config_for_handler,
                    ).await {
                        error!("❌ [EPOCH TRANSITION HANDLER] Failed to transition epoch: {}", e);
                    } else {
                        info!("✅ [EPOCH TRANSITION HANDLER] Successfully transitioned to epoch {}", new_epoch);
                    }
                } else {
                    warn!("⚠️ [EPOCH TRANSITION HANDLER] Node not registered in global registry yet - transition will be handled when node is available");
                    // Transition will be handled when node is registered
                }
            }
        });

        Ok(node)
    }

    #[allow(dead_code)] 
    pub fn transaction_submitter(&self) -> Arc<dyn TransactionSubmitter> {
        self.transaction_client_proxy.clone() as Arc<dyn TransactionSubmitter>
    }

    #[allow(dead_code)]
    pub fn current_commit_index(&self) -> u32 {
        self.current_commit_index.load(Ordering::SeqCst)
    }

    #[allow(dead_code)]
    pub fn update_commit_index(&self, index: u32) {
        self.current_commit_index.store(index, Ordering::SeqCst);
    }



    pub async fn is_ready_for_transactions(&self) -> (bool, String) {
        if self.authority.is_none() {
            return (false, "Node is still initializing".to_string());
        }

        if self.last_transition_hash.is_some() {
            return (false, format!(
                "Epoch transition in progress: epoch {} -> {} (waiting for new authority to start)",
                self.current_epoch, self.current_epoch + 1
            ));
        }
        
        (true, "Node is ready".to_string())
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
        
        (true, false, "Node is ready".to_string())
    }
    
    pub async fn queue_transaction_for_next_epoch(&self, tx_data: Vec<u8>) -> Result<()> {
        let mut queue = self.pending_transactions_queue.lock().await;
        queue.push(tx_data);
        info!("📦 [TX FLOW] Queued transaction for next epoch: queue_size={}", queue.len());
        Ok(())
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
        // Retry failed submissions with exponential backoff to handle fast epoch transitions
        // Submit transactions with retry mechanism
        // Retry failed submissions with exponential backoff to handle fast epoch transitions
        const MAX_RETRIES: u32 = 5;
        const INITIAL_RETRY_DELAY_MS: u64 = 100;
        let mut successful_count = 0;
        let mut requeued_count = 0;
        
        for tx_data in transactions {
            let transactions_vec = vec![tx_data.clone()];
            let mut retry_count = 0;
            
            while retry_count < MAX_RETRIES {
                match self.transaction_client_proxy.submit(transactions_vec.clone()).await {
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
                            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                        } else {
                            error!("❌ [TX FLOW] Failed to submit queued transaction after {} retries: {}", MAX_RETRIES, e);
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
        }
        
        Ok(successful_count)
    }

    pub async fn shutdown(self) -> Result<()> {
        info!("Shutting down consensus node...");
        if let Some(authority) = self.authority {
            authority.stop().await;
        }
        info!("Consensus node stopped");
        Ok(())
    }

    #[allow(dead_code)]
    pub async fn graceful_shutdown(&mut self) -> Result<()> {
        info!("Starting graceful shutdown...");
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
        
        // FORK-SAFETY: Use commit_index from EndOfEpoch transaction (deterministic across all nodes)
        // This ensures all nodes calculate the same last_global_exec_index_at_transition
        let last_global_exec_index_at_transition = calculate_global_exec_index(
            old_epoch,
            commit_index,  // ✅ Use commit_index from EndOfEpoch transaction (deterministic)
            self.last_global_exec_index,
        );
        
        // #region agent log
        {
            use std::io::Write;
            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/home/abc/chain-n/mtn-simple-2025/.cursor/debug.log") {
                let _ = writeln!(f, r#"{{"id":"log_{}_{}","timestamp":{},"location":"node.rs:1009","message":"AFTER calculate last_global_exec_index_at_transition","data":{{"old_epoch":{},"commit_index_from_endofepoch":{},"last_global_exec_index_at_transition":{},"hypothesisId":"D"}},"sessionId":"debug-session","runId":"run1"}}"#, 
                    ts.as_secs(), ts.as_nanos() % 1000000,
                    ts.as_millis(),
                    old_epoch, commit_index, last_global_exec_index_at_transition);
            }
        }
        // #endregion
        info!("📊 [SNAPSHOT] Last block of epoch {}: commit_index={} (from EndOfEpoch), actual_processed={}, global_exec_index={}", 
            old_epoch, commit_index, actual_processed_commit_index, last_global_exec_index_at_transition);
        
        // OPTIMIZED: Reduced graceful shutdown time for faster transition
        // Just a brief pause to ensure clean shutdown
        tokio::time::sleep(Duration::from_millis(100)).await; // Reduced from graceful_shutdown() which had multiple sleeps
        
        let new_last_global_exec_index = last_global_exec_index_at_transition;
        
        // Fetch committee from Go state
        let executor_client = if config.executor_read_enabled {
            Arc::new(ExecutorClient::new(true, false, 
                config.executor_send_socket_path.clone(), 
                config.executor_receive_socket_path.clone()))
        } else {
            anyhow::bail!("Executor read not enabled, cannot fetch committee");
        };
        
        // OPTIMIZED: Fetch committee in parallel with other setup to speed up transition
        let block_number = new_last_global_exec_index;
        info!("📋 [EPOCH TRANSITION] Fetching committee from Go state at block {}...", block_number);
        let new_committee_raw = Self::build_committee_from_go_validators_at_block(
            &executor_client, 
            block_number
        ).await?;
        info!("✅ [EPOCH TRANSITION] Committee fetched from Go state");
        
        let authorities: Vec<_> = new_committee_raw.authorities()
            .map(|(_, auth)| auth.clone())
            .collect();
        let new_committee = Committee::new(new_epoch, authorities);
        
        info!("✅ Committee built from Go state: {} authorities", new_committee.size());
        
        // CRITICAL FIX: Wait for commit processor to process ALL commits in pipeline
        // When EndOfEpoch transaction is detected in commit N, there may still be other commits
        // (N+1, N+2, ...) that have been committed by consensus but not yet processed by commit processor.
        // We must wait for commit processor to catch up before stopping authority.
        
        info!("⏳ [EPOCH TRANSITION] Waiting for commit processor to process all commits in pipeline...");
        info!("  📊 EndOfEpoch detected at commit_index={}, current_commit_index={}", 
            commit_index, self.current_commit_index.load(Ordering::SeqCst));
        
        // Wait for commit processor to catch up with adequate timeouts to prevent transaction loss
        // We wait until current_commit_index stops increasing (no new commits being processed)
        let start_time = std::time::Instant::now();
        const MAX_WAIT_FOR_COMMITS_SECS: u64 = 30; // Increased to ensure commit processor has enough time
        const STABLE_TIME_SECS: u64 = 5; // Increased to ensure commits are fully processed
        let mut last_commit_index = self.current_commit_index.load(Ordering::SeqCst);
        let mut last_change_time = std::time::Instant::now();
        
        loop {
            let current = self.current_commit_index.load(Ordering::SeqCst);
            let elapsed = start_time.elapsed().as_secs();
            
            if current > last_commit_index {
                // Commit index is still increasing - commits are being processed
                info!("  📈 Commit processor is processing: current_commit_index={} (was {}), elapsed={}s", 
                    current, last_commit_index, elapsed);
                last_commit_index = current;
                last_change_time = std::time::Instant::now();
            } else if last_change_time.elapsed().as_secs() >= STABLE_TIME_SECS {
                // Commit index has been stable for STABLE_TIME_SECS - processor likely caught up
                info!("  ✅ Commit processor appears to have caught up: current_commit_index={} (stable for {}s)", 
                    current, STABLE_TIME_SECS);
                break;
            }
            
            if elapsed >= MAX_WAIT_FOR_COMMITS_SECS {
                warn!("  ⚠️ [EPOCH TRANSITION] Timeout waiting for commit processor ({}s), stopping anyway. Last commit_index={}", 
                    MAX_WAIT_FOR_COMMITS_SECS, last_commit_index);
                break;
            }
            
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        
        // OPTIMIZED: Reduced wait time for executor drain for smoother transition
        // Commits have been sent to executor, but executor may still be processing them
        const EXECUTOR_DRAIN_SECS: u64 = 1; // Reduced from 3s to 1s for faster transition
        info!("⏳ [EPOCH TRANSITION] Waiting {}s for executor to execute all sent commits...", EXECUTOR_DRAIN_SECS);
        tokio::time::sleep(Duration::from_secs(EXECUTOR_DRAIN_SECS)).await;
        info!("✅ [EPOCH TRANSITION] Commit processor and executor drain completed, safe to stop old authority");
        
        // Stop old authority
        if let Some(authority) = self.authority.take() {
            authority.stop().await;
        }
        
        // CRITICAL OPTIMIZATION: Reset is_transitioning flag EARLY to allow new transactions
        // This allows transactions to be queued for the new epoch while we're still setting up
        // The new authority will be ready soon, and transactions can be submitted immediately
        self.is_transitioning.store(false, Ordering::SeqCst);
        info!("✅ [EPOCH TRANSITION] is_transitioning flag reset - new transactions can now be queued for epoch {}", new_epoch);
        
        // Update state
        self.current_epoch = new_epoch;
        self.last_global_exec_index = new_last_global_exec_index;
        self.current_commit_index.store(0, Ordering::SeqCst);
        
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
        std::fs::create_dir_all(&db_path)?;
        
        // Create new commit processor
        let (commit_consumer, commit_receiver, mut block_receiver) = CommitConsumerArgs::new(0, 0);
        let commit_index_for_callback = self.current_commit_index.clone();
        let new_epoch_clone = new_epoch;
        let executor_commit_enabled = self.executor_commit_enabled;
        
        let executor_client_opt = if executor_commit_enabled {
            // CRITICAL FIX: Set initial next_expected_index based on new_last_global_exec_index
            // This ensures commits with global_exec_index = new_last_global_exec_index + 1, +2, ... 
            // will be sent correctly, not stuck in buffer
            // For epoch 0: new_last_global_exec_index = 0, so initial = 1 (correct)
            // For epoch N: new_last_global_exec_index = last block of previous epoch, so initial = last + 1 (correct)
            let initial_next_expected = new_last_global_exec_index + 1;
            info!("🔧 [EXECUTOR INIT] Creating executor client for epoch {} with initial_next_expected={} (based on new_last_global_exec_index={})", 
                new_epoch, initial_next_expected, new_last_global_exec_index);
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
        let epoch_tx_sender_for_next_epoch = self.epoch_transition_sender.clone();
        let epoch_transition_callback = {
            move |new_epoch_cb, new_epoch_timestamp_ms, commit_index| {
                info!("🎯 [SYSTEM TX CALLBACK] EndOfEpoch detected in new epoch: epoch={}, timestamp={}, commit_index={}",
                    new_epoch_cb, new_epoch_timestamp_ms, commit_index);
                
                // DON'T set is_transitioning here - let transition_to_epoch_from_system_tx do it
                // Setting it here causes race condition where transition_to_epoch_from_system_tx
                // sees flag already set and skips the transition
                
                // Send transition request via channel
                if let Err(e) = epoch_tx_sender_for_next_epoch.send((new_epoch_cb, new_epoch_timestamp_ms, commit_index)) {
                    error!("Failed to send epoch transition request: {}", e);
                    return Err(anyhow::anyhow!("Failed to send epoch transition request: {}", e));
                }
                Ok(())
            }
        };
        
        // #region agent log
        {
            use std::io::Write;
            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/home/abc/chain-n/mtn-simple-2025/.cursor/debug.log") {
                let _ = writeln!(f, r#"{{"id":"log_{}_{}","timestamp":{},"location":"node.rs:1127","message":"CREATING NEW COMMIT PROCESSOR","data":{{"new_epoch":{},"new_last_global_exec_index":{},"hypothesisId":"A"}},"sessionId":"debug-session","runId":"run1"}}"#, 
                    ts.as_secs(), ts.as_nanos() % 1000000,
                    ts.as_millis(),
                    new_epoch, new_last_global_exec_index);
            }
        }
        // #endregion
        let mut commit_processor = crate::commit_processor::CommitProcessor::new(commit_receiver)
            .with_commit_index_callback(move |index| {
                commit_index_for_callback.store(index, Ordering::SeqCst);
            })
            .with_epoch_info(new_epoch, new_last_global_exec_index)
            .with_is_transitioning(is_transitioning_for_new_epoch)
            .with_pending_transactions_queue(self.pending_transactions_queue.clone())
            .with_epoch_transition_callback(epoch_transition_callback);
        
        if let Some(ref client) = executor_client_opt {
            // CRITICAL FIX: Initialize executor client to sync with Go state
            // This ensures next_expected_index and buffer are in sync with Go's last block number
            let executor_client_for_init = client.clone();
            tokio::spawn(async move {
                info!("🔧 [EXECUTOR INIT] Initializing executor client to sync with Go state...");
                executor_client_for_init.initialize_from_go().await;
                info!("✅ [EXECUTOR INIT] Executor client initialized and synced with Go state");
            });
            commit_processor = commit_processor.with_executor_client(client.clone());
        }
        
        tokio::spawn(async move {
            info!("🚀 [COMMIT PROCESSOR] Starting for epoch {} (last_global_exec_index={})",
                new_epoch_clone, new_last_global_exec_index);
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
        
        // Restart authority
        let mut parameters = self.parameters.clone();
        parameters.db_path = db_path.clone();
        self.boot_counter = self.boot_counter.saturating_add(1);
        
        let new_registry = Registry::new();
        
        let authority = ConsensusAuthority::start(
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
        .await;
        
        let registry_id = if let Some(ref rs) = self.registry_service {
            Some(rs.add(new_registry))
        } else {
            None
        };
        self.current_registry_id = registry_id;
        
        let new_client = authority.transaction_client();
        self.transaction_client_proxy.set_client(new_client).await;
        self.authority = Some(authority);
        
        // CRITICAL: Wait a short time to ensure new authority is fully ready before submitting transactions
        // This prevents transactions from being lost if epoch transition is too fast
        // Authority needs time to initialize network connections, start consensus, etc.
        const AUTHORITY_READY_DELAY_MS: u64 = 500; // 500ms delay to ensure authority is ready
        info!("⏳ [EPOCH TRANSITION] Waiting {}ms for new authority to be fully ready before submitting queued transactions...", 
            AUTHORITY_READY_DELAY_MS);
        tokio::time::sleep(Duration::from_millis(AUTHORITY_READY_DELAY_MS)).await;
        info!("✅ [EPOCH TRANSITION] Authority should be ready now, submitting queued transactions...");
        
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
        
        Ok(())
    }
}