// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use consensus_config::{AuthorityIndex, Committee};
use consensus_core::{
    ConsensusAuthority, NetworkType, Clock,
    CommitConsumerArgs, ReconfigState,
    DefaultSystemTransactionProvider, SystemTransactionProvider, // Added SystemTransactionProvider trait
};
// Removed unused imports ProtocolKeyPair, NetworkKeyPair
use crate::transaction::NoopTransactionVerifier;
use crate::clock_sync::ClockSyncManager;
use prometheus::Registry;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicBool, Ordering};
use std::time::Duration; // Removed SystemTime, UNIX_EPOCH
use meta_protocol_config::ProtocolConfig;
use tracing::{info, warn, error}; // Added error
use tokio::sync::RwLock;

use crate::config::NodeConfig;
use crate::tx_submitter::{TransactionClientProxy, TransactionSubmitter};
use crate::executor_client::ExecutorClient;
use consensus_core::storage::rocksdb_store::RocksDBStore;
use consensus_core::storage::Store; // Added Store trait

// Declare submodules
pub mod committee;
pub mod queue;
pub mod recovery;
pub mod sync;
pub mod transition;

/// Node operation modes
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum NodeMode {
    /// Node only syncs data, does not participate in voting
    SyncOnly,
    /// Node participates in consensus and voting
    Validator,
}

// Global registry for transition handler to access node
static TRANSITION_HANDLER_REGISTRY: tokio::sync::OnceCell<Arc<tokio::sync::Mutex<Option<Arc<tokio::sync::Mutex<ConsensusNode>>>>>> = tokio::sync::OnceCell::const_new();

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
        ));

        let latest_block_number = executor_client.get_last_block_number().await
            .map_err(|e| anyhow::anyhow!("Failed to fetch latest block from Go: {}", e))?;

        let current_epoch = executor_client.get_current_epoch().await
            .map_err(|e| anyhow::anyhow!("Failed to fetch current epoch from Go: {}", e))?;

        info!("📊 [STARTUP] State: Block {}, Epoch {}", latest_block_number, current_epoch);

        let (validators, _go_epoch_timestamp_ms) = executor_client.get_validators_at_block(latest_block_number).await
            .map_err(|e| anyhow::anyhow!("Failed to fetch committee from Go: {}", e))?;

        // Use epoch timestamp directly from Go state for all epochs
        let epoch_timestamp_ms = _go_epoch_timestamp_ms;

        if validators.is_empty() {
            anyhow::bail!("Go state returned empty validators list");
        }

        // Filter validators for single node debug if needed
        let validators_to_use = if std::env::var("SINGLE_NODE_DEBUG").is_ok() {
            info!("🔧 SINGLE_NODE_DEBUG: Using only node 0");
            validators.into_iter().filter(|v| v.name == "node-0").collect::<Vec<_>>()
        } else {
            validators
        };

        // Use helper from committee.rs
        let committee = committee::build_committee_from_validator_list(validators_to_use, current_epoch)?;
        info!("✅ Loaded committee with {} authorities", committee.size());

        let storage_path = config.storage_path.clone();
        
        let last_global_exec_index = if config.executor_read_enabled {
            match executor_client.get_last_block_number().await {
                Ok(go_last_block) => {
                    let db_path = config.storage_path
                        .join("epochs")
                        .join(format!("epoch_{}", current_epoch))
                        .join("consensus_db");

                    if db_path.exists() {
                        let temp_store = RocksDBStore::new(db_path.to_str().unwrap());
                        match temp_store.read_last_commit_info() {
                            Ok(Some((last_commit_ref, _))) => {
                                let storage_index = last_commit_ref.index as u64;
                                if storage_index < go_last_block && (go_last_block - storage_index) > 5 {
                                    warn!("🚨 [STARTUP] INDEX CONFLICT: Go {}, Storage {} - using storage", go_last_block, storage_index);
                                    storage_index
                                } else {
                                    go_last_block
                                }
                            },
                            _ => go_last_block
                        }
                    } else {
                        go_last_block
                    }
                },
                Err(e) => {
                    error!("🚨 [STARTUP] Failed to sync with Go: {}. Resetting to 0.", e);
                    0
                }
            }
        } else {
            0
        };

        let last_global_exec_index = if last_global_exec_index > 10000 {
            warn!("🚨 [STARTUP] last_global_exec_index={} too high - resetting to 0", last_global_exec_index);
            0
        } else {
            last_global_exec_index
        };

        // Recovery check
        if config.executor_read_enabled && last_global_exec_index > 0 {
            recovery::perform_block_recovery_check(
                &executor_client,
                last_global_exec_index,
                current_epoch,
                &config.storage_path,
                config.node_id as u32,
            ).await?;
        }

        let protocol_keypair = config.load_protocol_keypair()?;
        let network_keypair = config.load_network_keypair()?;

        let own_hostname = format!("node-{}", config.node_id);
        let own_index_opt = committee.authorities().find_map(|(idx, auth)| {
            if auth.hostname == own_hostname { Some(idx) } else { None }
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
        let persisted_queue = queue::load_transaction_queue_static(&storage_path).await.unwrap_or_default();
        if !persisted_queue.is_empty() {
            info!("💾 Loaded {} persisted transactions", persisted_queue.len());
        }
        let pending_transactions_queue = Arc::new(tokio::sync::Mutex::new(persisted_queue));
        
        let (epoch_tx_sender, epoch_tx_receiver) = tokio::sync::mpsc::unbounded_channel::<(u64, u64, u32)>();
        let epoch_transition_callback = crate::commit_callbacks::create_epoch_transition_callback(epoch_tx_sender.clone());
        
        let shared_last_global_exec_index = Arc::new(tokio::sync::Mutex::new(last_global_exec_index));

        let mut commit_processor = crate::commit_processor::CommitProcessor::new(commit_receiver)
            .with_commit_index_callback(crate::commit_callbacks::create_commit_index_callback(current_commit_index.clone()))
            .with_global_exec_index_callback(crate::commit_callbacks::create_global_exec_index_callback(shared_last_global_exec_index.clone()))
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
                initial_next_expected
            ))
        } else {
            Arc::new(ExecutorClient::new(false, false, "".to_string(), "".to_string()))
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
            let leader_timeout = config.leader_timeout_ms
                .map(Duration::from_millis)
                .unwrap_or_else(|| Duration::from_millis((200.0 / config.speed_multiplier) as u64));
            parameters.leader_timeout = leader_timeout;
        }

        let db_path = config.storage_path
            .join("epochs")
            .join(format!("epoch_{}", current_epoch))
            .join("consensus_db");

        if db_path.exists() {
            let _ = std::fs::remove_dir_all(&db_path);
        }
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
            Some(ConsensusAuthority::start(
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
            ).await)
        } else {
            info!("🔄 Starting as sync-only node");
            None
        };

        let transaction_client_proxy = if let Some(ref auth) = authority {
            Some(Arc::new(TransactionClientProxy::new(auth.transaction_client())))
        } else {
            None
        };

        // Initialize no-op epoch change handlers (required by core)
        use consensus_core::epoch_change_provider::{EpochChangeProvider, EpochChangeProcessor};
        struct NoOpProvider; impl EpochChangeProvider for NoOpProvider { fn get_proposal(&self) -> Option<Vec<u8>> { None } fn get_votes(&self) -> Vec<Vec<u8>> { Vec::new() } }
        struct NoOpProcessor; impl EpochChangeProcessor for NoOpProcessor { fn process_proposal(&self, _: &[u8]) {} fn process_vote(&self, _: &[u8]) {} }
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
        };

        crate::epoch_transition::start_epoch_transition_handler(
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
        self.transaction_client_proxy.as_ref().map(|proxy| proxy.clone() as Arc<dyn TransactionSubmitter>)
    }

    pub async fn check_transaction_acceptance(&self) -> (bool, bool, String) {
        if self.authority.is_none() {
            return (false, false, "Node is still initializing".to_string());
        }
        if self.last_transition_hash.is_some() {
            return (false, false, format!("Epoch transition in progress: epoch {} -> {}", self.current_epoch, self.current_epoch + 1));
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
        queue::queue_transaction(&self.pending_transactions_queue, &self.storage_path, tx_data).await
    }

    pub async fn submit_queued_transactions(&mut self) -> Result<usize> {
        queue::submit_queued_transactions(self).await
    }

    pub async fn shutdown(mut self) -> Result<()> {
        info!("Shutting down consensus node...");
        let _ = self.stop_sync_task().await;
        if let Some(authority) = self.authority {
            authority.stop().await;
        }
        info!("Consensus node stopped");
        Ok(())
    }

    // Delegated methods
    pub async fn check_and_update_node_mode(&mut self, committee: &Committee, config: &NodeConfig) -> Result<()> {
        let node_hostname = format!("node-{}", config.node_id);
        let should_be_validator = committee.authorities().any(|(_, authority)| authority.hostname == node_hostname);

        let new_mode = if should_be_validator { NodeMode::Validator } else { NodeMode::SyncOnly };

        if self.node_mode != new_mode {
            info!("🔄 [NODE MODE] Switching from {:?} to {:?}", self.node_mode, new_mode);
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

    pub async fn transition_to_epoch_from_system_tx(&mut self, new_epoch: u64, new_epoch_timestamp_ms: u64, commit_index: u32, config: &NodeConfig) -> Result<()> {
        transition::transition_to_epoch_from_system_tx(self, new_epoch, new_epoch_timestamp_ms, commit_index, config).await
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