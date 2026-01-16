# Full Sync Node - Luồng Đồng Bộ Mới

## Tổng quan

Full sync node (node-4) sử dụng **luồng đồng bộ mới** để:
1. **Đồng bộ độc lập full data blocks** từ validators qua network
2. **Gửi blocks đến Go Master** để thực thi nếu `executor_commit_enabled = true`
3. **Sẵn sàng tham gia đồng thuận** khi có trong danh sách committee khi chuyển đổi epoch

## Thay đổi chính

### ❌ Bỏ luồng đồng bộ cũ
- **REMOVED**: `perform_sync_operation` - chỉ lấy metadata (block numbers) từ Go Master
- **REMOVED**: Sync metadata từ Go executor
- **REMOVED**: Phụ thuộc vào Go Master để biết block numbers

### ✅ Luồng đồng bộ mới

#### 1. Chỉ sync từ network (validators)
- Full sync node **chỉ** sync full blocks từ validators qua `NetworkSyncManager`
- Không còn sync metadata từ Go Master
- Blocks được lưu vào `block_store` (InMemoryBlockStore)

#### 2. Gửi blocks đến Go Master (nếu enabled)
- Nếu `executor_commit_enabled = true`: Blocks được gửi đến Go Master để thực thi
- Nếu `executor_commit_enabled = false`: Blocks chỉ được lưu locally, không gửi đến Go Master

#### 3. Chuyển đổi mode tự động
- Khi có trong committee: Tự động chuyển từ `SyncOnly` sang `Validator` mode
- Khi không có trong committee: Tự động chuyển từ `Validator` sang `SyncOnly` mode
- Sync task được quản lý tự động (stop khi chuyển sang validator, start khi chuyển sang sync-only)

## Cấu hình

### Full Sync Node (node-4)

```toml
# Node mode
initial_node_mode = "SyncOnly"

# Network sync - BẮT BUỘC phải bật
network_sync_enabled = true
network_sync_interval_seconds = 30
network_sync_batch_size = 100

# Local execution
local_execution_enabled = true
executor_commit_enabled = true  # Enable để gửi blocks đến Go Master

# Executor paths
executor_send_socket_path = "/tmp/executor4.sock"
executor_receive_socket_path = "/tmp/rust-go.sock_1"
```

## Luồng hoạt động

### 1. Khởi động Full Sync Node

```
1. Node khởi động với initial_node_mode = "SyncOnly"
2. Kiểm tra committee membership
3. Nếu không có trong committee:
   - Khởi tạo NetworkSyncManager
   - Khởi tạo BlockStore
   - Start full sync task
4. Full sync task bắt đầu sync blocks từ validators
```

### 2. Sync Blocks từ Validators

```
1. NetworkSyncManager discover peers từ committee
2. Query network height từ peers
3. So sánh với local height
4. Request missing blocks từ peers
5. Store blocks vào BlockStore
6. Log số transactions nhận được
```

### 3. Gửi Blocks đến Go Master (nếu enabled)

```
1. Sau khi sync blocks từ network
2. Nếu executor_commit_enabled = true:
   - Lấy blocks từ BlockStore
   - Gửi đến Go Master qua ExecutorClient
   - Go Master thực thi blocks
3. Nếu executor_commit_enabled = false:
   - Blocks chỉ được lưu locally
   - Không gửi đến Go Master
```

### 4. Epoch Transition

```
1. EndOfEpoch transaction được detect
2. Fetch new committee từ Go state
3. check_and_update_node_mode():
   - Nếu node có trong new committee:
     * Chuyển node_mode từ SyncOnly → Validator
     * Stop sync task
     * Tạo ConsensusAuthority
     * Node tham gia consensus
   - Nếu node không có trong new committee:
     * Chuyển node_mode từ Validator → SyncOnly
     * Stop ConsensusAuthority
     * Start sync task
     * Tiếp tục sync từ network
4. Update network sync peers với new committee
```

## Logging

### Full Sync Task Logs

```
🚀 [FULL SYNC NODE] Node-4 is in sync-only mode - Initializing as FULL SYNC NODE
📋 [FULL SYNC NODE] NEW FLOW: Will sync FULL BLOCKS with transactions from validators via network
✅ [FULL SYNC NODE] Discovered 4 validator peers from committee
✅ [FULL SYNC NODE] Network sync manager initialized
✅ [FULL SYNC NODE] Executor commit enabled - synced blocks will be sent to Go Master for execution
✅ [FULL SYNC NODE] Full sync node initialized successfully
```

### Sync Cycle Logs

```
✅ [FULL SYNC] Synced 5 blocks from validators (cycle: 1)
📊 [FULL SYNC] Received 150 transactions in 5 blocks - Ready for execution!
📈 [FULL SYNC SUMMARY] Total received since startup: 150 transactions in 5 blocks
✅ [FULL SYNC] Successfully sent synced blocks to Go Master for execution
```

### Epoch Transition Logs

```
🔄 [NODE MODE] Switching from SyncOnly to Validator (hostname: node-4, in_committee: true)
🛑 [FULL SYNC] Stopping sync task...
✅ [NODE MODE] Successfully switched to validator mode
🚀 [AUTHORITY] Starting consensus authority for epoch 2 (node mode: Validator)
```

## Lợi ích

1. **Độc lập**: Full sync node không phụ thuộc vào Go Master để biết block numbers
2. **Hiệu quả**: Sync trực tiếp full blocks từ validators, không cần metadata sync
3. **Linh hoạt**: Có thể gửi blocks đến Go Master hoặc chỉ lưu locally
4. **Tự động**: Tự động chuyển đổi giữa sync-only và validator mode
5. **Sẵn sàng**: Luôn sẵn sàng tham gia consensus khi có trong committee

## So sánh với luồng cũ

| Tính năng | Luồng cũ | Luồng mới |
|-----------|----------|-----------|
| Metadata sync từ Go | ✅ Có | ❌ Không |
| Full blocks từ network | ⚠️ Chưa implement | ✅ Có |
| Gửi blocks đến Go Master | ❌ Không | ✅ Có (nếu enabled) |
| Độc lập | ❌ Phụ thuộc Go | ✅ Độc lập |
| Chuyển đổi mode | ✅ Có | ✅ Có |

## Yêu cầu

- `network_sync_enabled = true` (bắt buộc)
- Validators phải có block serving capability (đang triển khai)
- Network connectivity đến validators
