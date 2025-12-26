# Fork-Safety và Progress Guarantee

## Tổng quan

Tài liệu này mô tả các cơ chế đảm bảo:
1. **Fork-safety**: Tất cả nodes transition sang epoch mới với **cùng state**, tránh fork (nodes ở cùng epoch nhưng có state khác nhau)
2. **Progress guarantee**: Hệ thống luôn tiến về phía trước (không bị stuck)

## Vấn đề Fork

### Fork là gì?

**Fork** xảy ra khi các nodes ở cùng epoch nhưng có:
- `last_commit_index` khác nhau
- `last_global_exec_index` khác nhau
- `epoch_timestamp_ms` khác nhau
- Genesis blocks có hash khác nhau

**Hậu quả:**
- Nodes không thể validate blocks từ nhau
- Consensus bị dừng
- Network bị chia tách

### Ví dụ Fork

```
Node 0: epoch=3, last_commit_index=622, last_global_exec_index=5000
Node 1: epoch=3, last_commit_index=650, last_global_exec_index=5028  ← FORK!
```

Cả hai nodes đều ở epoch 3, nhưng có `last_global_exec_index` khác nhau → Fork!

## Fork-Safety Mechanisms

### 1. Global Execution Index - Deterministic Calculation

**File**: `metanode/src/checkpoint.rs`

```rust
pub fn calculate_global_exec_index(
    epoch: u64,
    commit_index: u32,
    last_global_exec_index: u64,
) -> u64 {
    if epoch == 0 {
        commit_index as u64
    } else {
        last_global_exec_index + commit_index as u64
    }
}
```

**Đảm bảo Fork-Safety:**
- ✅ Tất cả nodes tính cùng giá trị từ cùng inputs (`epoch`, `commit_index`, `last_global_exec_index`)
- ✅ **Deterministic**: Không phụ thuộc vào timing hay network

**Đảm bảo Progress:**
- ✅ `global_exec_index` luôn tăng (không reset giữa các epoch)
- ✅ Mỗi epoch tiếp tục từ `last_global_exec_index`

### 2. Commit Index Barrier

**Vấn đề:**
- Node A transition ở commit 622
- Node B transition ở commit 650
- → `last_commit_index` khác nhau → `global_exec_index` khác nhau → Fork

**Giải pháp:**
- Tất cả nodes phải đạt **barrier** (`proposal_commit_index + 10`) trước khi transition
- Tất cả nodes dùng **barrier** làm `last_commit_index`, không dùng `current_commit_index`

**Code:**
```rust
// Early barrier setting: khi proposal đạt quorum và đã committed
if quorum_status == Some(true) {
    if current_commit_index >= proposal.proposal_commit_index {
        let transition_commit_index = proposal.proposal_commit_index.saturating_add(10);
        self.transition_barrier.store(transition_commit_index, Ordering::SeqCst);
    }
}

// Use barrier as last_commit_index (deterministic)
let last_commit_index = transition_commit_index; // NOT current_commit_index!
```

**Fork-Safety:**
- ✅ Tất cả nodes set cùng barrier: `barrier = proposal_commit_index + 10`
- ✅ **Deterministic**: Tất cả nodes tính cùng giá trị

**Progress Guarantee:**
- ✅ Barrier chỉ skip commits **past barrier**
- ✅ Commits **before barrier** vẫn được xử lý bình thường
- ✅ **Timeout exception**: Nếu barrier không đạt sau 5 phút, cho phép transition (vẫn dùng barrier làm last_commit_index)

### 3. Commit Processor - Skip Commits Past Barrier

**File**: `metanode/src/commit_processor.rs`

```rust
// Check barrier BEFORE calculating global_exec_index
let (is_past_barrier, barrier_value) = if let Some(barrier) = transition_barrier.as_ref() {
    let barrier_val = barrier.load(Ordering::Relaxed);
    let effective_barrier = if barrier_val > 0 { barrier_val } else { barrier_snapshot };
    (effective_barrier > 0 && commit_index > effective_barrier, effective_barrier)
} else {
    (false, 0)
};

if is_past_barrier {
    // Skip commit entirely, don't calculate global_exec_index
    // Re-queue transactions for next epoch
    next_expected_index += 1;
    continue; // Skip this commit
}
```

**Fork-Safety:**
- ✅ Tất cả nodes skip cùng commits past barrier (deterministic)
- ✅ Transactions được re-queue deterministically

**Progress Guarantee:**
- ✅ `next_expected_index` vẫn tăng (advance để xử lý commit tiếp theo)
- ✅ Commits before barrier vẫn được xử lý
- ✅ **Không stuck**: Hệ thống tiếp tục xử lý commits sau barrier

### 4. Epoch Transition - Deterministic Last Commit Index

**File**: `metanode/src/node.rs`

```rust
// CRITICAL FORK-SAFETY: Use transition_commit_index (barrier) as last_commit_index
// NOT current_commit_index!
let last_commit_index = transition_commit_index; // Use barrier, not current_commit_index!
let new_last_global_exec_index = calculate_global_exec_index(
    old_epoch,
    last_commit_index,
    self.last_global_exec_index
);
```

**Fork-Safety:**
- ✅ Tất cả nodes dùng cùng `last_commit_index` (barrier) → cùng `new_last_global_exec_index`
- ✅ **Không fork**: Tất cả nodes transition với cùng state

**Progress Guarantee:**
- ✅ `last_global_exec_index` được tính từ barrier (deterministic)
- ✅ Epoch mới tiếp tục từ `new_last_global_exec_index`
- ✅ **Không reset**: Global_exec_index tiếp tục tăng

### 5. Go Master - Sequential Processing

**File**: `mtn-simple-2025/cmd/simple_chain/processor/block_processor.go`

```go
// Initialize from last block in DB
var nextExpectedGlobalExecIndex uint64
lastBlockFromDB := bp.GetLastBlock()
if lastBlockFromDB != nil {
    nextExpectedGlobalExecIndex = lastBlockFromDB.Header().BlockNumber() + 1
}

// Only process when global_exec_index == nextExpectedGlobalExecIndex
if globalExecIndex == nextExpectedGlobalExecIndex {
    // Process block
    nextExpectedGlobalExecIndex = globalExecIndex + 1
}
```

**Fork-Safety:**
- ✅ Tất cả Go Masters xử lý cùng thứ tự (sequential)
- ✅ Chỉ xử lý khi `global_exec_index == nextExpectedGlobalExecIndex`
- ✅ Out-of-order blocks được buffer, xử lý khi đến lượt

**Progress Guarantee:**
- ✅ `nextExpectedGlobalExecIndex` luôn tăng sau mỗi block
- ✅ Out-of-order blocks được buffer, xử lý khi đến lượt
- ✅ **Retention policy**: Skipped commits được lưu tạm (100 commits) để xử lý nếu đến muộn

### 6. Quorum Validation

**Vấn đề:**
- Nếu một node transition mà các nodes khác không → Fork

**Giải pháp:**
- Phải đạt quorum (2f+1 votes) trước khi transition
- Tất cả nodes phải thấy quorum đạt

**Code:**
```rust
ensure!(
    manager.check_proposal_quorum(proposal) == Some(true),
    "FORK-SAFETY: Quorum not reached for epoch transition - need 2f+1 votes"
);
```

### 7. Vote Propagation

**Vấn đề:**
- Node A đạt quorum và dừng broadcast votes
- Node B và C không nhận được votes → không thấy quorum → không transition → Fork

**Giải pháp:**
- Votes tiếp tục được broadcast ngay cả sau khi đạt quorum
- Đảm bảo tất cả nodes đều thấy quorum

### 8. Adaptive Sync Mode

**File**: `metanode/meta-consensus/core/src/commit_syncer.rs`

Khi node lag quá xa (>50 commits hoặc >5%), hệ thống tự động:
- Tăng batch size (1.5x hoặc 2x)
- Tăng parallel fetches (1.5x)
- Giảm check interval (1s thay vì 2s)

**Fork-Safety:**
- ✅ Sync chỉ fetch commits từ peers (verified)
- ✅ **Deterministic**: Tất cả nodes process cùng commits

**Progress Guarantee:**
- ✅ Node sẽ catch-up với quorum
- ✅ **Không stuck**: Adaptive sync đảm bảo catch-up

## Fork-Safety Validations

Khi transition, hệ thống thực hiện các validation sau:

1. ✅ **Commit Index Barrier**: Đảm bảo đạt barrier trước khi transition
2. ✅ **Quorum Check**: Đảm bảo đạt quorum (2f+1 votes)
3. ✅ **Deterministic last_commit_index**: Dùng barrier làm `last_commit_index`
4. ✅ **Deterministic global_exec_index**: Tính từ cùng `last_commit_index`
5. ✅ **Proposal Hash Consistency**: Verify hash giống nhau
6. ✅ **Timestamp Consistency**: Verify timestamp giống nhau

## Progress Guarantees

Hệ thống đảm bảo luôn tiến về phía trước:

1. ✅ **Global_exec_index luôn tăng**: Không reset giữa các epoch
2. ✅ **nextExpectedGlobalExecIndex luôn tăng**: Sau mỗi block
3. ✅ **Skip commits không block progress**: Advance index để xử lý commit tiếp theo
4. ✅ **Out-of-order handling**: Blocks được buffer, xử lý khi đến lượt
5. ✅ **Sync mode**: Đảm bảo catch-up khi lag
6. ✅ **Timeout exception**: Cho phép epoch transition sau 5 phút nếu barrier không đạt

## Logging và Verification

### Logs khi Transition

Hệ thống log chi tiết các giá trị deterministic:

```
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
🔄 EPOCH TRANSITION START: epoch 5 -> 6
  📊 Current State (BEFORE transition):
    - Current epoch: 5
    - Current commit index: 937
    - Last global exec index: 5000
    - Proposal commit index: 923
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
📊 FORK-SAFETY: Deterministic Values (ALL NODES MUST MATCH)
  🔑 Key Values:
    - Old epoch: 5
    - New epoch: 6
    - Last commit index (barrier): 933 (DETERMINISTIC - all nodes use this)
    - Current commit index: 937 (node-specific, may differ)
    - Commits past barrier: 4 (node-specific)
  📈 Global Execution Index:
    - Last global exec index (old epoch): 5000
    - New last global exec index (new epoch): 5933 (DETERMINISTIC - all nodes compute same)
    - Calculation: 5000 (old epoch) + 933 (barrier commit) = 5933
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
📊 FINAL STATE (AFTER transition) - FORK-SAFETY VERIFICATION:
  🔑 Deterministic Values (ALL NODES MUST MATCH - verify across all nodes):
    - New epoch: 6
    - Last commit index (barrier): 933 (used for transition - ALL NODES MUST USE THIS)
    - Last global exec index: 5933 (DETERMINISTIC - all nodes must have same)
    - Epoch timestamp: 1766303799266 (DETERMINISTIC - all nodes must have same)
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
⚠️  FORK-SAFETY CHECK: Verify all nodes have SAME values:
    - epoch: 6
    - last_commit_index (barrier): 933
    - last_global_exec_index: 5933
    - epoch_timestamp_ms: 1766303799266
   If any node has different values → FORK DETECTED!
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

### Verification Script

Sử dụng `./scripts/analysis/verify_epoch_transition.sh` để verify fork-safety:

```bash
./scripts/analysis/verify_epoch_transition.sh
```

Script sẽ:
- Extract các giá trị từ logs và committee.json
- So sánh giữa các nodes
- Phát hiện fork nếu có mismatch

## Best Practices

### 1. Luôn verify sau transition

```bash
# Sau mỗi epoch transition, chạy:
./scripts/analysis/verify_epoch_transition.sh
```

### 2. Monitor logs

```bash
# Kiểm tra deterministic values trong logs
grep "Deterministic Values.*ALL NODES MUST MATCH" logs/latest/node_*.log
grep "Last commit index (barrier)" logs/latest/node_*.log
grep "Last global exec index" logs/latest/node_*.log
```

### 3. Đảm bảo votes propagate

```bash
# Kiểm tra vote propagation
./scripts/analysis/analyze_vote_propagation.sh
```

### 4. Sync committee.json khi cần

Nếu một node restart sau transition, cần sync `committee.json` từ peers:

```bash
# Copy từ node đã transition
cp config/committee_node_0.json config/committee_node_1.json
./scripts/node/restart_node.sh 1
```

## Troubleshooting

### Fork Detected

**Triệu chứng:**
- `verify_epoch_transition.sh` báo fork
- Nodes có `last_global_exec_index` khác nhau

**Giải pháp:**
1. Xác định node đúng (node có quorum và transition thành công)
2. Sync `committee.json` từ node đúng
3. Restart các nodes sai
4. Verify lại

### Quorum không đạt

**Triệu chứng:**
- Một số nodes không transition
- Log hiển thị "quorum not reached"

**Giải pháp:**
1. Kiểm tra votes có propagate không
2. Đảm bảo tất cả nodes online vote
3. Với 3 nodes online, cần 100% nodes vote (3/3)

### Node bị stuck

**Triệu chứng:**
- Node không process commits
- `nextExpectedGlobalExecIndex` không tăng

**Giải pháp:**
1. Kiểm tra logs để xem có out-of-order blocks không
2. Kiểm tra sync mode có được kích hoạt không
3. Restart node nếu cần

## Tổng kết

✅ **Hệ thống đảm bảo fork-safety và progress**:
- Deterministic calculations
- Sequential processing
- Proper barrier handling
- Adaptive sync mode
- Comprehensive error handling

Hệ thống được thiết kế để:
1. **Luôn tiến về phía trước** (không bị stuck)
2. **Không có fork** (tất cả nodes có cùng state)

## Tham khảo

- [EPOCH.md](./EPOCH.md) - Epoch transition mechanism
- [QUORUM_LOGIC.md](./QUORUM_LOGIC.md) - Quorum logic
- [EPOCH_PRODUCTION.md](./EPOCH_PRODUCTION.md) - Production best practices
- [SYNC_MODE_IMPROVEMENTS.md](./SYNC_MODE_IMPROVEMENTS.md) - Adaptive sync mode
