# System Transaction Epoch Transition

## Tổng quan

Giải pháp lai (hybrid) thay thế cơ chế Proposal/Vote/Quorum bằng **System Transactions** giống như Sui's EndOfPublish, nhưng vẫn giữ lại fork-safety mechanism của hệ thống hiện tại.

## Kiến trúc

### 1. SystemTransaction Type

System transactions là các transactions đặc biệt được tự động inject vào blocks bởi consensus layer, không phải từ user submissions.

```rust
pub enum SystemTransactionKind {
    EndOfEpoch {
        new_epoch: Epoch,
        new_epoch_timestamp_ms: u64,
        commit_index: u32,
    },
}
```

### 2. SystemTransactionProvider

Provider tự động tạo system transactions khi điều kiện được thỏa mãn (ví dụ: đủ thời gian epoch).

```rust
pub trait SystemTransactionProvider: Send + Sync {
    fn get_system_transactions(
        &self, 
        current_epoch: Epoch, 
        current_commit_index: u32
    ) -> Option<Vec<SystemTransaction>>;
}
```

### 3. Integration Flow

```
Time-based trigger → SystemTransactionProvider → Core injects into block → 
Commit → CommitProcessor detects → Epoch transition callback → 
Node performs epoch transition
```

## So sánh với Proposal/Vote/Quorum

| Aspect | Proposal/Vote/Quorum | System Transactions |
|--------|---------------------|---------------------|
| **Trigger** | Time-based proposal creation | Time-based system transaction |
| **Consensus** | Proposal + Votes + Quorum (2f+1) | System transaction trong committed block |
| **Fork-safety** | Commit index barrier | Commit index barrier (giữ nguyên) |
| **Complexity** | Cao (proposal/vote/quorum management) | Thấp (chỉ system transaction) |
| **Broadcast** | Cần broadcast proposals/votes | Tự động qua block propagation |
| **Quorum** | Cần 2f+1 votes | Không cần (system transaction đã được commit) |

## Ưu điểm

1. **Đơn giản hơn**: Không cần quản lý proposals/votes/quorum
2. **Tự động**: System transactions được inject tự động vào blocks
3. **Đồng bộ tốt**: Dựa trên commit finalization (giống Sui)
4. **Fork-safe**: Vẫn giữ commit index barrier mechanism
5. **Backward compatible**: Có thể chạy song song với Proposal/Vote/Quorum (nếu provider = None)

## Cách sử dụng

### 1. Tạo SystemTransactionProvider

**Với default buffer (100 commits)** - Khuyến nghị cho hầu hết trường hợp:

```rust
use consensus_core::DefaultSystemTransactionProvider;

let system_tx_provider = Arc::new(DefaultSystemTransactionProvider::new(
    current_epoch,
    epoch_duration_seconds,
    epoch_start_timestamp_ms,
    time_based_enabled,
));
```

**Với custom buffer** - Cho hệ thống có commit rate đặc biệt:

```rust
// Cho hệ thống commit rate thấp (<10 commits/s): buffer 10-20
let system_tx_provider = Arc::new(DefaultSystemTransactionProvider::new_with_buffer(
    current_epoch,
    epoch_duration_seconds,
    epoch_start_timestamp_ms,
    time_based_enabled,
    20, // buffer size
));

// Cho hệ thống commit rate rất cao (>200 commits/s): buffer 200+
let system_tx_provider = Arc::new(DefaultSystemTransactionProvider::new_with_buffer(
    current_epoch,
    epoch_duration_seconds,
    epoch_start_timestamp_ms,
    time_based_enabled,
    200, // buffer size
));
```

### 2. Pass vào Core

```rust
let core = Core::new(
    context,
    leader_schedule,
    transaction_consumer,
    transaction_certifier,
    block_manager,
    commit_observer,
    signals,
    block_signer,
    dag_state,
    sync_last_known_own_block,
    round_tracker,
    adaptive_delay_state,
    Some(system_tx_provider), // ← Pass provider here
);
```

### 3. Setup Epoch Transition Callback

```rust
let commit_processor = CommitProcessor::new(commit_receiver)
    .with_epoch_transition_callback(move |new_epoch, new_timestamp_ms, transition_commit_index| {
        // Trigger epoch transition
        node.perform_epoch_transition(new_epoch, new_timestamp_ms, transition_commit_index)
    });
```

### 4. Update Provider sau Epoch Transition

```rust
// Sau khi epoch transition hoàn tất
system_tx_provider.update_epoch(new_epoch, new_timestamp_ms).await;
```

## Fork-Safety

Fork-safety được đảm bảo bằng:

1. **Commit Index Barrier**: System transaction chứa `transition_commit_index = current_commit_index + buffer` (default: 100 commits)
   - **Buffer size**: Default 100 commits (increased from 10 for high commit rate safety)
   - **Rationale**: Với commit rate 200 commits/s, 100 commits = 500ms (an toàn hơn 10 commits = 50ms)
   - **Configurable**: Có thể config buffer size qua `new_with_buffer()` method
2. **Deterministic Transition**: Tất cả nodes transition tại cùng commit index
3. **Barrier Check**: CommitProcessor chỉ trigger transition khi `commit_index >= transition_commit_index`

## Migration từ Proposal/Vote/Quorum

Để migrate từ Proposal/Vote/Quorum sang System Transactions:

1. **Tạo SystemTransactionProvider** với cùng config (epoch duration, time-based enabled)
2. **Pass provider vào Core** (thay vì None)
3. **Setup epoch transition callback** trong CommitProcessor
4. **Disable Proposal/Vote/Quorum** bằng cách không set epoch_change_proposal/votes trong blocks
5. **Test thoroughly** với single node và multi-node setup

## Backward Compatibility

- Nếu `system_transaction_provider = None`, hệ thống sẽ không inject system transactions
- Proposal/Vote/Quorum mechanism vẫn hoạt động nếu không có provider
- Có thể chạy song song cả hai mechanisms (không khuyến nghị)

## Testing

### Single Node Test

```rust
// Tạo provider với epoch duration ngắn (ví dụ: 60 giây)
let provider = DefaultSystemTransactionProvider::new(
    0, // current_epoch
    60, // epoch_duration_seconds
    now_ms,
    true, // time_based_enabled
);

// Verify system transaction được inject sau 60 giây
// Verify epoch transition được trigger
```

### Multi-Node Test

```rust
// Tất cả nodes phải có cùng provider config
// Verify tất cả nodes transition tại cùng commit index
// Verify không có fork
```

## Troubleshooting

### System transaction không được inject

- Kiểm tra `time_based_enabled = true`
- Kiểm tra `epoch_duration_seconds` đã đủ
- Kiểm tra provider được pass vào Core

### Epoch transition không được trigger

- Kiểm tra callback được setup trong CommitProcessor
- Kiểm tra `commit_index >= transition_commit_index`
- Kiểm tra logs để xem system transaction có được detect không

### Fork xảy ra

- Kiểm tra tất cả nodes có cùng `transition_commit_index`
- Kiểm tra barrier mechanism hoạt động đúng
- Kiểm tra clock sync (NTP) nếu dùng time-based
- Kiểm tra commit rate: Nếu commit rate rất cao (>200 commits/s), có thể cần tăng buffer size
- Kiểm tra network delay: Nếu network delay cao, có thể cần tăng buffer size

## Future Enhancements

1. **Multiple System Transaction Types**: Thêm các loại system transactions khác (ví dụ: validator set changes)
2. **Configurable Barrier**: Cho phép config barrier offset (hiện tại cố định +10)
3. **Metrics**: Thêm metrics cho system transaction injection và epoch transitions
4. **Monitoring**: Dashboard để monitor epoch transitions từ system transactions
