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
use crate::epoch_change::EpochChangeManager;
use crate::clock_sync::ClockSyncManager;
use crate::sui_epoch_transition::SuiEpochTransitionOrchestrator;
use prometheus::Registry;
use mysten_metrics::RegistryService;
use serde_json;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
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

pub struct ConsensusNode {
    authority: Option<ConsensusAuthority>,
    /// Stable handle for RPC submissions across in-process authority restart
    transaction_client_proxy: Arc<TransactionClientProxy>,
    /// Epoch change manager
    #[allow(dead_code)] // Used internally by monitoring tasks
    epoch_change_manager: Arc<RwLock<EpochChangeManager>>,
    /// Clock synchronization manager
    #[allow(dead_code)] // Used internally by sync tasks
    clock_sync_manager: Arc<RwLock<ClockSyncManager>>,
    /// Sui-style epoch transition orchestrator
    #[allow(dead_code)] // Used for Sui-style epoch transitions
    sui_epoch_orchestrator: Arc<SuiEpochTransitionOrchestrator>,
    /// Current commit index (for fork-safe epoch transition)
    #[allow(dead_code)] // Used internally by commit processor callback
    current_commit_index: Arc<AtomicU32>,

    /// Paths needed for real epoch transition (persist + clean state)
    storage_path: std::path::PathBuf,
    /// Current epoch (for deterministic global_exec_index calculation)
    current_epoch: u64,
    /// Last global execution index (for deterministic global_exec_index calculation)
    last_global_exec_index: u64,
    /// Block counter for current epoch (for block-based epoch transitions)
    blocks_in_current_epoch: Option<Arc<std::sync::atomic::AtomicU32>>,

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
    /// Transition barrier for current CommitProcessor (to prevent sending commits past barrier)
    /// This prevents duplicate global_exec_index between epochs
    transition_barrier: Arc<AtomicU32>,
    /// Global exec index at barrier (for commits past barrier)
    /// Commits past barrier will be sent as one block with global_exec_index = barrier_global_exec_index + 1
    /// Uses Arc<AtomicU64> for thread-safe access
    global_exec_index_at_barrier: Arc<std::sync::atomic::AtomicU64>,
    /// Queue for transactions received during barrier phase
    /// Transactions in this queue will be submitted to consensus in the next epoch
    pending_transactions_queue: Arc<tokio::sync::Mutex<Vec<Vec<u8>>>>,
    /// LVM snapshot configuration (from config)
    enable_lvm_snapshot: bool,
    lvm_snapshot_bin_path: Option<std::path::PathBuf>,
    lvm_snapshot_delay_seconds: u64,
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

        // FIX: Try to fetch committee from Go state, fallback to loading from file
        let (validators, epoch_timestamp_ms) = if config.executor_read_enabled {
            info!("🚀 [STARTUP] Loading committee from Go state via Unix Domain Socket (block 0/genesis)...");

            // Create executor client for fetching committee from Go
            let executor_client = Arc::new(ExecutorClient::new(
                true, // Always enable for committee fetching
                false, // Don't commit during committee fetching
                config.executor_send_socket_path.clone(),
                config.executor_receive_socket_path.clone(),
            ));

            // Fetch validators from Go at block 0 (genesis)
            match executor_client.get_validators_at_block(0).await {
                Ok((validators, go_epoch_timestamp_ms)) => {
                    info!("✅ Successfully loaded committee from Go state");
                    (validators, go_epoch_timestamp_ms)
                }
                Err(e) => {
                    warn!("⚠️  Failed to fetch committee from Go state: {}", e);
                    anyhow::bail!("Cannot load committee - Go state unavailable and no fallback implemented");
                }
            }
        } else {
            info!("⚠️  Executor read disabled - cannot load committee");
            anyhow::bail!("Cannot load committee - executor read disabled and no fallback implemented");
        };

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
                                    let default_ts = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap()
                                        .as_millis() as u64;
                                    warn!("⚠️  Could not find epoch_timestamp_ms in genesis.json, using current timestamp: {}", default_ts);
                                    default_ts
                                }
                            }
                        },
                        Err(e) => {
                            let default_ts = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_millis() as u64;
                            warn!("⚠️  Failed to parse genesis.json: {}, using current timestamp: {}", e, default_ts);
                            default_ts
                        }
                    }
                },
                Err(e) => {
                    let default_ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as u64;
                    warn!("⚠️  Failed to read genesis.json: {}, using current timestamp: {}", e, default_ts);
                    default_ts
                }
            }
        } else {
            let default_ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
            warn!("⚠️  genesis.json not found at {:?}, using current timestamp: {}", genesis_path, default_ts);
            default_ts
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
        
        // Load last_global_exec_index (for startup it is 0/genesis or whatever was loaded)
        let last_global_exec_index = 0; 
        info!("Loaded last_global_exec_index={} (genesis/startup default)", last_global_exec_index);

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
        
        // Create transition barrier (initialized to 0, will be set when epoch transition starts)
        let transition_barrier = Arc::new(AtomicU32::new(0));
        let transition_barrier_for_processor = transition_barrier.clone();
        
        // Create global_exec_index_at_barrier (initialized to 0, will be set when epoch transition starts)
        let global_exec_index_at_barrier = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let global_exec_index_at_barrier_for_processor = global_exec_index_at_barrier.clone();
        
        // Create pending transactions queue for barrier phase
        let pending_transactions_queue = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        
        // Create ordered commit processor
        let mut commit_processor = crate::commit_processor::CommitProcessor::new(commit_receiver)
            .with_commit_index_callback(move |index| {
                commit_index_for_callback.store(index, Ordering::SeqCst);
            })
            .with_epoch_info(current_epoch, last_global_exec_index)
            .with_transition_barrier(transition_barrier_for_processor)
            .with_global_exec_index_at_barrier(global_exec_index_at_barrier_for_processor)
            .with_pending_transactions_queue(pending_transactions_queue.clone());
        
        // Create executor client based on config settings
        // executor_read_enabled: allows reading committee state from Go
        // executor_commit_enabled: allows committing blocks to Go
        let node_id = config.node_id;
        let can_commit = config.executor_commit_enabled;

        // Only create executor client if reading is enabled
        let executor_client = if config.executor_read_enabled {
            Arc::new(ExecutorClient::new(true, can_commit, config.executor_send_socket_path.clone(), config.executor_receive_socket_path.clone()))
        } else {
            // Create disabled executor client for nodes that don't read from Go
            Arc::new(ExecutorClient::new(false, false, "".to_string(), "".to_string()))
        };
        info!("✅ Executor client configured (node_id={}, read_enabled={}, commit_enabled={}, send_socket={}, receive_socket={})",
            node_id, config.executor_read_enabled, can_commit, config.executor_send_socket_path, config.executor_receive_socket_path);

        let executor_client_for_init = executor_client.clone();
        tokio::spawn(async move {
            executor_client_for_init.initialize_from_go().await;
        });

        // Create checkpoint manager for checkpoint-based epoch transitions (Sui-style)
        let checkpoint_manager = if config.checkpoint_based_epochs {
            let blocks_per_epoch = config.blocks_per_epoch.unwrap_or(100);
            Some(Arc::new(crate::checkpoint::CheckpointManager::new(
                blocks_per_epoch,
                own_index,
            )))
        } else {
            None
        };

        // Create block counter for epoch transitions
        let blocks_in_current_epoch = Arc::new(std::sync::atomic::AtomicU32::new(0));

        // Create checkpoint-based epoch transition manager
        let checkpoint_epoch_transition_manager = if let Some(ref checkpoint_manager) = checkpoint_manager {
            Some(Arc::new(crate::checkpoint_epoch_transition::CheckpointEpochTransitionManager::new(
                checkpoint_manager.clone(),
                executor_client.clone(),
                config.clone(),
                own_index,
                blocks_in_current_epoch.clone(),
            )))
        } else {
            None
        };

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

        // Initialize epoch change manager
        let epoch_duration_seconds = config.epoch_duration_seconds.unwrap_or(0);
        let max_clock_drift_ms = config.max_clock_drift_seconds * 1000;
        let epoch_change_manager = Arc::new(RwLock::new(EpochChangeManager::new(
            current_epoch,
            Arc::new(committee.clone()),
            own_index,
            epoch_start_timestamp,
            config.time_based_epoch_change,
            epoch_duration_seconds,
            max_clock_drift_ms,
            None, // network_client
        )));

        // Clone executor client for Sui orchestrator
        let executor_client_clone = executor_client.clone();

        // Create Sui orchestrator after epoch_change_manager is available
        let sui_epoch_orchestrator_clone = Arc::new(SuiEpochTransitionOrchestrator::new(
            epoch_change_manager.clone(),
            executor_client_clone.clone(),
            config.clone(),
            current_epoch,
            own_index,
        ));

        // Setup commit processor with Sui epoch transition manager BEFORE spawning
        commit_processor = commit_processor.with_executor_client(executor_client.clone());
        commit_processor = commit_processor.with_epoch_transition_manager(sui_epoch_orchestrator_clone.sui_manager());

        // Setup checkpoint-based epoch transition manager (Sui-style)
        if let Some(ref manager) = checkpoint_epoch_transition_manager {
            commit_processor = commit_processor.with_checkpoint_epoch_transition_manager(manager.clone());
        }

        // Pass block counter for block-based epoch transitions
        let blocks_counter = Arc::new(std::sync::atomic::AtomicU32::new(0));
        commit_processor = commit_processor.with_blocks_in_current_epoch(blocks_counter);

        tokio::spawn(async move {
            if let Err(e) = commit_processor.run().await {
                tracing::error!("Commit processor error: {}", e);
            }
        });

        tokio::spawn(async move {
            use tracing::debug;
            while let Some(output) = block_receiver.recv().await {
                debug!("Received {} certified blocks", output.blocks.len());
            }
        });



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

        let protocol_keypair_for_epoch_task = protocol_keypair.clone();

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
        )
        .await;

        let transaction_client = authority.transaction_client();
        let transaction_client_proxy = Arc::new(TransactionClientProxy::new(transaction_client));

        // Initialize epoch change hook
        use crate::epoch_change_hook::EpochChangeHook;
        let epoch_change_hook = Arc::new(EpochChangeHook::new(
            epoch_change_manager.clone(),
            Arc::new(protocol_keypair_for_epoch_task.clone()),
            own_index,
        ));
        EpochChangeHook::init_global(epoch_change_hook);

        info!("Consensus node {} initialized successfully", config.node_id);

        // Use the Sui orchestrator created earlier
        let sui_epoch_orchestrator = sui_epoch_orchestrator_clone;

        // Start Sui-style epoch transition orchestrator
        sui_epoch_orchestrator.start().await?;

        // DISABLED: Time-based epoch change monitoring (replaced by integrated commit-based triggering)
        // Epoch transitions now happen automatically when processing commits, not via separate monitoring
        if config.time_based_epoch_change {
            info!("⚠️  Time-based epoch change monitoring DISABLED - replaced by integrated commit-based triggering");
            info!("   Epoch transitions will now be triggered automatically during commit processing");
        }

        // DISABLED: Epoch transition monitoring task (replaced by integrated commit-based triggering)
        // Epoch transitions are now triggered automatically during commit processing,
        // and reconfiguration is monitored in the simplified main loop

        Ok(Self {
            authority: Some(authority),
            transaction_client_proxy,
            epoch_change_manager,
            clock_sync_manager,
            sui_epoch_orchestrator: sui_epoch_orchestrator.clone(),
            current_commit_index,
            storage_path,
            current_epoch,
            last_global_exec_index,
            blocks_in_current_epoch: Some(Arc::new(std::sync::atomic::AtomicU32::new(0))),
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
            transition_barrier,
            global_exec_index_at_barrier,
            pending_transactions_queue,
            enable_lvm_snapshot: config.enable_lvm_snapshot,
            lvm_snapshot_bin_path: config.lvm_snapshot_bin_path,
            lvm_snapshot_delay_seconds: config.lvm_snapshot_delay_seconds,
        })
    }

    #[allow(dead_code)] 
    pub fn transaction_submitter(&self) -> Arc<dyn TransactionSubmitter> {
        self.transaction_client_proxy.clone() as Arc<dyn TransactionSubmitter>
    }

    #[allow(dead_code)]
    pub fn epoch_change_manager(&self) -> Arc<RwLock<EpochChangeManager>> {
        self.epoch_change_manager.clone()
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

        let manager = self.epoch_change_manager.read().await;
        
        let current_epoch = self.current_epoch;
        let all_pending_proposals = manager.get_all_pending_proposals();
        
        let has_catchup_proposals = all_pending_proposals.iter().any(|p| {
            p.new_epoch > current_epoch + 1
        });
        
        if has_catchup_proposals {
            let catchup_epochs: Vec<u64> = all_pending_proposals.iter()
                .filter(|p| p.new_epoch > current_epoch + 1)
                .map(|p| p.new_epoch)
                .collect();
            return (false, format!(
                "Node is catching up: current epoch {}, pending proposals for epochs {:?}",
                current_epoch, catchup_epochs
            ));
        }

        if self.last_transition_hash.is_some() {
            return (false, format!(
                "Epoch transition in progress: epoch {} -> {} (waiting for new authority to start)",
                current_epoch, current_epoch + 1
            ));
        }
        
        drop(manager);
        
        (true, "Node is ready".to_string())
    }
    
    pub async fn check_transaction_acceptance(&self) -> (bool, bool, String) {
        if self.authority.is_none() {
            return (false, false, "Node is still initializing".to_string());
        }

        let manager = self.epoch_change_manager.read().await;
        
        let current_epoch = self.current_epoch;
        let all_pending_proposals = manager.get_all_pending_proposals();
        
        let has_catchup_proposals = all_pending_proposals.iter().any(|p| {
            p.new_epoch > current_epoch + 1
        });
        
        if has_catchup_proposals {
            let catchup_epochs: Vec<u64> = all_pending_proposals.iter()
                .filter(|p| p.new_epoch > current_epoch + 1)
                .map(|p| p.new_epoch)
                .collect();
            drop(manager);
            return (false, false, format!(
                "Node is catching up: current epoch {}, pending proposals for epochs {:?}",
                current_epoch, catchup_epochs
            ));
        }

        if self.last_transition_hash.is_some() {
            drop(manager);
            return (false, false, format!(
                "Epoch transition in progress: epoch {} -> {} (waiting for new authority to start)",
                current_epoch, current_epoch + 1
            ));
        }
        
        let barrier_value = self.transition_barrier.load(Ordering::SeqCst);
        
        if barrier_value > 0 {
            let current_commit_index = self.current_commit_index.load(Ordering::SeqCst);
            drop(manager);
            info!("🔒 [FORK-SAFETY] Queueing transaction - barrier is set (barrier={}, current_commit={}): transaction will be queued for next epoch to prevent loss in commits past barrier (all nodes use same barrier from same proposal)", 
                barrier_value, current_commit_index);
            return (false, true, format!(
                "Barrier phase: barrier={} is set - transaction will be queued for next epoch (current_commit={})",
                barrier_value, current_commit_index
            ));
        }

        let current_commit_index = self.current_commit_index.load(Ordering::SeqCst);
        let next_epoch_proposals: Vec<_> = all_pending_proposals.iter()
            .filter(|p| p.new_epoch == current_epoch + 1)
            .collect();
        
        for proposal in next_epoch_proposals {
            let quorum_status = manager.check_proposal_quorum(proposal);
            if quorum_status == Some(true) {
                if current_commit_index >= proposal.proposal_commit_index {
                    let transition_commit_index = proposal.proposal_commit_index.saturating_add(10);
                    
                    let current_barrier = self.transition_barrier.load(Ordering::SeqCst);
                    if current_barrier == 0 {
                        self.transition_barrier.store(transition_commit_index, Ordering::SeqCst);
                        info!("🔒 [FORK-SAFETY] Set transition barrier EARLY to {} (quorum reached + proposal committed, current_commit={}) - commits past barrier will be skipped to prevent duplicate global_exec_index", 
                            transition_commit_index, current_commit_index);
                    } else if current_barrier != transition_commit_index {
                        warn!("⚠️ [FORK-SAFETY] Barrier already set to {} but proposal has barrier {} - using existing barrier", 
                            current_barrier, transition_commit_index);
                    }
                    
                    drop(manager);
                    info!("🔒 [FORK-SAFETY] Queueing transaction - pending proposal with quorum reached and committed (proposal_commit_index={}, barrier={}, current_commit={}): transaction will be queued for next epoch to prevent loss in commits past barrier", 
                        proposal.proposal_commit_index, transition_commit_index, current_commit_index);
                    return (false, true, format!(
                        "Barrier phase (pending): proposal with quorum reached and committed, barrier={} is set - transaction will be queued for next epoch (current_commit={})",
                        transition_commit_index, current_commit_index
                    ));
                }
            }
        }

        drop(manager);
        
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
            return Ok(0);
        }
        
        info!("📤 [TX FLOW] Submitting {} queued transactions to consensus in new epoch", original_count);
        
        use crate::tx_hash::calculate_transaction_hash;
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
        
        for tx_data in transactions {
            let transactions_vec = vec![tx_data];
            if let Err(e) = self.transaction_client_proxy.submit(transactions_vec).await {
                warn!("❌ [TX FLOW] Failed to submit queued transaction: {}", e);
            }
        }
        
        info!(
            "✅ [TX FLOW] Submitted {} queued transactions to consensus in deterministic order",
            unique_count
        );
        Ok(unique_count)
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

    #[allow(dead_code)]
    pub async fn transition_to_epoch(
        &mut self,
        proposal: &crate::epoch_change::EpochChangeProposal,
        current_commit_index: u32,
        config: &crate::config::NodeConfig,
    ) -> Result<()> {
        let proposal_hash = {
            let mgr = self.epoch_change_manager.read().await;
            mgr.hash_proposal(proposal)
        };
        if self.last_transition_hash.as_ref() == Some(&proposal_hash) {
            return Ok(());
        }
        self.last_transition_hash = Some(proposal_hash);

        info!("Transitioning to epoch {}...", proposal.new_epoch);
        
        info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        info!("🔄 EPOCH TRANSITION START: epoch {} -> {}", self.current_epoch, proposal.new_epoch);
        info!("  📊 Current State (BEFORE transition):");
        info!("    - Current epoch: {}", self.current_epoch);
        info!("    - Current commit index: {}", current_commit_index);
        info!("    - Last global exec index: {}", self.last_global_exec_index);
        info!("    - Proposal commit index: {}", proposal.proposal_commit_index);
        info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        
        let transition_commit_index = proposal.proposal_commit_index.saturating_add(10);
        let barrier_reached = current_commit_index >= transition_commit_index;
        
        let manager = self.epoch_change_manager.read().await;
        let quorum_reached = manager.check_proposal_quorum(proposal) == Some(true);
        let now_seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let proposal_age_seconds = now_seconds.saturating_sub(proposal.created_at_seconds);
        const TIMEOUT_SECONDS: u64 = 300; 
        let timeout_reached = proposal_age_seconds >= TIMEOUT_SECONDS;
        let timeout_exception = timeout_reached && quorum_reached && !barrier_reached;
        drop(manager);
        
        if !barrier_reached && !timeout_exception {
            return Err(anyhow::anyhow!(
                "FORK-SAFETY: Must wait until commit index {} (current: {}) to ensure all nodes transition together",
                transition_commit_index,
                current_commit_index
            ));
        }
        
        if timeout_exception {
            warn!("⏰ TIMEOUT EXCEPTION: Allowing transition despite barrier not reached");
        }
        
        let manager = self.epoch_change_manager.read().await;
        let quorum_status = manager.check_proposal_quorum(proposal);
        let quorum_reached = quorum_status == Some(true);
        
        let epoch_lag = proposal.new_epoch.saturating_sub(self.current_epoch);
        const MAX_EPOCH_LAG_FOR_QUORUM: u64 = 2;
        let is_catchup_mode = epoch_lag > MAX_EPOCH_LAG_FOR_QUORUM;
        
        if !quorum_reached && !is_catchup_mode {
            drop(manager);
            anyhow::bail!(
                "FORK-SAFETY: Quorum not reached for epoch transition - need 2f+1 votes (epoch lag: {})",
                epoch_lag
            );
        }
        drop(manager);
        
        let current_timestamp = self.epoch_change_manager.read().await.epoch_start_timestamp_ms();
        if current_timestamp != proposal.new_epoch_timestamp_ms {
            warn!("⚠️  Epoch timestamp mismatch: current={}, proposal={}", current_timestamp, proposal.new_epoch_timestamp_ms);
        }

        info!("✅ FORK-SAFE TRANSITION VALIDATED - All nodes will use last_commit_index={}", transition_commit_index);

        // Use Sui-style reconfiguration
        self.sui_epoch_orchestrator.handle_epoch_transition(proposal, current_commit_index).await?;

        self.transition_barrier.store(transition_commit_index, Ordering::SeqCst);
        info!("🔒 [FORK-SAFETY] Set transition barrier to {}", transition_commit_index);
        
        let old_epoch = self.current_epoch;
        let last_commit_index_at_barrier = transition_commit_index;
        let last_global_exec_index_at_barrier = calculate_global_exec_index(
            old_epoch,
            last_commit_index_at_barrier,
            self.last_global_exec_index,
        );
        info!("📊 [SNAPSHOT] Last block of epoch {}: global_exec_index={}", old_epoch, last_global_exec_index_at_barrier);

        self.graceful_shutdown().await?;

        let new_last_global_exec_index = last_global_exec_index_at_barrier;
        
        info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        info!("📊 FORK-SAFETY: Deterministic Values (ALL NODES MUST MATCH)");
        info!("    - Old epoch: {}", old_epoch);
        info!("    - New epoch: {}", proposal.new_epoch);
        info!("    - New last global exec index (new epoch): {} (DETERMINISTIC)", new_last_global_exec_index);
        info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

        // 4) Build new committee from Go state
        // FIX: LOOP INFINITELY to fetch from Go, ensure no fallback to local files.
        // Only create executor client for transition if reading is enabled
        let executor_client = if config.executor_read_enabled {
            Arc::new(ExecutorClient::new(true, false, config.executor_send_socket_path.clone(), config.executor_receive_socket_path.clone())) // không commit trong transition
        } else {
            warn!("⚠️  Cannot perform epoch transition: executor_read_enabled=false, cannot fetch committee from Go");
            anyhow::bail!("Executor read not enabled, cannot fetch committee for epoch transition");
        };

        // Set executor client for epoch change manager
        {
            let mut mgr = self.epoch_change_manager.write().await;
            mgr.set_executor_client(executor_client.clone());
        }

        let block_number = new_last_global_exec_index;
        
        info!("🔄 [EPOCH-TRANSITION] Fetching committee from Go state at block {}...", block_number);
        
        // This helper now contains the infinite loop. It will NOT return until Go responds successfully.
        let new_committee_raw = Self::build_committee_from_go_validators_at_block(&executor_client, block_number).await?;
        
        // Adjust epoch number in the received committee to match the new epoch
        let authorities: Vec<_> = new_committee_raw.authorities().map(|(_, auth)| auth.clone()).collect();
        let new_committee = Committee::new(proposal.new_epoch, authorities);
        
        info!("✅ [EPOCH-TRANSITION] Committee built from Go state: {} authorities.", new_committee.size());

        // FIX: DELETE OR COMMENT OUT THE FILE SAVE
        // We do NOT save committee.json anymore. State is purely in-memory and synchronized via Go.
        // crate::config::NodeConfig::save_committee_with_global_exec_index(...) -> REMOVED
        info!("🚫 [CONFIG] Skipped saving committee.json (running stateless/memory-only for committee config).");

        if let Some(authority) = self.authority.take() {
            authority.stop().await;
        }

        self.current_epoch = proposal.new_epoch;
        self.last_global_exec_index = new_last_global_exec_index;
        
        {
            let mut mgr = self.epoch_change_manager.write().await;
            mgr.reset_for_new_epoch(
                proposal.new_epoch,
                proposal.new_epoch_timestamp_ms,
            );
        }
        self.current_commit_index.store(0, Ordering::SeqCst);

        let db_path = self
            .storage_path
            .join("epochs")
            .join(format!("epoch_{}", proposal.new_epoch))
            .join("consensus_db");
        std::fs::create_dir_all(&db_path)?;

        self.transition_barrier.store(0, Ordering::SeqCst);
        self.global_exec_index_at_barrier.store(0, Ordering::SeqCst);
        info!("🔓 [FORK-SAFETY] Reset transition barrier and global_exec_index_at_barrier to 0 for new epoch");
        
        let (commit_consumer, commit_receiver, mut block_receiver) = CommitConsumerArgs::new(0, 0);
        let commit_index_for_callback = self.current_commit_index.clone();
        let new_epoch = proposal.new_epoch;
        let executor_commit_enabled = self.executor_commit_enabled;
        let executor_client_opt = if executor_commit_enabled {
            let client = Arc::new(ExecutorClient::new(true, true, config.executor_send_socket_path.clone(), config.executor_receive_socket_path.clone())); // luôn có thể commit trong transition
            Some(client)
        } else {
            None
        };
        
        let transition_barrier_for_new_epoch = self.transition_barrier.clone();
        let global_exec_index_at_barrier_for_new_epoch = self.global_exec_index_at_barrier.clone();
        
        // Store commit processor config for later spawning
        let commit_processor_config = (
            commit_receiver,
            commit_index_for_callback,
            new_epoch,
            new_last_global_exec_index,
            transition_barrier_for_new_epoch,
            global_exec_index_at_barrier_for_new_epoch,
            self.pending_transactions_queue.clone(),
            executor_client_opt,
        );

        let mut parameters = self.parameters.clone();
        parameters.db_path = db_path.clone();
        self.boot_counter = self.boot_counter.saturating_add(1);

        info!("🔁 Restarting authority in-process for epoch {} with db_path={:?}", proposal.new_epoch, parameters.db_path);

        let new_registry = Registry::new();
        
        let authority = ConsensusAuthority::start(
            NetworkType::Tonic,
            proposal.new_epoch_timestamp_ms,
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

        // NOW spawn commit processor AFTER consensus authority is ready
        let (commit_receiver, commit_index_for_callback, new_epoch, new_last_global_exec_index,
             transition_barrier_for_new_epoch, global_exec_index_at_barrier_for_new_epoch,
             pending_transactions_queue, executor_client_opt) = commit_processor_config;

        let mut commit_processor = crate::commit_processor::CommitProcessor::new(commit_receiver)
            .with_commit_index_callback(move |index| {
                commit_index_for_callback.store(index, Ordering::SeqCst);
            })
            .with_epoch_info(new_epoch, new_last_global_exec_index)
            .with_transition_barrier(transition_barrier_for_new_epoch)
            .with_global_exec_index_at_barrier(global_exec_index_at_barrier_for_new_epoch)
            .with_pending_transactions_queue(pending_transactions_queue);

        if let Some(ref client) = executor_client_opt {
            let executor_client_for_init = client.clone();
            tokio::spawn(async move {
                executor_client_for_init.initialize_from_go().await;
            });
            commit_processor = commit_processor.with_executor_client(client.clone());
        }

        tokio::spawn(async move {
            info!("🚀 [COMMIT PROCESSOR] Starting commit processor for new epoch {} (last_global_exec_index={})",
                new_epoch, new_last_global_exec_index);
            match commit_processor.run().await {
                Ok(()) => {
                    info!("✅ [COMMIT PROCESSOR] Commit processor exited normally (epoch {})",
                        new_epoch);
                }
                Err(e) => {
                    tracing::error!("❌ [COMMIT PROCESSOR] Commit processor error (epoch {}): {}",
                        new_epoch, e);
                }
            }
        });
        tokio::spawn(async move {
            use tracing::debug;
            while let Some(output) = block_receiver.recv().await {
                debug!("Received {} certified blocks", output.blocks.len());
            }
        });

        let queued_count = self.submit_queued_transactions().await?;
        if queued_count > 0 {
            info!("✅ Submitted {} queued transactions to consensus in new epoch {}", queued_count, proposal.new_epoch);
        }
        
        self.last_transition_hash = None;
        info!("✅ New authority started and ready to accept transactions");

        info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        info!("✅ EPOCH TRANSITION COMPLETE: epoch {} -> {}", old_epoch, proposal.new_epoch);
        info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

        // 🔄 SUI-STYLE: Advance Go state epoch to complete the transition
        if config.executor_commit_enabled {
            info!("🔄 [SUI-STYLE] Advancing Go state epoch to complete Sui-style transition");

            let advance_executor_client = Arc::new(ExecutorClient::new(
                true, // read enabled
                true, // commit enabled for advance epoch
                config.executor_send_socket_path.clone(),
                config.executor_receive_socket_path.clone(),
            ));

            match advance_executor_client.advance_epoch(proposal.new_epoch, proposal.new_epoch_timestamp_ms).await {
                Ok((confirmed_epoch, confirmed_timestamp)) => {
                    info!("✅ [SUI-STYLE] Successfully advanced Go state epoch: {} -> {} (timestamp: {})",
                        old_epoch, confirmed_epoch, confirmed_timestamp);
                    info!("🎉 COMPLETE SUI-STYLE EPOCH TRANSITION: Consensus + Go state synchronized!");
                }
                Err(e) => {
                    error!("❌ [SUI-STYLE] Failed to advance Go state epoch: {}", e);
                    error!("⚠️  WARNING: Consensus transitioned but Go state epoch not advanced!");
                    error!("🔄 This may cause epoch mismatch on restart - manual intervention may be needed");
                }
            }
        } else {
            warn!("⚠️  [SUI-STYLE] Executor commit disabled - Go state epoch not advanced");
            warn!("⚠️  WARNING: This breaks Sui-style epoch transition - Go state will be out of sync");
        }

        if self.enable_lvm_snapshot {
            if let Some(ref bin_path) = self.lvm_snapshot_bin_path {
                let bin_path = bin_path.clone();
                let new_epoch = proposal.new_epoch;
                let delay_seconds = self.lvm_snapshot_delay_seconds;
                
                tokio::spawn(async move {
                    tokio::time::sleep(tokio::time::Duration::from_secs(delay_seconds)).await;
                    info!("📸 Creating LVM snapshot for epoch {} (delay completed)", new_epoch);
                    
                    let bin_dir = bin_path.parent().unwrap_or_else(|| std::path::Path::new("."));
                    let output = std::process::Command::new("sudo")
                        .current_dir(bin_dir)
                        .arg(bin_path)
                        .arg("--id")
                        .arg(new_epoch.to_string())
                        .output();
                    
                    match output {
                        Ok(result) => {
                            if result.status.success() {
                                info!("✅ LVM snapshot created successfully for epoch {}", new_epoch);
                            } else {
                                let stderr = String::from_utf8_lossy(&result.stderr);
                                tracing::warn!("⚠️  LVM snapshot creation failed for epoch {}:\n{}", new_epoch, stderr);
                            }
                        }
                        Err(e) => {
                            tracing::error!("❌ Failed to execute LVM snapshot command for epoch {}: {}", new_epoch, e);
                        }
                    }
                });
            }
        }

        // Reset block counter for new epoch (block-based transitions)
        if let Some(atomic) = self.blocks_in_current_epoch.take() {
            atomic.store(0, Ordering::SeqCst);
            self.blocks_in_current_epoch = Some(atomic);
            info!("🔄 [BLOCK COUNTER] Reset to 0 for new epoch {}", proposal.new_epoch);
        }

        Ok(())
    }
}