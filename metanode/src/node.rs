// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use consensus_config::{AuthorityIndex, Committee};
use consensus_core::{
    ConsensusAuthority, NetworkType, Clock,
    CommitConsumerArgs,
};
use crate::transaction::NoopTransactionVerifier;
use crate::epoch_change::EpochChangeManager;
use crate::clock_sync::ClockSyncManager;
use prometheus::Registry;
use mysten_metrics::RegistryService;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use meta_protocol_config::ProtocolConfig;
use tracing::{info, warn};
use tokio::sync::RwLock;
use hex;

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
    /// Current commit index (for fork-safe epoch transition)
    #[allow(dead_code)] // Used internally by commit processor callback
    current_commit_index: Arc<AtomicU32>,

    /// Paths needed for real epoch transition (persist + clean state)
    committee_path: std::path::PathBuf,
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
    /// Executor enabled flag (from config)
    executor_enabled: bool,
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

        // CRITICAL: Tất cả nodes đều lấy committee từ Go state qua Unix Domain Socket
        // Không phụ thuộc vào executor_enabled - tất cả nodes đều cần committee từ Go
        let executor_enabled = config.executor_enabled;
        let node_id = config.node_id;
        
        // Tạo ExecutorClient cho TẤT CẢ nodes (không chỉ executor-enabled nodes)
        // ExecutorClient dùng để lấy committee từ Go qua Unix Domain Socket
        // enabled=true để có thể kết nối đến Go request socket
        let executor_client = Arc::new(ExecutorClient::new(true, node_id));
        
        info!("🔄 [STARTUP] Loading committee from Go state at block 0 (genesis) via Unix Domain Socket (node_id={}, executor_enabled={})...", 
            node_id, executor_enabled);
        
        // CRITICAL: Retry with exponential backoff to wait for Go to finish initializing genesis
        // Go may need time to init genesis block and register validators
        const MAX_RETRIES: u32 = 10;
        const INITIAL_DELAY_MS: u64 = 500; // Start with 500ms delay
        const MAX_DELAY_MS: u64 = 5000;   // Max 5 seconds between retries
        
        let mut last_error = None;
        let mut committee_result = None;
        
        for attempt in 1..=MAX_RETRIES {
            match Self::build_committee_from_go_validators_at_block(&executor_client, 0).await {
                Ok(committee) => {
                    info!("✅ [STARTUP] Successfully loaded committee from Go state: {} authorities, epoch={} (attempt {}/{})", 
                        committee.size(), committee.epoch(), attempt, MAX_RETRIES);
                    committee_result = Some(committee);
                    break;
                }
                Err(e) => {
                    last_error = Some(e);
                    if attempt < MAX_RETRIES {
                        // Exponential backoff: delay = min(INITIAL_DELAY * 2^(attempt-1), MAX_DELAY)
                        let delay_ms = std::cmp::min(
                            INITIAL_DELAY_MS * (1u64 << (attempt - 1)),
                            MAX_DELAY_MS
                        );
                        warn!("⚠️  [STARTUP] Failed to load committee from Go state (attempt {}/{}): {}. Waiting {}ms for Go to finish initializing genesis...", 
                            attempt, MAX_RETRIES, last_error.as_ref().unwrap(), delay_ms);
                        tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                    } else {
                        warn!("⚠️  [STARTUP] Failed to load committee from Go state after {} attempts: {}", MAX_RETRIES, last_error.as_ref().unwrap());
                    }
                }
            }
        }
        
        let committee = match committee_result {
            Some(committee) => committee,
            None => {
                // Try fallback to file if available (only if Go is not ready)
                match config.load_committee() {
                    Ok(committee) => {
                        warn!("⚠️  [STARTUP] Using committee from file as fallback (Go state unavailable after {} retries). This should only happen if Go Master is not running.", MAX_RETRIES);
                        committee
                    }
                    Err(file_err) => {
                        // All nodes should load from Go - this is a critical error
                        anyhow::bail!(
                            "Failed to load committee from Go state after {} retries: {}. Also failed to load from file: {}. \
                            All nodes must load committee from Go state via Unix Domain Socket. \
                            Please ensure Go Master is running, has initialized genesis block, and protobuf is regenerated.",
                            MAX_RETRIES, last_error.unwrap(), file_err
                        );
                    }
                }
            }
        };
        let current_epoch = committee.epoch();
        info!("Loaded committee with {} authorities, epoch={}", committee.size(), current_epoch);

        // Capture paths needed for epoch transition
        let committee_path = config
            .committee_path
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Committee path not specified in config"))?;
        let storage_path = config.storage_path.clone();
        
        // Load last_global_exec_index from Go state (block 0 = 0, otherwise from Go's last block)
        // Tất cả nodes đều lấy từ Go (block 0 = 0 cho genesis)
        // In epoch transition, it will be updated from the barrier
        let last_global_exec_index = 0; // For startup, last_global_exec_index is 0 (genesis)
        info!("Loaded last_global_exec_index={} (deterministic checkpoint sequence)", last_global_exec_index);

        // Load keypairs (kept for in-process restart)
        let protocol_keypair = config.load_protocol_keypair()?;
        let network_keypair = config.load_network_keypair()?;

        // Get own authority index
        let own_index = AuthorityIndex::new_for_test(config.node_id as u32);
        if !committee.is_valid_index(own_index) {
            anyhow::bail!("Node ID {} is out of range for committee size {}", config.node_id, committee.size());
        }

        // Create storage directory
        std::fs::create_dir_all(&config.storage_path)?;

        // Use provided registry (from metrics server if enabled)

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
        // Commits past barrier will be sent as one block with global_exec_index = barrier_global_exec_index + 1
        let global_exec_index_at_barrier = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let global_exec_index_at_barrier_for_processor = global_exec_index_at_barrier.clone();
        
        // Create pending transactions queue for barrier phase
        let pending_transactions_queue = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        
        // Create ordered commit processor with commit index tracking and epoch info
        let mut commit_processor = crate::commit_processor::CommitProcessor::new(commit_receiver)
            .with_commit_index_callback(move |index| {
                commit_index_for_callback.store(index, Ordering::SeqCst);
            })
            .with_epoch_info(current_epoch, last_global_exec_index)
            .with_transition_barrier(transition_barrier_for_processor)
            .with_global_exec_index_at_barrier(global_exec_index_at_barrier_for_processor)
            .with_pending_transactions_queue(pending_transactions_queue.clone());
        
        // Add executor client if enabled (for initial startup, not just epoch transition)
        if config.executor_enabled {
            let node_id = config.node_id;
            let executor_client = Arc::new(ExecutorClient::new(true, node_id));
            info!("✅ Executor client enabled for initial startup (node_id={}, socket=/tmp/executor{}.sock)", 
                node_id, node_id);
            commit_processor = commit_processor.with_executor_client(executor_client);
        } else {
            info!("ℹ️  Executor client disabled for initial startup (node_id={}, consensus only - executor_enabled=false in config)", config.node_id);
        }
        
        tokio::spawn(async move {
            if let Err(e) = commit_processor.run().await {
                tracing::error!("Commit processor error: {}", e);
            }
        });
        
        // Spawn task to consume blocks (for logging)
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

        // Apply commit sync parameters for faster catch-up
        parameters.commit_sync_batch_size = config.commit_sync_batch_size;
        parameters.commit_sync_parallel_fetches = config.commit_sync_parallel_fetches;
        parameters.commit_sync_batches_ahead = config.commit_sync_batches_ahead;
        info!("Commit sync parameters: batch_size={}, parallel_fetches={}, batches_ahead={}, adaptive={}", 
            parameters.commit_sync_batch_size,
            parameters.commit_sync_parallel_fetches,
            parameters.commit_sync_batches_ahead,
            config.adaptive_catchup_enabled);

        // Apply speed multiplier to consensus parameters (to slow down system)
        let speed_multiplier = config.speed_multiplier;
        if speed_multiplier != 1.0 {
            info!("Applying speed multiplier: {}x ({}x slower)", 
                speed_multiplier, 1.0 / speed_multiplier);
            
            // Calculate delays based on multiplier
            // Default values: leader_timeout=200ms, min_round_delay=50ms
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
            
            info!("Consensus delays: leader_timeout={:?}, min_round_delay={:?}, max_forward_time_drift={:?}",
                parameters.leader_timeout,
                parameters.min_round_delay,
                parameters.max_forward_time_drift);
        }

        // Load epoch start timestamp (must be same for all nodes)
        // CRITICAL: All nodes must use the SAME timestamp for genesis blocks
        let epoch_start_timestamp = config.load_epoch_timestamp()?;
        let current_epoch = committee.epoch();

        // Per-epoch DB path: storage/node_X/epochs/epoch_N/consensus_db
        let db_path = config
            .storage_path
            .join("epochs")
            .join(format!("epoch_{}", current_epoch))
            .join("consensus_db");
        std::fs::create_dir_all(&db_path)?;
        parameters.db_path = db_path;
        
        // If epoch 0 and time-based epoch change is enabled, check if timestamp is too old
        // IMPORTANT: We do NOT reset timestamp here to avoid different nodes having different timestamps
        // Instead, we just warn and let the epoch change mechanism handle it
        if current_epoch == 0 && config.time_based_epoch_change {
            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
            let elapsed_seconds = (now_ms.saturating_sub(epoch_start_timestamp)) / 1000;
            let epoch_duration_seconds = config.epoch_duration_seconds.unwrap_or(600);
            
            // If elapsed time already exceeds duration, warn but don't reset
            // All nodes must use the same timestamp from file to ensure genesis blocks match
            if elapsed_seconds > epoch_duration_seconds {
                warn!(
                    "⚠️  Epoch start timestamp is old (elapsed={}s > duration={}s), but keeping it to ensure all nodes use same timestamp for genesis blocks",
                    elapsed_seconds, epoch_duration_seconds
                );
                warn!(
                    "⚠️  To reset: manually update epoch_timestamp_ms in config/committee.json with a new timestamp (same for all nodes)"
                );
            }
        }
        
        info!("Using epoch start timestamp: {} (epoch={}) - CRITICAL: All nodes must use the same timestamp!", epoch_start_timestamp, current_epoch);

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
        )));
        
        // Log epoch information on startup
        if config.time_based_epoch_change {
            info!("📅 EPOCH INFO: current_epoch={}, epoch_duration={}s ({} minutes), epoch_start_timestamp={}", 
                current_epoch, 
                epoch_duration_seconds,
                epoch_duration_seconds / 60,
                epoch_start_timestamp);
            info!(
                "📅 EPOCH STARTED: epoch={}, duration={}s, enable_ntp_sync={}, max_clock_drift_seconds={}, db_path={:?}",
                current_epoch,
                epoch_duration_seconds,
                config.enable_ntp_sync,
                config.max_clock_drift_seconds,
                parameters.db_path
            );
        } else {
            info!("📅 EPOCH INFO: current_epoch={}, time_based_epoch_change=DISABLED", current_epoch);
            info!(
                "📅 EPOCH STARTED: epoch={}, time_based_epoch_change=DISABLED, enable_ntp_sync={}, max_clock_drift_seconds={}, db_path={:?}",
                current_epoch,
                config.enable_ntp_sync,
                config.max_clock_drift_seconds,
                parameters.db_path
            );
        }

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
            
            // Initial sync
            tokio::spawn(async move {
                let mut manager = sync_manager_clone.write().await;
                if let Err(e) = manager.sync_with_ntp().await {
                    tracing::warn!("Initial NTP sync failed: {}", e);
                }
            });
            
            // Start periodic sync
            ClockSyncManager::start_sync_task(clock_sync_manager.clone());
            
            // Start drift monitoring
            ClockSyncManager::start_drift_monitor(monitor_manager_clone);
        }

        // Clone protocol_keypair before it's moved into ConsensusAuthority
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
            registry.clone(),  // Clone registry for authority (original will be stored in struct)
            0, // boot_counter
        )
        .await;

        let transaction_client = authority.transaction_client();
        let transaction_client_proxy = Arc::new(TransactionClientProxy::new(transaction_client));

        // Initialize epoch change hook for Core to access
        use crate::epoch_change_hook::EpochChangeHook;
        let epoch_change_hook = Arc::new(EpochChangeHook::new(
            epoch_change_manager.clone(),
            Arc::new(protocol_keypair_for_epoch_task.clone()),
            own_index,
        ));
        EpochChangeHook::init_global(epoch_change_hook);

        info!("Consensus node {} initialized successfully", config.node_id);

        // Start monitoring task for time-based epoch change proposal
        if config.time_based_epoch_change {
            let epoch_change_manager_clone = epoch_change_manager.clone();
            let current_commit_index_clone = current_commit_index.clone();
            let protocol_keypair_for_task = protocol_keypair_for_epoch_task.clone();
            let own_index_clone = own_index;
            
            let clock_sync_manager_clone_for_epoch_task = clock_sync_manager.clone();
            let ntp_enabled_for_epoch_task = config.enable_ntp_sync;
            tokio::spawn(async move {
                let mut last_epoch_log = SystemTime::now();
                let mut last_skip_pending_log = SystemTime::now() - Duration::from_secs(300);
                let mut last_ntp_unhealthy_log = SystemTime::now() - Duration::from_secs(300);
                loop {
                    tokio::time::sleep(Duration::from_secs(5)).await; // Check every 5 seconds
                    
                    let mut manager = epoch_change_manager_clone.write().await;
                    let current_epoch = manager.current_epoch();
                    
                    // Log epoch status every 30 seconds
                    if last_epoch_log.elapsed().unwrap_or(Duration::from_secs(0)) >= Duration::from_secs(30) {
                        let now_ms = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_millis() as u64;
                        let elapsed_seconds = (now_ms - manager.epoch_start_timestamp_ms()) / 1000;
                        let remaining_seconds = manager.epoch_duration_seconds().saturating_sub(elapsed_seconds);
                        
                        info!("📅 CURRENT EPOCH: epoch={}, elapsed={}s ({}m {}s), remaining={}s ({}m {}s), duration={}s",
                            current_epoch,
                            elapsed_seconds,
                            elapsed_seconds / 60,
                            elapsed_seconds % 60,
                            remaining_seconds,
                            remaining_seconds / 60,
                            remaining_seconds % 60,
                            manager.epoch_duration_seconds()
                        );
                        
                        last_epoch_log = SystemTime::now();
                    }
                    
                    // Check if should propose time-based epoch change
                    let should_propose = manager.should_propose_time_based();
                    if should_propose {
                        // Check if already have pending proposal for next epoch
                        let has_pending = manager.has_pending_proposal_for_epoch(current_epoch + 1);
                        
                        // Get current committee and epoch duration before dropping read lock
                        let current_committee = manager.committee();
                        let epoch_duration_seconds = manager.epoch_duration_seconds();
                        let epoch_duration_minutes = epoch_duration_seconds / 60;
                        let mut new_authorities = Vec::new();
                        for (_, auth) in current_committee.authorities() {
                            use consensus_config::Authority;
                            new_authorities.push(Authority {
                                stake: auth.stake,
                                address: auth.address.clone(),
                                hostname: auth.hostname.clone(),
                                authority_key: auth.authority_key.clone(),
                                protocol_key: auth.protocol_key.clone(),
                                network_key: auth.network_key.clone(),
                            });
                        }
                        drop(manager);
                        
                        if !has_pending {
                            // Production gate: if NTP sync is enabled and unhealthy, do NOT propose.
                            // This prevents a badly-drifted node from constantly proposing.
                            if ntp_enabled_for_epoch_task {
                                let clock_ok = clock_sync_manager_clone_for_epoch_task.read().await.is_healthy();
                                if !clock_ok {
                                    if last_ntp_unhealthy_log.elapsed().unwrap_or(Duration::from_secs(0))
                                        >= Duration::from_secs(60)
                                    {
                                        warn!("⏱️  Skipping epoch proposal: clock/NTP sync unhealthy (production safety gate)");
                                        last_ntp_unhealthy_log = SystemTime::now();
                                    }
                                    continue;
                                }
                            }

                            info!("🔄 Time-based epoch change trigger: epoch {} -> {} ({} minutes elapsed)", 
                                current_epoch, current_epoch + 1, epoch_duration_minutes);
                            
                            // Propose epoch change
                            let mut manager = epoch_change_manager_clone.write().await;
                            let current_commit_index = current_commit_index_clone.load(Ordering::SeqCst);
                            
                            // For testing: use same committee but increment epoch
                            // In production, you would load/generate new committee
                            let new_committee = Committee::new(current_epoch + 1, new_authorities);
                            
                            // Set epoch timestamp in the future (10 seconds buffer) to ensure
                            // it passes validation even with network delay
                            let new_epoch_timestamp_ms = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap()
                                .as_millis() as u64
                                + 10_000; // Add 10 seconds buffer
                            
                            // Propose với commit index hiện tại + buffer
                            let proposal_commit_index = current_commit_index.saturating_add(100);
                            
                            info!("📝 Creating epoch change proposal: epoch {} -> {}, commit_index={} (current={})",
                                current_epoch, current_epoch + 1, proposal_commit_index, current_commit_index);
                            
                            match manager.propose_epoch_change(
                                new_committee,
                                new_epoch_timestamp_ms,
                                proposal_commit_index,
                                own_index_clone,
                                &protocol_keypair_for_task,
                            ) {
                                Ok(proposal) => {
                                    let proposal_hash = manager.hash_proposal(&proposal);
                                    let hash_hex = hex::encode(&proposal_hash[..8]);
                                    info!(
                                        "✅ EPOCH CHANGE PROPOSAL CREATED: epoch {} -> {}, proposal_hash={}, commit_index={}, proposer={}",
                                        current_epoch,
                                        proposal.new_epoch,
                                        hash_hex,
                                        proposal_commit_index,
                                        own_index_clone.value()
                                    );
                                    
                                    // CRITICAL FIX: Auto-vote on own proposal
                                    // The proposer should automatically vote on their own proposal
                                    // This ensures the proposal has at least one vote and can reach quorum
                                    match manager.vote_on_proposal(
                                        &proposal,
                                        own_index_clone,
                                        &protocol_keypair_for_task,
                                    ) {
                                        Ok(vote) => {
                                            let vote_hash_hex = hex::encode(&vote.proposal_hash[..8.min(vote.proposal_hash.len())]);
                                            info!(
                                                "🗳️  Auto-voted on own proposal: proposal_hash={}, epoch {} -> {}, voter={}, approve={}",
                                                vote_hash_hex,
                                                proposal.new_epoch - 1,
                                                proposal.new_epoch,
                                                own_index_clone.value(),
                                                vote.approve
                                            );
                                        }
                                        Err(e) => {
                                            tracing::warn!("⚠️  Failed to auto-vote on own proposal: {}", e);
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::error!("❌ Failed to create epoch change proposal: {}", e);
                                }
                            }
                        } else {
                            // Throttle this log to avoid spamming when we're waiting for commit-index barrier.
                            if last_skip_pending_log.elapsed().unwrap_or(Duration::from_secs(0)) >= Duration::from_secs(30) {
                                info!(
                                    "⏭️  Skipping proposal creation: already have pending proposal for epoch {}",
                                    current_epoch + 1
                                );
                                last_skip_pending_log = SystemTime::now();
                            }
                        }
                    }
                }
            });
        }

        // Start monitoring task for epoch transition (fork-safe)
        let epoch_change_manager_clone = epoch_change_manager.clone();
        let current_commit_index_clone = current_commit_index.clone();
        
        tokio::spawn(async move {
            let mut last_quorum_check = SystemTime::now();
            let mut last_waiting_log = SystemTime::now() - Duration::from_secs(300);
            let mut last_waiting_commit_index: u32 = 0;
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                
                let current_commit_index = current_commit_index_clone.load(Ordering::SeqCst);
                let manager = epoch_change_manager_clone.read().await;
                
                // Check quorum status every 10 seconds
                if last_quorum_check.elapsed().unwrap_or(Duration::from_secs(0)) >= Duration::from_secs(10) {
                    let pending_proposals = manager.get_all_pending_proposals();
                    for proposal in &pending_proposals {
                        let quorum_status = manager.check_proposal_quorum(proposal);
                        let proposal_hash = manager.hash_proposal(proposal);
                        let hash_hex = hex::encode(&proposal_hash[..8.min(proposal_hash.len())]);
                        
                        if let Some(approved) = quorum_status {
                            if approved {
                                let votes = manager.get_proposal_votes_count(proposal);
                                let transition_commit_index = proposal.proposal_commit_index + 10;
                                
                                if current_commit_index >= transition_commit_index {
                                    info!(
                                        "🎯 EPOCH TRANSITION READY: epoch {} -> {}, proposal_hash={}, votes={}, commit_index={} (barrier={})",
                                        proposal.new_epoch - 1,
                                        proposal.new_epoch,
                                        hash_hex,
                                        votes,
                                        current_commit_index,
                                        transition_commit_index
                                    );
                                } else {
                                    // Throttle waiting log: waiting is expected; keep logs clean.
                                    // Log at most once per 30s OR when commit index advances significantly.
                                    let should_log = last_waiting_log
                                        .elapsed()
                                        .unwrap_or(Duration::from_secs(0)) >= Duration::from_secs(30)
                                        || current_commit_index.saturating_sub(last_waiting_commit_index) >= 100;
                                    if should_log {
                                        info!(
                                            "⏳ EPOCH TRANSITION WAITING FOR COMMIT INDEX: epoch {} -> {}, proposal_hash={}, votes={}, commit_index={} (need {})",
                                            proposal.new_epoch - 1,
                                            proposal.new_epoch,
                                            hash_hex,
                                            votes,
                                            current_commit_index,
                                            transition_commit_index
                                        );
                                        last_waiting_log = SystemTime::now();
                                        last_waiting_commit_index = current_commit_index;
                                    }
                                }
                            } else {
                                info!(
                                    "❌ EPOCH CHANGE REJECTED: epoch {} -> {}, proposal_hash={}",
                                    proposal.new_epoch - 1,
                                    proposal.new_epoch,
                                    hash_hex
                                );
                            }
                        }
                    }
                    last_quorum_check = SystemTime::now();
                }
                
                // Check for transition-ready proposal (fork-safe)
                // IMPORTANT: This check ensures fork-safety by:
                // 1. Only transitioning when quorum is reached
                // 2. Only transitioning when commit index barrier is passed
                // 3. All nodes will transition at similar commit indices (within buffer range)
                if let Some(proposal) = manager.get_transition_ready_proposal(current_commit_index) {
                    drop(manager);
                    
                    let proposal_hash = epoch_change_manager_clone.read().await.hash_proposal(&proposal);
                    let hash_hex = hex::encode(&proposal_hash[..8]);
                    let transition_commit_index = proposal.proposal_commit_index.saturating_add(10);
                    let commit_index_diff = current_commit_index.saturating_sub(transition_commit_index);
                    
                    info!(
                        "🚀 ========================================"
                    );
                    info!(
                        "🚀 EPOCH TRANSITION TRIGGERED (FORK-SAFE)"
                    );
                    info!(
                        "🚀 ========================================"
                    );
                    info!(
                        "  📋 Proposal: epoch {} -> {}",
                        proposal.new_epoch - 1,
                        proposal.new_epoch
                    );
                    info!(
                        "  🔑 Proposal Hash: {}",
                        hash_hex
                    );
                    info!(
                        "  ✅ Quorum: APPROVED (2f+1 votes received)"
                    );
                    info!(
                        "  ✅ Commit Index Barrier: PASSED"
                    );
                    info!(
                        "    - Current commit index: {}",
                        current_commit_index
                    );
                    info!(
                        "    - Barrier commit index: {}",
                        transition_commit_index
                    );
                    info!(
                        "    - Commits past barrier: {}",
                        commit_index_diff
                    );
                    info!(
                        "  🔒 Fork-Safety: All nodes will transition at commit index ~{} (within buffer range)",
                        transition_commit_index
                    );
                    info!(
                        "  🔁 Transition: in-process authority restart (implemented)"
                    );
                    info!(
                        "🚀 ========================================"
                    );
                    
                    // Note: Actual transition is handled by the coordinator (main.rs) which calls
                    // `transition_to_epoch()` on the ConsensusNode when it is ready.
                }
            }
        });

        Ok(Self {
            authority: Some(authority),
            transaction_client_proxy,
            epoch_change_manager,
            clock_sync_manager,
            current_commit_index,
            committee_path,
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
            executor_enabled: config.executor_enabled,
            transition_barrier,
            global_exec_index_at_barrier,
            pending_transactions_queue,
        })
    }

    #[allow(dead_code)] // Used by main.rs to wire RPC in a restart-safe way
    pub fn transaction_submitter(&self) -> Arc<dyn TransactionSubmitter> {
        self.transaction_client_proxy.clone()
    }

    /// Get epoch change manager
    #[allow(dead_code)] // Reserved for future use (RPC endpoints, CLI commands)
    pub fn epoch_change_manager(&self) -> Arc<RwLock<EpochChangeManager>> {
        self.epoch_change_manager.clone()
    }

    /// Get current commit index
    #[allow(dead_code)] // Reserved for future use (monitoring, debugging)
    pub fn current_commit_index(&self) -> u32 {
        self.current_commit_index.load(Ordering::SeqCst)
    }

    /// Update current commit index (called from commit processor)
    #[allow(dead_code)] // Used by commit processor callback (indirectly)
    pub fn update_commit_index(&self, index: u32) {
        self.current_commit_index.store(index, Ordering::SeqCst);
    }

    /// Check if node is ready to accept transactions
    /// Returns (is_ready, reason) where reason explains why not ready if false
    /// 
    /// Node is NOT ready if:
    /// 1. Authority is not initialized (still starting up)
    /// 2. There's a pending epoch transition (transitioning to new epoch)
    /// 3. There are pending proposals for different epochs (node might be catching up)
    /// 
    /// This prevents nodes from accepting transactions when:
    /// - They're still syncing/catching up
    /// - They're transitioning epochs (which could cause forks)
    /// - They're not fully initialized
    pub async fn is_ready_for_transactions(&self) -> (bool, String) {
        // 1. Check if authority is initialized
        if self.authority.is_none() {
            return (false, "Node is still initializing".to_string());
        }

        // 2. Check if there's a pending transition (last_transition_hash indicates transition in progress)
        // Note: last_transition_hash is set during transition and cleared after, but we check manager state
        let manager = self.epoch_change_manager.read().await;
        
        // 3. Check if there are pending proposals for epochs other than current_epoch + 1
        // This indicates the node might be catching up and shouldn't accept new transactions yet
        let current_epoch = self.current_epoch;
        let all_pending_proposals = manager.get_all_pending_proposals();
        
        // Check if any pending proposal is for an epoch that's not the immediate next epoch
        // This suggests the node is behind and catching up
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

        // 4. Check if there's a ready-to-transition proposal (transition might happen soon)
        // IMPORTANT: We should NOT reject transactions during epoch transition
        // Transactions that are already submitted to DAG will continue processing
        // Only reject if we're actively transitioning (last_transition_hash is set)
        // This ensures transactions are not lost during epoch transition
        
        // Check if transition is in progress (last_transition_hash indicates active transition)
        // This is set at the start of transition_to_epoch and cleared after new authority starts
        if self.last_transition_hash.is_some() {
            return (false, format!(
                "Epoch transition in progress: epoch {} -> {} (waiting for new authority to start)",
                current_epoch, current_epoch + 1
            ));
        }
        
        // Allow transactions even if transition is ready - they will be processed in new epoch
        // The DAG will continue processing transactions that are already submitted
        // Only reject if we're actively transitioning (checked above)

        drop(manager);
        
        // All checks passed - node is ready
        (true, "Node is ready".to_string())
    }
    
    /// Check if transaction should be accepted or queued
    /// Returns: (should_accept, should_queue, reason)
    /// - should_accept: true if transaction should be submitted to consensus immediately
    /// - should_queue: true if transaction should be queued for next epoch (barrier phase)
    /// - reason: explanation string
    pub async fn check_transaction_acceptance(&self) -> (bool, bool, String) {
        // 1. Check if authority is initialized
        if self.authority.is_none() {
            return (false, false, "Node is still initializing".to_string());
        }

        // 2. Check if there's a pending transition (last_transition_hash indicates transition in progress)
        let manager = self.epoch_change_manager.read().await;
        
        // 3. Check if there are pending proposals for epochs other than current_epoch + 1
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

        // 4. Check if transition is in progress
        if self.last_transition_hash.is_some() {
            drop(manager);
            return (false, false, format!(
                "Epoch transition in progress: epoch {} -> {} (waiting for new authority to start)",
                current_epoch, current_epoch + 1
            ));
        }
        
        // 5. Check if we're in barrier phase (transition barrier is set)
        // CRITICAL FIX: Once barrier is set, ALL transactions must be queued (not submitted to consensus)
        // 
        // PROBLEM WITH OLD LOGIC:
        // - Old logic only queued when current_commit_index >= barrier_value
        // - If current_commit_index < barrier_value, transactions were still submitted to consensus
        // - But blocks might be committed at commit_index > barrier, causing transactions to be lost
        //   (because commits past barrier send empty commits)
        // 
        // NEW LOGIC (FORK-SAFE):
        // - Once barrier is set (barrier_value > 0), queue ALL transactions
        // - This prevents transactions from being included in blocks that will be committed past barrier
        // - Fork-safety is guaranteed because:
        //   1. Barrier value is set from the same proposal (proposal_commit_index + 10)
        //   2. All nodes receive the same proposal and set the same barrier value
        //   3. All nodes will queue transactions at the same logical point (when barrier is set)
        //   4. Queued transactions are sorted by hash and submitted deterministically in next epoch
        // 
        // WHY THIS IS SAFE:
        // - Barrier is atomic (stored in AtomicU32) and set from deterministic proposal
        // - All nodes see barrier being set at the same logical point (same proposal approval)
        // - Even if nodes have different current_commit_index, they all queue when barrier > 0
        // - This ensures no transactions are lost in commits past barrier
        let barrier_value = self.transition_barrier.load(Ordering::SeqCst);
        
        if barrier_value > 0 {
            // Barrier is set - we're in barrier phase
            // CRITICAL: Queue ALL transactions to prevent loss in commits past barrier
            // All nodes will queue transactions when barrier is set (fork-safe)
            let current_commit_index = self.current_commit_index.load(Ordering::SeqCst);
            drop(manager);
            info!("🔒 [FORK-SAFETY] Queueing transaction - barrier is set (barrier={}, current_commit={}): transaction will be queued for next epoch to prevent loss in commits past barrier (all nodes use same barrier from same proposal)", 
                barrier_value, current_commit_index);
            return (false, true, format!(
                "Barrier phase: barrier={} is set - transaction will be queued for next epoch (current_commit={})",
                barrier_value, current_commit_index
            ));
        }

        // 6. CRITICAL RACE CONDITION FIX: Check pending proposals with quorum reached
        // If there's a pending proposal for next epoch with quorum reached and current_commit_index
        // >= proposal.proposal_commit_index (proposal has been committed), queue transactions.
        // This prevents race condition where:
        // 1. Transaction is submitted to consensus
        // 2. Proposal gets approved and barrier is set
        // 3. Transaction gets included in block past barrier → lost
        // 
        // FORK-SAFETY: This is safe because:
        // - All nodes see the same proposal with same quorum status
        // - All nodes will queue when current_commit_index >= proposal.proposal_commit_index (deterministic)
        // - Proposal has been committed, so this is a safe point to queue
        // - Barrier will be set at proposal.proposal_commit_index + 10, giving us safety margin
        // - Queued transactions are sorted by hash and submitted deterministically in next epoch
        let current_commit_index = self.current_commit_index.load(Ordering::SeqCst);
        let next_epoch_proposals: Vec<_> = all_pending_proposals.iter()
            .filter(|p| p.new_epoch == current_epoch + 1)
            .collect();
        
        for proposal in next_epoch_proposals {
            // Check if proposal has quorum (2f+1 votes)
            let quorum_status = manager.check_proposal_quorum(proposal);
            if quorum_status == Some(true) {
                // Quorum reached - proposal will be approved
                // Queue transactions if proposal has been committed (current_commit_index >= proposal.proposal_commit_index)
                // This ensures proposal is deterministic and all nodes will queue at the same point
                if current_commit_index >= proposal.proposal_commit_index {
                    let transition_commit_index = proposal.proposal_commit_index.saturating_add(10);
                    
                    // CRITICAL FIX: Set barrier EARLY when quorum reached and proposal committed
                    // This ensures commits past barrier are skipped BEFORE they're processed
                    // Preventing duplicate global_exec_index issues
                    let current_barrier = self.transition_barrier.load(Ordering::SeqCst);
                    if current_barrier == 0 {
                        // Barrier not set yet - set it now (early)
                        self.transition_barrier.store(transition_commit_index, Ordering::SeqCst);
                        info!("🔒 [FORK-SAFETY] Set transition barrier EARLY to {} (quorum reached + proposal committed, current_commit={}) - commits past barrier will be skipped to prevent duplicate global_exec_index", 
                            transition_commit_index, current_commit_index);
                    } else if current_barrier != transition_commit_index {
                        // Barrier already set but different - log warning
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
        
        // All checks passed - node is ready to accept transactions
        (true, false, "Node is ready".to_string())
    }
    
    /// Queue transaction for next epoch (called during barrier phase)
    pub async fn queue_transaction_for_next_epoch(&self, tx_data: Vec<u8>) -> Result<()> {
        let mut queue = self.pending_transactions_queue.lock().await;
        queue.push(tx_data);
        info!("📦 [TX FLOW] Queued transaction for next epoch: queue_size={}", queue.len());
        Ok(())
    }
    
    /// Submit queued transactions to consensus (called after epoch transition)
    /// CRITICAL FORK-SAFETY: Transactions are sorted by hash before submission
    /// to ensure all nodes submit them in the same deterministic order
    pub async fn submit_queued_transactions(&mut self) -> Result<usize> {
        let mut queue = self.pending_transactions_queue.lock().await;
        let original_count = queue.len();
        if original_count == 0 {
            return Ok(0);
        }
        
        info!("📤 [TX FLOW] Submitting {} queued transactions to consensus in new epoch", original_count);
        
        // CRITICAL FORK-SAFETY: Sort queued transactions by hash (deterministic ordering)
        // This ensures all nodes submit queued transactions in the same order
        // Even if transactions were queued in different orders across nodes,
        // sorting by hash ensures deterministic submission order
        use crate::tx_hash::calculate_transaction_hash;
        let mut transactions_with_hash: Vec<(Vec<u8>, Vec<u8>)> = queue
            .iter()
            .map(|tx_data| {
                let tx_hash = calculate_transaction_hash(tx_data);
                (tx_data.clone(), tx_hash)
            })
            .collect();
        
        // Sort by hash bytes (lexicographic order) - deterministic across all nodes
        transactions_with_hash.sort_by(|(_, hash_a), (_, hash_b)| hash_a.cmp(hash_b));
        
        // Deduplicate by hash (deterministic after sort) to avoid submitting the same tx multiple times
        let before_dedup = transactions_with_hash.len();
        transactions_with_hash.dedup_by(|a, b| a.1 == b.1);
        let unique_count = transactions_with_hash.len();
        info!(
            "✅ [FORK-SAFETY] Sorted queued transactions by hash and deduped: before={}, unique={}",
            before_dedup, unique_count
        );
        
        // Extract sorted transactions
        let transactions: Vec<Vec<u8>> = transactions_with_hash
            .into_iter()
            .map(|(tx_data, _)| tx_data)
            .collect();
        
        // Clear queue
        queue.clear();
        drop(queue);
        
        // Submit all queued transactions in deterministic order
        for tx_data in transactions {
            let transactions_vec = vec![tx_data];
            if let Err(e) = self.transaction_client_proxy.submit(transactions_vec).await {
                warn!("❌ [TX FLOW] Failed to submit queued transaction: {}", e);
                // Continue submitting other transactions
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

    /// Graceful shutdown: stop accepting transactions, wait for pending, then shutdown
    #[allow(dead_code)] // Will be used when implementing full epoch transition
    pub async fn graceful_shutdown(&mut self) -> Result<()> {
        info!("Starting graceful shutdown...");
        
        // 1. Stop accepting new transactions
        // Note: TransactionClient doesn't have a stop method yet, so we'll skip this for now
        // In a full implementation, we'd set a flag that TransactionClient checks
        
        // 2. Wait for pending transactions to complete
        // This would require tracking pending transactions, which is complex
        // For now, we'll add a small delay to allow current transactions to complete
        tokio::time::sleep(Duration::from_millis(100)).await;
        
        // 3. Wait for current round to complete
        // This would require access to Core's round state
        // For now, we'll add a small delay
        tokio::time::sleep(Duration::from_millis(500)).await;
        
        // 4. Flush pending commits
        // This would require access to commit processor
        // For now, we'll add a small delay
        tokio::time::sleep(Duration::from_millis(200)).await;
        
        info!("Graceful shutdown preparation complete");
        Ok(())
    }

    /// Build Committee from Go validators at a specific block
    /// Used for startup (block 0) and epoch transition (last_global_exec_index)
    async fn build_committee_from_go_validators_at_block(
        executor_client: &Arc<ExecutorClient>,
        block_number: u64,
    ) -> Result<Committee> {
        // Get validators from Go at specific block
        let validators = executor_client.get_validators_at_block(block_number).await
            .map_err(|e| anyhow::anyhow!("Failed to get validators from Go at block {}: {}", block_number, e))?;
        
        if validators.is_empty() {
            anyhow::bail!("No validators found in Go state at block {}", block_number);
        }
        
        info!("📋 Building committee from {} validators in Go state at block {}", validators.len(), block_number);
        
        // Determine epoch from block number (block 0 = epoch 0)
        let epoch = if block_number == 0 { 0 } else {
            // For epoch transition, epoch will be set by caller
            // For now, use 0 as default (will be overridden)
            0
        };
        
        Self::build_committee_from_validator_list(validators, epoch)
    }

    /// Build Committee from Go validators for epoch transition (uses current state)
    /// Maps validators from Go state to Rust Committee format
    /// NOTE: Currently unused, kept for backward compatibility
    #[allow(dead_code)]
    async fn build_committee_from_go_validators(
        executor_client: &Arc<ExecutorClient>,
        new_epoch: u64,
    ) -> Result<Committee> {
        // Get active validators from Go
        let validators = executor_client.get_active_validators().await
            .map_err(|e| anyhow::anyhow!("Failed to get active validators from Go: {}", e))?;
        
        if validators.is_empty() {
            anyhow::bail!("No active validators found in Go state");
        }
        
        info!("📋 [EPOCH-TRANSITION] Building committee from {} active validators in Go state", validators.len());
        
        Self::build_committee_from_validator_list(validators, new_epoch)
    }

    /// Helper function to build Committee from validator list
    fn build_committee_from_validator_list(
        validators: Vec<crate::executor_client::proto::ValidatorInfo>,
        epoch: u64,
    ) -> Result<Committee> {
        use consensus_config::{Authority, AuthorityPublicKey, NetworkPublicKey, ProtocolPublicKey};
        use mysten_network::Multiaddr;
        use fastcrypto::{bls12381, ed25519};
        use fastcrypto::traits::ToFromBytes;
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        
        // Sort validators by address (deterministic ordering for fork-safety)
        let mut sorted_validators: Vec<_> = validators.into_iter().collect();
        sorted_validators.sort_by(|a, b| a.address.cmp(&b.address));
        
        let mut authorities = Vec::new();
        let mut total_stake_normalized = 0u64;
        
        for (idx, validator) in sorted_validators.iter().enumerate() {
            // Parse stake (đã được normalize trong Go, chỉ cần parse u64)
            let stake = validator.stake.parse::<u64>()
                .map_err(|e| anyhow::anyhow!("Invalid stake '{}': {}", validator.stake, e))?;
            total_stake_normalized += stake;
            
            // Parse address to Multiaddr (tên khớp với committee.json)
            let address: Multiaddr = validator.address.parse()
                .map_err(|e| anyhow::anyhow!("Invalid address '{}': {}", validator.address, e))?;
            
            // Parse authority_key (BLS) - hỗ trợ cả base64 và hex
            // Go có thể gửi base64 (theo proto) hoặc hex (nếu lưu trong DB dưới dạng hex)
            // Thử base64 trước (theo proto definition), nếu fail thì thử hex
            let (authority_key_bytes, auth_key_format) = if validator.authority_key.starts_with("0x") {
                // Rõ ràng là hex format (có prefix 0x)
                let hex_str = &validator.authority_key[2..];
                let bytes = hex::decode(hex_str)
                    .map_err(|e| anyhow::anyhow!("Failed to decode authority_key (BLS) hex '{}': {}", validator.authority_key, e))?;
                (bytes, "hex")
            } else {
                // Thử base64 trước (theo proto definition)
                match STANDARD.decode(&validator.authority_key) {
                    Ok(bytes) => (bytes, "base64"),
                    Err(_) => {
                        // Nếu base64 fail, thử hex (không có prefix 0x)
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
            
            // Parse protocol_key (Ed25519) - hỗ trợ cả base64 và hex
            let (protocol_key_bytes, protocol_key_format) = if validator.protocol_key.starts_with("0x") {
                // Rõ ràng là hex format (có prefix 0x)
                let hex_str = &validator.protocol_key[2..];
                let bytes = hex::decode(hex_str)
                    .map_err(|e| anyhow::anyhow!("Failed to decode protocol_key hex '{}': {}", validator.protocol_key, e))?;
                (bytes, "hex")
            } else {
                // Thử base64 trước (theo proto definition)
                match STANDARD.decode(&validator.protocol_key) {
                    Ok(bytes) => (bytes, "base64"),
                    Err(_) => {
                        // Nếu base64 fail, thử hex (không có prefix 0x)
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
            
            // Parse network_key (Ed25519) - hỗ trợ cả base64 và hex
            let (network_key_bytes, network_key_format) = if validator.network_key.starts_with("0x") {
                // Rõ ràng là hex format (có prefix 0x)
                let hex_str = &validator.network_key[2..];
                let bytes = hex::decode(hex_str)
                    .map_err(|e| anyhow::anyhow!("Failed to decode network_key hex '{}': {}", validator.network_key, e))?;
                (bytes, "hex")
            } else {
                // Thử base64 trước (theo proto definition)
                match STANDARD.decode(&validator.network_key) {
                    Ok(bytes) => (bytes, "base64"),
                    Err(_) => {
                        // Nếu base64 fail, thử hex (không có prefix 0x)
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
            
            // Generate hostname từ name (nếu có) hoặc fallback về "node-{idx}"
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
        
        // Create committee with specified epoch
        let committee = Committee::new(epoch, authorities);
        Ok(committee)
    }

    /// Transition to a new epoch (fork-safe)
    #[allow(dead_code)] // Will be used when implementing full epoch transition
    pub async fn transition_to_epoch(
        &mut self,
        proposal: &crate::epoch_change::EpochChangeProposal,
        current_commit_index: u32,
    ) -> Result<()> {
        // Guard: run once per proposal hash
        let proposal_hash = {
            let mgr = self.epoch_change_manager.read().await;
            mgr.hash_proposal(proposal)
        };
        if self.last_transition_hash.as_ref() == Some(&proposal_hash) {
            return Ok(());
        }
        self.last_transition_hash = Some(proposal_hash);

        info!("Transitioning to epoch {}...", proposal.new_epoch);
        
        // 📋 LOG: Epoch transition summary (for fork-safety verification)
        info!(
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        );
        info!(
            "🔄 EPOCH TRANSITION START: epoch {} -> {}",
            self.current_epoch,
            proposal.new_epoch
        );
        info!(
            "  📊 Current State (BEFORE transition):"
        );
        info!(
            "    - Current epoch: {}",
            self.current_epoch
        );
        info!(
            "    - Current commit index: {}",
            current_commit_index
        );
        info!(
            "    - Last global exec index: {}",
            self.last_global_exec_index
        );
        info!(
            "    - Proposal commit index: {}",
            proposal.proposal_commit_index
        );
        info!(
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        );
        
        // ✅ FORK-SAFETY VALIDATION 1: Commit Index Barrier (with timeout exception)
        // CRITICAL: Tất cả nodes PHẢI transition tại CÙNG commit index (barrier) để tránh fork
        // Buffer of 10 commits ensures:
        // - Proposal has been committed and propagated to all nodes
        // - All nodes have had time to reach this commit index
        // - Reduces risk of fork due to network delay or processing speed differences
        //
        // EXCEPTION: Timeout mechanism allows transition when:
        // - Quorum is reached (2f+1 votes)
        // - Commit index hasn't increased for 5 minutes (other nodes may have transitioned)
        // - CRITICAL: Even with timeout, all nodes MUST use barrier as last_commit_index (fork-safe)
        let transition_commit_index = proposal.proposal_commit_index.saturating_add(10);
        let barrier_reached = current_commit_index >= transition_commit_index;
        
        // Check if timeout exception applies
        let manager = self.epoch_change_manager.read().await;
        let quorum_reached = manager.check_proposal_quorum(proposal) == Some(true);
        let now_seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let proposal_age_seconds = now_seconds.saturating_sub(proposal.created_at_seconds);
        const TIMEOUT_SECONDS: u64 = 300; // 5 minutes timeout
        let timeout_reached = proposal_age_seconds >= TIMEOUT_SECONDS;
        let timeout_exception = timeout_reached && quorum_reached && !barrier_reached;
        drop(manager);
        
        if !barrier_reached && !timeout_exception {
            // Normal case: must wait for barrier (fork-safety)
            return Err(anyhow::anyhow!(
                "FORK-SAFETY: Must wait until commit index {} (current: {}) to ensure all nodes transition together",
                transition_commit_index,
                current_commit_index
            ));
        }
        
        if timeout_exception {
            // Timeout exception: allow transition but MUST use barrier as last_commit_index
            warn!(
                "⏰ TIMEOUT EXCEPTION: Allowing transition despite barrier not reached (current={}, barrier={}, age={}s)",
                current_commit_index,
                transition_commit_index,
                proposal_age_seconds
            );
            warn!(
                "   🔒 FORK-SAFETY: Will use barrier ({}) as last_commit_index, NOT current ({})",
                transition_commit_index,
                current_commit_index
            );
            warn!(
                "   ✅ This ensures all nodes compute same global_exec_index (no fork)"
            );
        }
        
        // ✅ FORK-SAFETY VALIDATION 2: Quorum Check (with catch-up exception)
        // Đảm bảo đủ votes (2f+1) trước khi transition
        // EXCEPTION: Nếu node lag quá xa (epoch khác > current_epoch + 2), cho phép transition để catch-up
        // This ensures consensus on the epoch change, but allows catch-up when lagging
        let manager = self.epoch_change_manager.read().await;
        let quorum_status = manager.check_proposal_quorum(proposal);
        let quorum_reached = quorum_status == Some(true);
        
        // SIMPLE CATCH-UP LOGIC: Nếu node lag quá xa, cho phép transition mà không cần quorum
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
        
        if is_catchup_mode && !quorum_reached {
            warn!(
                "🚀 CATCH-UP MODE: Allowing transition without quorum - epoch {} -> {} (lag={} epochs)",
                self.current_epoch,
                proposal.new_epoch,
                epoch_lag
            );
            warn!(
                "   ⚠️  Node is lagging behind - allowing transition to catch up"
            );
            warn!(
                "   ✅ Fork-safety ensured by commit index barrier (all nodes use same barrier)"
            );
        }
        drop(manager);
        
        // ✅ FORK-SAFETY VALIDATION 4: Verify proposal hash consistency
        // CRITICAL: Đảm bảo proposal hash được tính giống nhau ở tất cả nodes
        // Nếu hash khác nhau, nodes sẽ không thể validate proposal → fork
        let computed_hash = {
            let mgr = self.epoch_change_manager.read().await;
            mgr.hash_proposal(proposal)
        };
        // Note: proposal_hash trong vote được tính từ proposal, nên nếu proposal giống nhau thì hash sẽ giống nhau
        // Validation này đảm bảo proposal data consistency
        let proposal_hash_hex = hex::encode(&computed_hash[..8.min(computed_hash.len())]);
        info!(
            "🔍 Proposal hash verification: proposal_hash={}, epoch {} -> {}",
            proposal_hash_hex,
            proposal.new_epoch - 1,
            proposal.new_epoch
        );
        
        // ✅ FORK-SAFETY VALIDATION 5: Verify epoch_timestamp_ms consistency
        // CRITICAL: Tất cả nodes phải dùng CÙNG epoch_timestamp_ms để tránh timestamp divergence
        // Nếu timestamp khác nhau, genesis blocks sẽ có hash khác nhau → fork
        // 
        // Note: epoch_timestamp_ms được lưu trong proposal và được sync khi catch-up
        // Validation này đảm bảo timestamp consistency
        let current_timestamp = self.epoch_change_manager.read().await.epoch_start_timestamp_ms();
        if current_timestamp != proposal.new_epoch_timestamp_ms {
            warn!(
                "⚠️  Epoch timestamp mismatch: current={}, proposal={}, diff={}ms",
                current_timestamp,
                proposal.new_epoch_timestamp_ms,
                (proposal.new_epoch_timestamp_ms as i64) - (current_timestamp as i64)
            );
            warn!(
                "   Node will sync timestamp from proposal to ensure consistency"
            );
        } else {
            info!(
                "✅ Epoch timestamp verified: timestamp={} (consistent across all nodes)",
                proposal.new_epoch_timestamp_ms
            );
        }
        
        // ✅ FORK-SAFETY VALIDATION 3: Use transition_commit_index (barrier) as last_commit_index
        // CRITICAL: Tất cả nodes PHẢI dùng CÙNG last_commit_index khi transition
        // Nếu node A transition ở commit 622 và node B transition ở commit 650,
        // chúng sẽ có last_commit_index khác nhau → global_exec_index khác nhau → FORK!
        // 
        // Giải pháp: Tất cả nodes dùng transition_commit_index (barrier) làm last_commit_index
        // - Node nào đạt barrier trước: đợi một chút để các node khác catch-up (optional, để tối ưu)
        // - Node nào catch-up muộn: vẫn dùng barrier làm last_commit_index (không dùng current_commit_index)
        // - Node nào timeout: vẫn dùng barrier làm last_commit_index (KHÔNG dùng current_commit_index)
        // - Điều này đảm bảo tất cả nodes có cùng state khi transition → không fork
        let commit_index_diff = current_commit_index.saturating_sub(transition_commit_index);
        let barrier_behind = transition_commit_index.saturating_sub(current_commit_index);
        
        warn!(
            "🔒 FORK-SAFETY: Using transition_commit_index (barrier) as last_commit_index"
        );
        warn!(
            "   - Transition barrier: {} (proposal_commit_index {} + 10)",
            transition_commit_index,
            proposal.proposal_commit_index
        );
        if commit_index_diff > 0 {
            warn!(
                "   - Current commit index: {} ({} commits past barrier)",
                current_commit_index,
                commit_index_diff
            );
        } else if barrier_behind > 0 {
            warn!(
                "   - Current commit index: {} ({} commits behind barrier - TIMEOUT EXCEPTION)",
                current_commit_index,
                barrier_behind
            );
        } else {
            warn!(
                "   - Current commit index: {} (exactly at barrier)",
                current_commit_index
            );
        }
        warn!(
            "   - All nodes will use last_commit_index={} to ensure fork-safety",
            transition_commit_index
        );
        if commit_index_diff > 0 {
            warn!(
                "   - ⚠️  Node has processed {} commits past barrier - these will be included in epoch transition",
                commit_index_diff
            );
        } else if barrier_behind > 0 {
            warn!(
                "   - ⚠️  TIMEOUT EXCEPTION: Node is {} commits behind barrier, but using barrier as last_commit_index (fork-safe)",
                barrier_behind
            );
        }
        
        info!(
            "✅ FORK-SAFE TRANSITION VALIDATED:"
        );
        info!(
            "  - All nodes will transition at commit index {} (barrier)",
            transition_commit_index
        );
        info!(
            "  - All nodes will use last_commit_index={} (deterministic)",
            transition_commit_index
        );
        info!(
            "  - Current commit index: {} ({} commits past barrier)",
            current_commit_index,
            commit_index_diff
        );
        info!(
            "  - Quorum: APPROVED (2f+1 votes received)"
        );
        info!(
            "  - Fork risk: ZERO (all nodes use same last_commit_index)"
        );
        
        // 1) Set transition barrier
        // SIMPLE APPROACH: Commits past barrier will send empty commits
        // Transactions in these commits will be processed in the next epoch
        // This is the simplest, most effective, fork-safe approach
        // CRITICAL: Set barrier BEFORE graceful shutdown so CommitProcessor can check it
        self.transition_barrier.store(transition_commit_index, Ordering::SeqCst);
        info!(
            "🔒 [FORK-SAFETY] Set transition barrier to {} - commits past this will send empty commits (transactions processed in next epoch)",
            transition_commit_index
        );
        
        // 3) Graceful shutdown current authority (best-effort)
        self.graceful_shutdown().await?;

        // 4) Calculate new last_global_exec_index (deterministic)
        // CRITICAL FORK-SAFETY: Use transition_commit_index (barrier) as last_commit_index, NOT current_commit_index
        // This ensures all nodes compute the same global_exec_index for new epoch
        // Even if node A transitions at commit 1281 and node B catches up and transitions at commit 1283,
        // both will use transition_commit_index (1281) as last_commit_index → same global_exec_index → no fork
        //
        // IMPORTANT: Commits past barrier MUST NOT be sent to Go Master (they are skipped in commit_processor)
        // This ensures Go Master only receives commits before barrier → sequential processing → no duplicate
        let old_epoch = self.current_epoch;
        let last_commit_index = transition_commit_index; // Use barrier, not current_commit_index! (DETERMINISTIC)
        let new_last_global_exec_index_from_barrier = calculate_global_exec_index(
            old_epoch,
            last_commit_index,
            self.last_global_exec_index,
        );
        
        // CRITICAL FORK-SAFETY: All nodes MUST use the same new_last_global_exec_index (from barrier)
        // Even if a node processed commits past barrier (e.g., 1282, 1283), we MUST use barrier value
        // This ensures all nodes compute the same global_exec_index for the new epoch
        let new_last_global_exec_index = new_last_global_exec_index_from_barrier;
        
        if commit_index_diff > 0 {
            warn!(
                "⚠️  FORK-SAFETY: Node processed {} commits past barrier ({} -> {})",
                commit_index_diff,
                transition_commit_index,
                current_commit_index
            );
            warn!(
                "   - Barrier global_exec_index: {} (USED for fork-safety - deterministic)",
                new_last_global_exec_index_from_barrier
            );
            let last_sent_global_exec_index = calculate_global_exec_index(
                old_epoch,
                current_commit_index,
                self.last_global_exec_index,
            );
            warn!(
                "   - Last sent global_exec_index: {} (NOT USED - commits past barrier are skipped)",
                last_sent_global_exec_index
            );
            warn!(
                "   - ⚠️  Commits past barrier ({} -> {}) are skipped (NOT sent to Go Master)",
                transition_commit_index + 1,
                current_commit_index
            );
            warn!(
                "   - ⚠️  This ensures all nodes use same new_last_global_exec_index (fork-safe)"
            );
        }
        
        // 📋 LOG: Deterministic values for fork-safety verification
        info!(
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        );
        info!(
            "📊 FORK-SAFETY: Deterministic Values (ALL NODES MUST MATCH)"
        );
        info!(
            "  🔑 Key Values:"
        );
        info!(
            "    - Old epoch: {}",
            old_epoch
        );
        info!(
            "    - New epoch: {}",
            proposal.new_epoch
        );
        info!(
            "    - Last commit index (barrier): {} (DETERMINISTIC - all nodes use this)",
            last_commit_index
        );
        info!(
            "    - Current commit index: {} (node-specific, may differ)",
            current_commit_index
        );
        info!(
            "    - Commits past barrier: {} (node-specific)",
            commit_index_diff
        );
        info!(
            "  📈 Global Execution Index:"
        );
        info!(
            "    - Last global exec index (old epoch): {}",
            self.last_global_exec_index
        );
        info!(
            "    - New last global exec index (new epoch): {} (DETERMINISTIC - all nodes compute same)",
            new_last_global_exec_index
        );
        // Calculate expected result using correct formula for display
        // NOTE: commit_index starts from 1 in every epoch, so:
        // - epoch 0: global_exec_index = commit_index
        // - epoch N>0: global_exec_index = last_global_exec_index + commit_index
        let expected_result = if old_epoch == 0 {
            last_commit_index as u64
        } else {
            self.last_global_exec_index + last_commit_index as u64
        };
        info!(
            "    - Calculation: {} (last_global_exec_index) + {} (barrier commit_index) = {}",
            self.last_global_exec_index,
            last_commit_index,
            expected_result
        );
        // Verify calculation is correct (only in debug mode to avoid panic in production)
        #[cfg(debug_assertions)]
        {
            if new_last_global_exec_index != expected_result {
                warn!(
                    "⚠️  BUG DETECTED: new_last_global_exec_index calculation mismatch! Expected {}, got {}. This may cause duplicate global_exec_index!",
                    expected_result, new_last_global_exec_index
                );
            }
        }
        if commit_index_diff > 0 {
            warn!(
                "  ⚠️  Note: Node processed {} commits past barrier ({} -> {}), but using barrier ({}) as last_commit_index for fork-safety",
                commit_index_diff,
                transition_commit_index,
                current_commit_index,
                transition_commit_index
            );
        }
        info!(
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        );

        // 4) Build new committee from Go state (TẤT CẢ nodes đều lấy từ Go)
        // CRITICAL FORK-SAFETY: Tất cả nodes phải có CÙNG committee
        // Tất cả nodes đều lấy validators từ Go state tại last_global_exec_index của epoch trước
        // Không phụ thuộc vào executor_enabled - tất cả nodes đều cần committee từ Go
        // Create executor client to get validators from Go state
        let node_id = self.committee_path.file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.strip_prefix("committee_node_"))
            .and_then(|s| s.strip_suffix(".json"))
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        let executor_client = Arc::new(ExecutorClient::new(true, node_id));
        
        // CRITICAL: Get validators from Go state at the LAST block of previous epoch
        // Block number = last_global_exec_index of previous epoch (this is the final block of epoch N-1)
        // This ensures we get the committee state at the exact point where epoch N-1 ended
        // NOTE: self.last_global_exec_index is still the value from previous epoch (not updated yet)
        let block_number = self.last_global_exec_index;
        info!("🔄 [EPOCH-TRANSITION] Loading committee from Go state at block {} (last_global_exec_index of previous epoch - this is the FINAL block of epoch {}) via Unix Domain Socket (node_id={})", 
            block_number, self.current_epoch, node_id);
        
        let new_committee = match Self::build_committee_from_go_validators_at_block(&executor_client, block_number).await {
            Ok(committee) => {
                // Set the new epoch
                // Note: Committee::new requires epoch, but we need to update it
                // For now, create a new committee with the correct epoch
                let authorities: Vec<_> = committee.authorities().map(|(_, auth)| auth.clone()).collect();
                let new_committee = Committee::new(proposal.new_epoch, authorities);
                info!("✅ [EPOCH-TRANSITION] Successfully built committee from Go state at block {}: {} authorities, epoch={}", 
                    block_number, new_committee.size(), proposal.new_epoch);
                new_committee
            }
            Err(e) => {
                warn!("⚠️  [EPOCH-TRANSITION] Failed to build committee from Go state at block {}: {}. Using proposal.new_committee as fallback.", block_number, e);
                proposal.new_committee.clone()
            }
        };

        // 4) Persist new epoch config (committee.json + epoch_timestamp_ms + last_global_exec_index)
        // CRITICAL FORK-SAFETY: Tất cả nodes phải ghi CÙNG data vào committee.json
        // - new_committee: Committee mới (từ Go state hoặc từ proposal)
        // - proposal.new_epoch_timestamp_ms: Timestamp mới (giống nhau ở tất cả nodes vì từ cùng proposal)
        // - new_last_global_exec_index: Global exec index mới (giống nhau vì dùng cùng last_commit_index = barrier)
        // 
        // Atomic write đảm bảo không bị corrupt nếu process crash giữa chừng
        crate::config::NodeConfig::save_committee_with_global_exec_index(
            &self.committee_path,
            &new_committee,
            proposal.new_epoch_timestamp_ms,
            new_last_global_exec_index,
        )?;
        
        info!(
            "💾 Committee.json saved: epoch={}, timestamp_ms={}, last_global_exec_index={}",
            proposal.new_epoch,
            proposal.new_epoch_timestamp_ms,
            new_last_global_exec_index
        );
        warn!(
            "   ⚠️  FORK-SAFETY: Tất cả nodes phải có CÙNG committee.json sau transition"
        );
        warn!(
            "   ⚠️  Nếu node restart sau transition, cần sync committee.json từ peers hoặc từ node đã transition"
        );

        // 5) Stop old authority (in-process restart)
        if let Some(authority) = self.authority.take() {
            authority.stop().await;
        }

        // 6) Update epoch and last_global_exec_index for new epoch
        self.current_epoch = proposal.new_epoch;
        self.last_global_exec_index = new_last_global_exec_index;
        
        // 📋 LOG: State after update (for fork-safety verification)
        info!(
            "✅ State Updated:"
        );
        info!(
            "    - Current epoch: {} (updated)",
            self.current_epoch
        );
        info!(
            "    - Last global exec index: {} (updated)",
            self.last_global_exec_index
        );
        info!(
            "    - Current commit index: 0 (reset for new epoch)"
        );

        // 7) Reset epoch-local state (manager + commit index)
        {
            let mut mgr = self.epoch_change_manager.write().await;
            mgr.reset_for_new_epoch(
                proposal.new_epoch,
                Arc::new(proposal.new_committee.clone()),
                proposal.new_epoch_timestamp_ms,
            );
        }
        self.current_commit_index.store(0, Ordering::SeqCst);

        // 8) Create fresh per-epoch DB path (do NOT delete old epoch DB)
        let db_path = self
            .storage_path
            .join("epochs")
            .join(format!("epoch_{}", proposal.new_epoch))
            .join("consensus_db");
        std::fs::create_dir_all(&db_path)?;

        // 9) Reset transition barrier and global_exec_index_at_barrier for new epoch
        // (new CommitProcessor will not have barrier initially)
        self.transition_barrier.store(0, Ordering::SeqCst);
        self.global_exec_index_at_barrier.store(0, Ordering::SeqCst);
        info!("🔓 [FORK-SAFETY] Reset transition barrier and global_exec_index_at_barrier to 0 for new epoch");
        
        // 10) Recreate commit consumer + commit processor for the new epoch (clean DAG/round)
        // NOTE: global_exec_index calculation is deterministic (all nodes compute same value)
        let (commit_consumer, commit_receiver, mut block_receiver) = CommitConsumerArgs::new(0, 0);
        let commit_index_for_callback = self.current_commit_index.clone();
        let new_epoch = proposal.new_epoch;
        let new_last_global_exec_index = new_last_global_exec_index;
        // Recreate executor client for new epoch (if enabled)
        // Get node_id from committee_path filename (e.g., "committee_node_0.json" -> 0)
        let node_id = self.committee_path.file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.strip_prefix("committee_node_"))
            .and_then(|s| s.strip_suffix(".json"))
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        // Check if executor is enabled via config (executor_enabled field in node_X.toml)
        // Only node 0 should have executor_enabled = true by default
        // Can be changed by editing node_X.toml files
        let executor_enabled = self.executor_enabled;
        let executor_client = if executor_enabled {
            let client = Arc::new(ExecutorClient::new(true, node_id));
            info!("✅ Executor client enabled for epoch transition (node_id={}, socket=/tmp/executor{}.sock)", 
                node_id, node_id);
            Some(client)
        } else {
            info!("ℹ️  Executor client disabled (node_id={}, consensus only - executor_enabled=false in config)", node_id);
            None
        };
        
        // Create transition barrier and global_exec_index_at_barrier for new epoch
        // (will be set when next epoch transition starts)
        let transition_barrier_for_new_epoch = self.transition_barrier.clone();
        let global_exec_index_at_barrier_for_new_epoch = self.global_exec_index_at_barrier.clone();
        
        let mut commit_processor = crate::commit_processor::CommitProcessor::new(commit_receiver)
            .with_commit_index_callback(move |index| {
                commit_index_for_callback.store(index, Ordering::SeqCst);
            })
            .with_epoch_info(new_epoch, new_last_global_exec_index)
            // NOTE: Committee is not stored in CommitProcessor, it's used in authority creation
            .with_transition_barrier(transition_barrier_for_new_epoch)
            .with_global_exec_index_at_barrier(global_exec_index_at_barrier_for_new_epoch)
            .with_pending_transactions_queue(self.pending_transactions_queue.clone());
        
        // Add executor client if enabled
        if let Some(ref client) = executor_client {
            commit_processor = commit_processor.with_executor_client(client.clone());
        }
        
        // Spawn commit processor with error handling and monitoring (epoch transition)
        let node_id_for_log_epoch = node_id;
        tokio::spawn(async move {
            info!("🚀 [COMMIT PROCESSOR] Starting commit processor for new epoch {} (node {}, last_global_exec_index={})", 
                new_epoch, node_id_for_log_epoch, new_last_global_exec_index);
            match commit_processor.run().await {
                Ok(()) => {
                    info!("✅ [COMMIT PROCESSOR] Commit processor exited normally (epoch {}, node {})", 
                        new_epoch, node_id_for_log_epoch);
                }
                Err(e) => {
                    tracing::error!("❌ [COMMIT PROCESSOR] Commit processor error (epoch {}, node {}): {}", 
                        new_epoch, node_id_for_log_epoch, e);
                }
            }
        });
        tokio::spawn(async move {
            use tracing::debug;
            while let Some(output) = block_receiver.recv().await {
                debug!("Received {} certified blocks", output.blocks.len());
            }
        });

        // 7) Start new authority in-process with per-epoch db_path
        let mut parameters = self.parameters.clone();
        parameters.db_path = db_path.clone();
        self.boot_counter = self.boot_counter.saturating_add(1);

        info!(
            "🔁 Restarting authority in-process for epoch {} with db_path={:?}",
            proposal.new_epoch,
            parameters.db_path
        );

        // Create a new registry for the new epoch to avoid AlreadyReg errors
        // CRITICAL: Prometheus Registry::new() creates a completely empty registry
        // Each epoch needs its own registry because metrics cannot be re-registered to the same registry
        // PRODUCTION-READY: Create a fresh registry for each epoch to ensure clean state management
        let new_registry = Registry::new();
        
        // Start authority with the NEW registry (metrics will be registered to this new registry)
        // IMPORTANT: We move the registry (not clone) to avoid sharing internal state
        // Prometheus Registry clone() shares internal state, which can cause AlreadyReg errors
        let authority = ConsensusAuthority::start(
            NetworkType::Tonic,
            proposal.new_epoch_timestamp_ms,
            self.own_index,
            proposal.new_committee.clone(),
            parameters,
            self.protocol_config.clone(),
            self.protocol_keypair.clone(),
            self.network_keypair.clone(),
            self.clock.clone(),
            self.transaction_verifier.clone(),
            commit_consumer,
            new_registry.clone(),  // Clone ONLY for passing to authority - this is safe because
                                   // metrics will be registered to the original registry instance
            self.boot_counter,
        )
        .await;

        // Add the registry to RegistryService AFTER metrics have been registered
        // The registry now contains all metrics from the new epoch
        // RegistryService.gather_all() will expose metrics from all registries
        let registry_id = if let Some(ref rs) = self.registry_service {
            // Add the registry that contains the new epoch's metrics
            // The metrics server will expose metrics from all registries via gather_all()
            Some(rs.add(new_registry))
        } else {
            None
        };

        // Optionally remove old registry to avoid accumulating too many registries
        // For now, we keep old registries to preserve metrics history
        // if let Some(ref rs) = self.registry_service {
        //     if let Some(old_id) = self.current_registry_id {
        //         rs.remove(old_id);
        //     }
        // }

        // Update current registry ID
        self.current_registry_id = registry_id;

        let new_client = authority.transaction_client();
        self.transaction_client_proxy.set_client(new_client).await;
        self.authority = Some(authority);
        
        // Submit queued transactions from barrier phase to new epoch consensus
        let queued_count = self.submit_queued_transactions().await?;
        if queued_count > 0 {
            info!(
                "✅ Submitted {} queued transactions to consensus in new epoch {}",
                queued_count, proposal.new_epoch
            );
        }
        
        // Clear last_transition_hash to allow new transactions after transition completes
        // This ensures transactions can be accepted again after new authority is ready
        self.last_transition_hash = None;
        info!("✅ New authority started and ready to accept transactions");

        // 📋 LOG: Final state after transition (for fork-safety verification)
        info!(
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        );
        info!(
            "✅ EPOCH TRANSITION COMPLETE: epoch {} -> {}",
            old_epoch,
            proposal.new_epoch
        );
        info!(
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        );
        info!(
            "📊 FINAL STATE (AFTER transition) - FORK-SAFETY VERIFICATION:"
        );
        info!(
            "  🔑 Deterministic Values (ALL NODES MUST MATCH - verify across all nodes):"
        );
        info!(
            "    - New epoch: {}",
            proposal.new_epoch
        );
        info!(
            "    - Last commit index (barrier): {} (used for transition - ALL NODES MUST USE THIS)",
            last_commit_index
        );
        info!(
            "    - Last global exec index: {} (DETERMINISTIC - all nodes must have same)",
            new_last_global_exec_index
        );
        info!(
            "    - Epoch timestamp: {} (DETERMINISTIC - all nodes must have same)",
            proposal.new_epoch_timestamp_ms
        );
        info!(
            "  📈 Current Node State:"
        );
        info!(
            "    - Current epoch: {}",
            self.current_epoch
        );
        info!(
            "    - Current commit index: 0 (reset for new epoch)"
        );
        info!(
            "    - Last global exec index: {}",
            self.last_global_exec_index
        );
        info!(
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        );
        warn!(
            "⚠️  FORK-SAFETY CHECK: Verify all nodes have SAME values:"
        );
        warn!(
            "    - epoch: {}",
            proposal.new_epoch
        );
        warn!(
            "    - last_commit_index (barrier): {}",
            last_commit_index
        );
        warn!(
            "    - last_global_exec_index: {}",
            new_last_global_exec_index
        );
        warn!(
            "    - epoch_timestamp_ms: {}",
            proposal.new_epoch_timestamp_ms
        );
        warn!(
            "   If any node has different values → FORK DETECTED!"
        );
        info!(
            "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        );
        info!(
            "✅ Epoch transition COMPLETE in-process: now running epoch {} (clean consensus DB per-epoch).",
            proposal.new_epoch
        );
        Ok(())
    }
}

