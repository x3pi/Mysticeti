# Sui Epoch Transition Integration for MetaNode

## Tổng quan

MetaNode đã được tích hợp cơ chế epoch transition theo pattern của Sui, trong khi vẫn giữ nguyên luồng xử lý giao dịch hiện tại. Việc tích hợp này bao gồm:

1. **Trigger mechanism**: Thay thế time-based trigger bằng state-based trigger (Sui-style)
2. **Execution Lock**: Tích hợp execution lock để đảm bảo không có transaction nào đang chạy trong quá trình reconfiguration
3. **Reconfiguration Flow**: Tối ưu quy trình reconfiguration theo pattern Sui

## Kiến trúc

### Components mới

#### 1. SuiEpochTransitionManager
- **Mục đích**: Quản lý việc trigger epoch transition theo cách Sui
- **Chức năng chính**:
  - Monitor Go state để detect epoch advancement
  - Tạo epoch change proposals khi cần thiết
  - Tích hợp với MetaNode's voting mechanism

#### 2. SuiReconfigurationManager
- **Mục đích**: Quản lý execution lock trong quá trình reconfiguration
- **Chức năng chính**:
  - Acquire/release execution lock
  - Đảm bảo không có transaction nào chạy trong reconfiguration

#### 3. SuiEpochTransitionOrchestrator
- **Mục đích**: Điều phối toàn bộ quá trình epoch transition
- **Chức năng chính**:
  - Khởi động monitoring tasks
  - Handle epoch transition với Sui-style reconfiguration

## Luồng hoạt động

### 1. Monitoring Phase (Sui-style)
```
Go State Epoch > Consensus Epoch
    ↓
SuiEpochTransitionManager.monitor_epoch_transitions()
    ↓
create_epoch_change_proposal()
    ↓
EpochChangeManager.propose_epoch_change_to_target_epoch()
```

### 2. Voting Phase (MetaNode preserved)
```
Proposal created
    ↓
Auto-vote by proposer
    ↓
Wait for quorum (2f+1 votes)
    ↓
Transition ready when quorum reached + commit index barrier passed
```

### 3. Transition Phase (Hybrid Sui + MetaNode)
```
transition_to_epoch() called
    ↓
SuiEpochTransitionOrchestrator.handle_epoch_transition()
    ↓
Acquire Execution Lock (Sui pattern)
    ↓
Reconfigure caches and state (Sui pattern)
    ↓
Restart authority with new committee (MetaNode pattern)
```

## So sánh với Sui gốc

| Aspect | Sui Original | MetaNode Integration |
|--------|-------------|---------------------|
| Trigger | Checkpoint-based | Go state monitoring |
| Lock | Execution Lock | Execution Lock + MetaNode barriers |
| State | EndOfEpochTransaction | Preserved MetaNode state |
| Consensus | Built-in | MetaNode's voting system |
| Restart | In-process | In-process (preserved) |

## Cấu hình

Không cần cấu hình thêm. Module Sui epoch transition sẽ được khởi tạo tự động với các settings hiện tại của MetaNode.

## Lợi ích

1. **State-based Triggering**: Trigger dựa trên trạng thái thực tế thay vì thời gian
2. **Execution Safety**: Execution lock đảm bảo consistency trong reconfiguration
3. **Preserved Compatibility**: Giữ nguyên luồng xử lý giao dịch hiện tại
4. **Gradual Migration**: Có thể chuyển đổi dần từ time-based sang state-based

## Files được thêm/sửa

### Files mới:
- `src/sui_epoch_transition.rs`: Core Sui epoch transition logic

### Files được sửa:
- `src/main.rs`: Thêm module import
- `src/node.rs`: Tích hợp SuiEpochTransitionOrchestrator

## Testing

Để test integration:

1. **Unit Tests**: Test riêng các components Sui epoch transition
2. **Integration Tests**: Test end-to-end epoch transition flow
3. **Compatibility Tests**: Đảm bảo không break existing transaction processing

## Future Enhancements

1. **Checkpoint-based Triggering**: Thay thế Go state monitoring bằng checkpoint-based (giống Sui hoàn toàn)
2. **EndOfEpochTransaction**: Implement Sui-style system transactions
3. **Advanced Reconfiguration**: Thêm các bước reconfiguration phức tạp hơn
