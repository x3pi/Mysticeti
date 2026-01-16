# Sync Rust State with Go Execution State on Startup

## Vấn đề (Problem)

Trước đây, khi hệ thống khởi động lại:
1. **Rust load state từ RocksDB storage** - có thể chứa các commits (blocks) mà Go chưa xử lý xong
2. **CommitObserver recover và resend các commits** từ Rust storage
3. **Vấn đề**: Rust có thể đã tạo và lưu commits mà Go chưa execute xong trước khi tắt máy
   - Rust tạo block → lưu vào RocksDB → gửi đến Go
   - Nếu tắt máy lúc này, Go có thể chưa execute xong block
   - Khi restart, Rust sẽ load và resend block đó, gây duplicate hoặc inconsistency

## Giải pháp (Solution)

Sửa code để khi khởi động, hệ thống sẽ:
1. **Query Go Master** để lấy `last_block_number` (block cuối cùng mà Go đã execute xong)
2. **Sử dụng giá trị này làm `replay_after_commit_index`** khi tạo `CommitConsumer`
3. **Skip các commits trong Rust storage** có commit_index <= `last_global_exec_index`
4. **Chỉ replay commits** có commit_index > `last_global_exec_index`

Điều này đảm bảo Rust sẽ resume từ đúng trạng thái mà Go đã execute, không phải từ trạng thái được lưu trong Rust storage.

## Các thay đổi code

### 1. Startup Flow (`metanode/src/node.rs`)

**Trước đây:**
```rust
// Tạo commit_consumer TRƯỚC khi query Go
let (commit_consumer, commit_receiver, mut block_receiver) = CommitConsumerArgs::new(0, 0);

// Sau đó mới query Go để lấy last_block_number
let last_global_exec_index = executor_client.get_last_block_number().await?;
```

**Bây giờ:**
```rust
// Query Go TRƯỚC để lấy last_block_number
let last_global_exec_index = if config.executor_read_enabled {
    match executor_client.get_last_block_number().await {
        Ok(last_block_number) => {
            info!("📊 [STARTUP] Synced last_global_exec_index={} from Go state (last executed block)", last_block_number);
            info!("✅ [STARTUP] System will resume from block {} (Go's last executed block), NOT from Rust storage", last_block_number);
            last_block_number
        },
        Err(e) => {
            error!("🚨 [STARTUP] CRITICAL: Failed to sync with Go: {}. Resetting to 0.", e);
            0
        }
    }
} else {
    0
};

// Sau đó tạo commit_consumer với replay_after_commit_index = last_global_exec_index
info!("🔧 [STARTUP] Creating commit consumer with replay_after_commit_index={} (Go's last executed block)", last_global_exec_index);
let (commit_consumer, commit_receiver, mut block_receiver) = CommitConsumerArgs::new(
    last_global_exec_index as u32, // replay_after_commit_index
    last_global_exec_index as u32, // consumer_last_processed_commit_index
);
info!("✅ [STARTUP] Commit consumer created - will skip Rust storage commits <= {}, only replay commits > {}", 
    last_global_exec_index, last_global_exec_index);
```

### 2. Epoch Transition Flow (`metanode/src/node.rs`)

Tương tự, khi chuyển epoch:
```rust
// Query Go để lấy last_block_number của epoch trước
let synced_last_global_exec_index = if self.executor_read_enabled {
    // ... query Go ...
} else {
    new_last_global_exec_index
};

// Tạo commit_consumer cho epoch mới với replay_after = synced value
info!("🔧 [EPOCH TRANSITION] Creating commit consumer with replay_after_commit_index={}", synced_last_global_exec_index);
let (commit_consumer, commit_receiver, mut block_receiver) = CommitConsumerArgs::new(
    synced_last_global_exec_index as u32,
    synced_last_global_exec_index as u32,
);
```

## Luồng hoạt động (Flow)

### Khởi động hệ thống:

1. **Load committee từ Go** (block 0/genesis)
2. **Query Go Master**: `get_last_block_number()` → `last_global_exec_index`
3. **Tạo CommitConsumer**: với `replay_after_commit_index = last_global_exec_index`
4. **CommitObserver recover commits**:
   - Đọc từ RocksDB storage
   - Chỉ replay commits có `commit_index > last_global_exec_index`
   - Skip commits có `commit_index <= last_global_exec_index` (đã được Go execute)
5. **Consensus bắt đầu**: tạo blocks mới từ `last_global_exec_index + 1`

### Ví dụ:

**Scenario**: Rust tạo blocks 1-10, Go chỉ execute xong đến block 7, sau đó tắt máy

**Trước đây:**
- Restart → Rust load từ storage → có blocks 1-10
- Resend tất cả blocks 1-10 cho Go
- Go receive duplicate blocks 1-7 → có thể gây lỗi

**Bây giờ:**
- Restart → Query Go → `last_block_number = 7`
- Tạo CommitConsumer với `replay_after_commit_index = 7`
- CommitObserver chỉ replay blocks 8-10 (skip blocks 1-7)
- Go receive blocks 8-10 → không duplicate, không lỗi

## Lợi ích (Benefits)

1. **Consistency**: Rust và Go luôn sync về trạng thái execution
2. **No Duplicates**: Không gửi lại blocks mà Go đã execute
3. **Reliable Recovery**: Hệ thống luôn recover từ trạng thái chính xác
4. **Epoch Transitions**: Đảm bảo consistency khi chuyển epoch

## Testing

### Test Manual:

1. Start hệ thống, gửi transactions
2. Chờ Rust tạo một số blocks
3. Tắt hệ thống khi Go đang execute (chưa xong)
4. Restart hệ thống
5. Verify logs:
   ```
   📊 [STARTUP] Synced last_global_exec_index=X from Go state
   ✅ [STARTUP] System will resume from block X (Go's last executed block)
   🔧 [STARTUP] Creating commit consumer with replay_after_commit_index=X
   ✅ [STARTUP] Commit consumer created - will skip commits <= X
   ```
6. Verify không có duplicate blocks được gửi đến Go

### Logs để theo dõi:

- `[STARTUP]` - startup flow
- `[EPOCH TRANSITION]` - epoch transition flow
- `[EXECUTOR-REQ]` - communication với Go Master
- `[SEQUENTIAL-BUFFER]` - block buffering and sending

## Files Modified

1. `/home/abc/chain-n/Mysticeti/metanode/src/node.rs`
   - Line ~358-392: Startup flow - query Go before creating commit_consumer
   - Line ~1750-1760: Epoch transition flow - sync with Go before creating new commit_consumer

## Related Components

- `executor_client.rs`: `get_last_block_number()` - query Go Master
- `commit_observer.rs`: `recover_and_send_commits()` - replay commits from storage
- `commit_consumer.rs`: `CommitConsumerArgs::new()` - set replay_after_commit_index
- `dag_state.rs`: Load state from RocksDB storage

## Notes

- Requires `executor_read_enabled = true` in config
- If Go query fails, fallback to 0 (genesis) to prevent conflicts
- Compatible with both single-epoch and multi-epoch scenarios
