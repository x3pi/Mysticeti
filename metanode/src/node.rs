// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Result, ensure};
use consensus_config::{AuthorityIndex, Committee};
use consensus_core::{
    ConsensusAuthority, NetworkType, Clock,
    CommitConsumerArgs,
};
use crate::transaction::NoopTransactionVerifier;
use crate::epoch_change::EpochChangeManager;
use crate::clock_sync::ClockSyncManager;
use prometheus::Registry;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use sui_protocol_config::ProtocolConfig;
use tracing::{info, warn};
use tokio::sync::RwLock;

use crate::config::NodeConfig;
use crate::tx_submitter::{TransactionClientProxy, TransactionSubmitter};

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
}

impl ConsensusNode {
    pub async fn new(config: NodeConfig) -> Result<Self> {
        info!("Initializing consensus node {}...", config.node_id);

        // Load committee
        let committee = config.load_committee()?;
        info!("Loaded committee with {} authorities", committee.size());

        // Capture paths needed for epoch transition
        let committee_path = config
            .committee_path
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Committee path not specified in config"))?;
        let storage_path = config.storage_path.clone();

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

        // Initialize metrics registry
        let registry = Registry::new();

        // Create clock (kept for in-process restart)
        let clock = Arc::new(Clock::default());

        // Create transaction verifier (no-op for now, kept for in-process restart)
        let transaction_verifier = Arc::new(NoopTransactionVerifier);

        // Create commit consumer args
        let (commit_consumer, commit_receiver, mut block_receiver) = CommitConsumerArgs::new(0, 0);
        
        // Track current commit index for fork-safe epoch transition
        let current_commit_index = Arc::new(AtomicU32::new(0));
        let commit_index_for_callback = current_commit_index.clone();
        
        // Create ordered commit processor with commit index tracking
        let commit_processor = crate::commit_processor::CommitProcessor::new(commit_receiver)
            .with_commit_index_callback(move |index| {
                commit_index_for_callback.store(index, Ordering::SeqCst);
            });
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
            registry,
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
            protocol_keypair,
            network_keypair,
            protocol_config,
            clock,
            transaction_verifier,
            parameters,
            own_index,
            boot_counter: 0,
            last_transition_hash: None,
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
        
        // ✅ FORK-SAFETY VALIDATION 1: Commit Index Barrier
        // Tất cả nodes sẽ transition tại cùng commit_index → fork-safe
        // Buffer of 10 commits ensures:
        // - Proposal has been committed and propagated to all nodes
        // - All nodes have had time to reach this commit index
        // - Reduces risk of fork due to network delay or processing speed differences
        let transition_commit_index = proposal.proposal_commit_index.saturating_add(10);
        ensure!(
            current_commit_index >= transition_commit_index,
            "FORK-SAFETY: Must wait until commit index {} (current: {}) to ensure all nodes transition together",
            transition_commit_index,
            current_commit_index
        );
        
        // ✅ FORK-SAFETY VALIDATION 2: Quorum Check
        // Đảm bảo đủ votes (2f+1) trước khi transition
        // This ensures consensus on the epoch change
        let manager = self.epoch_change_manager.read().await;
        ensure!(
            manager.check_proposal_quorum(proposal) == Some(true),
            "FORK-SAFETY: Quorum not reached for epoch transition - need 2f+1 votes"
        );
        drop(manager);
        
        let commit_index_diff = current_commit_index.saturating_sub(transition_commit_index);
        info!(
            "✅ FORK-SAFE TRANSITION VALIDATED:"
        );
        info!(
            "  - All nodes will transition at commit index ~{} (within buffer range)",
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
            "  - Fork risk: MINIMAL (deterministic transition point)"
        );
        
        // 1) Graceful shutdown current authority (best-effort)
        self.graceful_shutdown().await?;

        // 2) Persist new epoch config (committee.json + epoch_timestamp_ms)
        crate::config::NodeConfig::save_committee_with_epoch_timestamp(
            &self.committee_path,
            &proposal.new_committee,
            proposal.new_epoch_timestamp_ms,
        )?;

        // 3) Stop old authority (in-process restart)
        if let Some(authority) = self.authority.take() {
            authority.stop().await;
        }

        // 4) Reset epoch-local state (manager + commit index)
        {
            let mut mgr = self.epoch_change_manager.write().await;
            mgr.reset_for_new_epoch(
                proposal.new_epoch,
                Arc::new(proposal.new_committee.clone()),
                proposal.new_epoch_timestamp_ms,
            );
        }
        self.current_commit_index.store(0, Ordering::SeqCst);

        // 5) Create fresh per-epoch DB path (do NOT delete old epoch DB)
        let db_path = self
            .storage_path
            .join("epochs")
            .join(format!("epoch_{}", proposal.new_epoch))
            .join("consensus_db");
        std::fs::create_dir_all(&db_path)?;

        // 6) Recreate commit consumer + commit processor for the new epoch (clean DAG/round)
        let (commit_consumer, commit_receiver, mut block_receiver) = CommitConsumerArgs::new(0, 0);
        let commit_index_for_callback = self.current_commit_index.clone();
        let commit_processor = crate::commit_processor::CommitProcessor::new(commit_receiver)
            .with_commit_index_callback(move |index| {
                commit_index_for_callback.store(index, Ordering::SeqCst);
            });
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

        // 7) Start new authority in-process with per-epoch db_path
        let mut parameters = self.parameters.clone();
        parameters.db_path = db_path.clone();
        self.boot_counter = self.boot_counter.saturating_add(1);

        info!(
            "🔁 Restarting authority in-process for epoch {} with db_path={:?}",
            proposal.new_epoch,
            parameters.db_path
        );

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
            Registry::new(),
            self.boot_counter,
        )
        .await;

        let new_client = authority.transaction_client();
        self.transaction_client_proxy.set_client(new_client).await;
        self.authority = Some(authority);

        info!(
            "✅ Epoch transition COMPLETE in-process: now running epoch {} (clean consensus DB per-epoch).",
            proposal.new_epoch
        );
        Ok(())
    }
}

