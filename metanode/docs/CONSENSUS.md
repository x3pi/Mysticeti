# Cơ chế Consensus

## Tổng quan

MetaNode sử dụng **Sui Mysticeti Consensus Protocol**, một DAG-based consensus algorithm với các đặc điểm:

- **DAG-based**: Sử dụng Directed Acyclic Graph để tổ chức blocks
- **Leader-based**: Có leader election để quyết định commit
- **Byzantine Fault Tolerant**: Chịu được f faulty nodes trong 3f+1 nodes
- **High Throughput**: Có thể xử lý hàng trăm commits/second

## DAG Structure

### Blocks và Rounds

```
Round 1:     B1([0]) ──┐
                       │
Round 2:     B2([1]) ──┼──► B3([2]) ──┐
                       │              │
Round 3:     B4([3]) ──┘              │
                                     │
Round 4:     B5([0]) ────────────────┼──► B6([1]) ──┐
                                     │              │
Round 5:     B7([2]) ────────────────┘              │
                                                    │
Round 6:     B8([3]) ────────────────────────────────┘
```

**Giải thích:**
- Mỗi authority tạo một block mỗi round
- Blocks reference các blocks từ rounds trước (ancestors)
- Tạo thành một DAG (không có cycle)

### Block Structure

Mỗi block chứa:
- **Round**: Round number
- **Author**: Authority index tạo block
- **Transactions**: Danh sách transactions
- **Ancestors**: References đến blocks trước đó
- **Signature**: Được ký bởi protocol keypair

### Commits và SubDAGs

Một **commit** (CommittedSubDag) bao gồm:
- **Leader Block**: Block được chọn làm leader
- **Supporting Blocks**: Các blocks từ các authorities khác trong cùng round
- **Commit Index**: Số thứ tự commit (tăng dần)
- **Timestamp**: Thời điểm commit

**Ví dụ:**
```
Commit #100:
  Leader: B100([0], ...)  ← từ authority 0
  Blocks:
    - B99([1], ...)       ← từ authority 1
    - B99([2], ...)       ← từ authority 2
    - B99([3], ...)       ← từ authority 3
    - B100([0], ...)      ← leader từ authority 0
  Total: 4 blocks, 5 transactions
```

## Consensus Process

### 1. Block Proposal

Mỗi authority:
1. Thu thập transactions từ transaction pool
2. Tạo block mới với ancestors
3. Ký block bằng protocol keypair
4. Broadcast block đến tất cả peers

### 2. Block Verification

Khi nhận block:
1. Verify signature
2. Verify ancestors tồn tại
3. Verify transactions (nếu có verifier)
4. Add vào DAG state

### 3. Leader Election

Leader được chọn dựa trên:
- **Round number**: Mỗi round có một leader
- **Leader Schedule**: Dựa trên reputation scores
- **Stake**: Authorities có stake cao hơn có cơ hội làm leader nhiều hơn

### 4. Commit Finalization

Khi đủ votes cho leader block:
1. **Commit Observer** phát hiện commit
2. Tạo CommittedSubDag
3. Gửi đến Commit Consumer
4. Commit Processor xử lý theo thứ tự

## Byzantine Fault Tolerance

### Quorum và Thresholds

Với **n = 4 nodes**:
- **Quorum threshold**: 3 (2f+1 với f=1)
- **Validity threshold**: 2 (f+1)
- **Có thể chịu**: 1 faulty node

### Safety và Liveness

- **Safety**: Tất cả honest nodes commit cùng thứ tự
- **Liveness**: Hệ thống tiếp tục commit nếu có quorum

### Faulty Node Behavior

Faulty node có thể:
- Không tạo blocks
- Tạo blocks không hợp lệ
- Broadcast blocks muộn
- Không vote cho leader

Hệ thống vẫn hoạt động nếu chỉ có ≤f faulty nodes.

## Leader Schedule

### Reputation-based Leader Selection

Leader được chọn dựa trên:
1. **Reputation Scores**: Dựa trên performance lịch sử
2. **Stake**: Authorities có stake cao hơn
3. **Round-robin**: Đảm bảo fairness

### Leader Swap Table

- Tính toán dựa trên reputation scores
- Đảm bảo good nodes được chọn làm leader thường xuyên hơn
- Bad nodes ít được chọn hơn

## Commit Processing

### Ordered Execution

Commits phải được xử lý theo thứ tự:

```
Commit #1  ──► Process
Commit #2  ──► Process
Commit #3  ──► Wait (out of order)
Commit #4  ──► Wait
Commit #3  ──► Process (now in order)
Commit #4  ──► Process
```

### Commit Processor

1. Nhận CommittedSubDag từ consensus
2. Kiểm tra commit index
3. Nếu là commit tiếp theo → Process ngay
4. Nếu out-of-order → Lưu vào pending
5. Process pending khi đến lượt

## Synchronization

### Block Sync

Khi node lag:
1. **Synchronizer** phát hiện lag
2. Request missing blocks từ peers
3. Fetch blocks theo batch
4. Verify và add vào DAG
5. Catch up với current round

### Commit Sync

Khi node restart:
1. Load committed state từ storage
2. Sync missing commits từ peers
3. Verify commit chain
4. Replay commits nếu cần

## Performance Characteristics

### Throughput

- **Theoretical**: ~1000+ commits/second
- **Practical**: ~100-200 commits/second (tùy config)
- **Bottleneck**: Network latency, block size

### Latency

- **Block proposal**: ~50-100ms
- **Commit finalization**: ~200-500ms
- **End-to-end**: ~300-600ms

### Scalability

- **Node count**: Tốt nhất 4-16 nodes
- **Network**: Local network <10ms latency
- **Storage**: SSD recommended cho production

## Parameters

### Consensus Parameters

```rust
Parameters {
    leader_timeout: 200ms,              // Timeout cho leader
    min_round_delay: 50ms,              // Delay tối thiểu giữa rounds
    max_forward_time_drift: 500ms,      // Max time drift
    max_blocks_per_sync: 32,            // Blocks per sync batch
    max_blocks_per_fetch: 1000,         // Max blocks per fetch
    sync_last_known_own_block_timeout: 5s,
    round_prober_interval_ms: 5000,     // Round prober interval
    dag_state_cached_rounds: 500,      // Cached rounds in memory
    commit_sync_parallel_fetches: 8,    // Parallel fetch count
    commit_sync_batch_size: 100,        // Commit sync batch size
    commit_sync_batches_ahead: 32,      // Batches ahead
}
```

### Tuning Guidelines

**Tăng throughput:**
- Giảm `min_round_delay`
- Tăng `max_blocks_per_sync`
- Tăng `commit_sync_parallel_fetches`

**Giảm latency:**
- Giảm `leader_timeout`
- Giảm `min_round_delay`
- Tối ưu network

**Tăng reliability:**
- Tăng `leader_timeout`
- Tăng `sync_last_known_own_block_timeout`
- Tăng `round_prober_interval_ms`

## Recovery và Persistence

### State Recovery

Khi node restart:
1. Load DAG state từ RocksDB
2. Recover committed state
3. Recover block commit statuses
4. Recover commit observer state
5. Sync missing blocks từ peers

### Storage

- **DAG State**: Lưu trong RocksDB
- **Committed Blocks**: Persisted để recovery
- **Commit History**: Lưu để audit

### Recovery Time

- **Empty state**: <1 giây
- **1K commits**: ~1-2 giây
- **100K commits**: ~10-20 giây
- **1M+ commits**: ~40-60 giây

## References

- [Mysticeti Paper](https://arxiv.org/pdf/2310.14821)
- [Sui Consensus Documentation](https://docs.sui.io/learn/architecture/consensus)

