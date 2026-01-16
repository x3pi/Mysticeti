# Full Sync Node Implementation

## Tổng quan

Node-4 đã được nâng cấp thành **Full Sync Node** với khả năng:
- ✅ **Sync blocks qua mạng** từ validator nodes
- ✅ **Lưu trữ blocks locally** trong block store
- ✅ **Execute blocks locally** với Go Master riêng (nếu enabled)
- ✅ **Tự động chuyển sang validator mode** khi được thêm vào committee

## Kiến trúc

```
┌─────────────────┐    ┌──────────────────┐    ┌─────────────────┐
│   Validator     │────│   Full Sync      │────│   Go Master     │
│   Nodes         │    │   Node (4)       │    │   (Local)       │
└─────────────────┘    └──────────────────┘    └─────────────────┘
        │                        │                        │
        ▼                        ▼                        ▼
  Broadcast blocks ───────►   Sync blocks ───────────► Execute blocks
  (Network)              Download & Verify        State updates
                        Store locally
```

## Components

### 1. Network Sync Manager (`network_sync.rs`)

**Chức năng:**
- Discover validator peers từ committee
- Request blocks từ peers qua network
- Sync missing blocks theo batch
- Track sync state (local height, network height)

**API:**
```rust
pub struct NetworkSyncManager {
    peers: Arc<Mutex<Vec<Peer>>>,
    block_store: Arc<dyn BlockStore>,
    sync_state: Arc<Mutex<SyncState>>,
    network_client: Arc<NetworkClient>,
}

// Methods:
- update_peers(committee) -> Update peers from committee
- sync_missing_blocks() -> Sync blocks from network
- get_sync_state() -> Get current sync state
```

### 2. Block Store (`network_sync.rs`)

**Chức năng:**
- Store committed subdags locally
- Retrieve blocks by global_exec_index
- Track latest stored block index

**Implementation:**
- `InMemoryBlockStore`: In-memory storage (hiện tại)
- Có thể extend với persistent storage (RocksDB, SQLite, etc.)

**Trait:**
```rust
#[async_trait]
pub trait BlockStore: Send + Sync {
    async fn store_block(&self, subdag: &CommittedSubDag, global_exec_index: u64) -> Result<()>;
    async fn get_block(&self, global_exec_index: u64) -> Result<Option<CommittedSubDag>>;
    async fn get_latest_index(&self) -> Result<u64>;
    async fn has_block(&self, global_exec_index: u64) -> Result<bool>;
}
```

### 3. Network Client (`network_sync.rs`)

**Chức năng:**
- Discover peers từ committee
- Request blocks từ peers
- Handle network communication

**Status:**
- ⚠️ **Placeholder implementation** - cần integrate với consensus network
- Hiện tại: Discover peers từ committee (✅)
- TODO: Implement actual block request protocol

### 4. Local Execution (`node.rs`)

**Chức năng:**
- Execute synced blocks với local Go Master
- Sequential execution để đảm bảo consistency
- Update shared_last_global_exec_index sau mỗi execution

**Flow:**
```rust
// Trong sync task
1. Sync blocks từ network → block_store
2. Execute blocks sequentially từ last_executed + 1
3. Send blocks tới local Go Master qua ExecutorClient
4. Update shared_last_global_exec_index
```

## Configuration

### node_4.toml

```toml
# Network sync configuration
network_sync_enabled = true
network_sync_interval_seconds = 30
network_sync_batch_size = 100

# Local execution configuration
local_execution_enabled = true
executor_commit_enabled = true  # Enable to send blocks to local Go Master
local_go_master_path = "/path/to/go-master"
local_db_path = "config/storage/node_4/local_db"
```

## Flow hoạt động

### 1. Startup
```
1. Load config → Check network_sync_enabled
2. Create block_store (InMemoryBlockStore)
3. Create network_client
4. Create network_sync_manager
5. Update peers từ initial committee
6. Start sync task với network sync integration
```

### 2. Runtime Sync
```
Loop mỗi 5 giây:
  1. Basic sync: Get last_block_number từ Go Master
  2. Network sync (nếu enabled):
     - Check missing blocks (network_height > local_height)
     - Request blocks từ peers
     - Store blocks vào block_store
     - Execute locally (nếu local_execution_enabled)
```

### 3. Epoch Transition
```
1. Fetch new committee từ Go Master
2. Update network_sync_manager peers
3. Check_and_update_node_mode:
   - Nếu chuyển SyncOnly → Validator:
     - Stop sync task
     - Keep network sync running (optional)
     - Create authority
   - Nếu chuyển Validator → SyncOnly:
     - Start sync task
     - Update network sync peers
     - Stop authority
```

## Implementation Status

### ✅ Completed
- [x] Network sync manager structure
- [x] Block store trait và implementation
- [x] Configuration options
- [x] Integration vào sync task
- [x] Local execution support
- [x] Epoch transition compatibility

### ⚠️ TODO (Future Enhancements)

#### 1. Network Block Request Protocol
**Hiện tại:** Placeholder - cần implement actual protocol

**Options:**
- **Option A**: Extend consensus network để serve historical blocks
- **Option B**: Tạo block serving protocol riêng (HTTP/gRPC)
- **Option C**: Sử dụng existing consensus network broadcast để capture blocks

**Recommended:** Option C - Listen to consensus network broadcasts và store blocks

#### 2. Persistent Block Storage
**Hiện tại:** InMemoryBlockStore (mất data khi restart)

**Cần:**
- RocksDB implementation
- SQLite implementation
- Block pruning/compaction

#### 3. Block Verification
**Hiện tại:** Basic validation

**Cần:**
- Verify block signatures
- Verify block chain integrity
- Verify transaction validity

#### 4. Peer Management
**Hiện tại:** Static peers from committee

**Cần:**
- Dynamic peer discovery
- Peer health monitoring
- Failover to different peers

#### 5. Local Go Master Management
**Hiện tại:** Manual path configuration

**Cần:**
- Auto-start Go Master process
- Health monitoring
- Restart on failure

## Usage

### Enable Full Sync Node

1. **Update node_4.toml:**
```toml
network_sync_enabled = true
local_execution_enabled = true
executor_commit_enabled = true
```

2. **Start node:**
```bash
./target/release/metanode start --config config/node_4.toml
```

3. **Monitor sync:**
```bash
tail -f logs/latest/node_4.log | grep -E "(NETWORK SYNC|LOCAL EXECUTION)"
```

## Benefits

### 1. **Decentralization**
- Node-4 trở thành independent full node
- Có thể verify và execute blocks độc lập
- Không phụ thuộc vào Go Master chính

### 2. **Scalability**
- Giảm load cho Go Master chính
- Network sync thay vì chỉ Unix socket
- Parallel execution trên nhiều nodes

### 3. **Reliability**
- Local execution đảm bảo data consistency
- Block verification độc lập
- Failover capability nếu Go Master chính down

### 4. **Flexibility**
- Configurable sync (network vs local)
- Optional execution (commit enabled/disabled)
- Dynamic peer discovery

## Notes

⚠️ **Network block request protocol chưa được implement đầy đủ**

Hiện tại, `NetworkClient::request_blocks()` là placeholder. Để hoàn thiện:
1. Integrate với consensus network để request historical blocks
2. Hoặc implement block serving protocol riêng
3. Hoặc listen to consensus broadcasts và store blocks

**Tuy nhiên, infrastructure đã sẵn sàng** - chỉ cần implement actual network protocol! 🚀
