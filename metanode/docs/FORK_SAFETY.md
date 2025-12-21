# Fork-Safety trong Epoch Transition

## Tổng quan

Fork-safety đảm bảo tất cả nodes transition sang epoch mới với **cùng state**, tránh fork (nodes ở cùng epoch nhưng có state khác nhau).

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

### 1. Commit Index Barrier

**Vấn đề:**
- Node A transition ở commit 622
- Node B transition ở commit 650
- → `last_commit_index` khác nhau → `global_exec_index` khác nhau → Fork

**Giải pháp:**
- Tất cả nodes phải đạt **barrier** (`proposal_commit_index + 10`) trước khi transition
- Tất cả nodes dùng **barrier** làm `last_commit_index`, không dùng `current_commit_index`

**Code:**
```rust
let transition_commit_index = proposal.proposal_commit_index.saturating_add(10);
ensure!(
    current_commit_index >= transition_commit_index,
    "FORK-SAFETY: Must wait until commit index {} (current: {})",
    transition_commit_index,
    current_commit_index
);

// Use barrier as last_commit_index (deterministic)
let last_commit_index = transition_commit_index; // NOT current_commit_index!
```

### 2. Deterministic Global Execution Index

**Vấn đề:**
- Nếu nodes dùng `current_commit_index` khác nhau → `global_exec_index` khác nhau → Fork

**Giải pháp:**
- Tất cả nodes dùng **cùng `last_commit_index`** (barrier) để tính `global_exec_index`
- Công thức: `global_exec_index = last_global_exec_index + last_commit_index`

**Code:**
```rust
let new_last_global_exec_index = calculate_global_exec_index(
    old_epoch,
    last_commit_index,  // Use barrier, not current_commit_index!
    self.last_global_exec_index,
);
```

### 3. Quorum Validation

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

### 4. Vote Propagation

**Vấn đề:**
- Node A đạt quorum và dừng broadcast votes
- Node B và C không nhận được votes → không thấy quorum → không transition → Fork

**Giải pháp:**
- Votes tiếp tục được broadcast ngay cả sau khi đạt quorum
- Đảm bảo tất cả nodes đều thấy quorum

**Code:**
```rust
// CRITICAL FIX: Continue broadcasting votes even after quorum is reached!
// This ensures all nodes see quorum and can transition together.
pub fn get_pending_votes_to_broadcast(&self) -> Vec<EpochChangeVote> {
    // Always broadcast votes, even if quorum is reached
    // This ensures all nodes can see quorum and transition together
    let mut out = Vec::new();
    for proposal in self.pending_proposals.values() {
        let proposal_hash = self.hash_proposal(proposal);
        if let Some(votes_by_voter) = self.proposal_votes.get(&proposal_hash) {
            out.extend(votes_by_voter.values().cloned());
        }
    }
    out
}
```

### 5. Proposal Hash Consistency

**Vấn đề:**
- Nếu proposal hash khác nhau giữa các nodes → votes không được count → không đạt quorum

**Giải pháp:**
- Verify proposal hash được tính giống nhau ở tất cả nodes
- Hash được tính từ các field deterministic

**Code:**
```rust
pub fn hash_proposal(&self, proposal: &EpochChangeProposal) -> Vec<u8> {
    // Hash từ các field deterministic:
    // - new_epoch
    // - new_committee.epoch()
    // - new_epoch_timestamp_ms
    // - proposal_commit_index
    // - proposer().value()
    let proposal_data = format!(
        "{}-{}-{}-{}-{}",
        proposal.new_epoch,
        proposal.new_committee.epoch(),
        proposal.new_epoch_timestamp_ms,
        proposal.proposal_commit_index,
        proposal.proposer().value()
    );
    Blake2b256::digest(proposal_data.as_bytes()).to_vec()
}
```

### 6. Timestamp Consistency

**Vấn đề:**
- Nếu `epoch_timestamp_ms` khác nhau → genesis blocks có hash khác nhau → Fork

**Giải pháp:**
- Tất cả nodes dùng cùng `epoch_timestamp_ms` từ proposal
- Sync timestamp khi catch-up

**Code:**
```rust
// Sync timestamp from proposal if catch-up scenario
if is_catchup {
    self.epoch_start_timestamp_ms = proposal.new_epoch_timestamp_ms;
}
```

## Fork-Safety Validations

Khi transition, hệ thống thực hiện các validation sau:

1. ✅ **Commit Index Barrier**: Đảm bảo đạt barrier trước khi transition
2. ✅ **Quorum Check**: Đảm bảo đạt quorum (2f+1 votes)
3. ✅ **Deterministic last_commit_index**: Dùng barrier làm `last_commit_index`
4. ✅ **Deterministic global_exec_index**: Tính từ cùng `last_commit_index`
5. ✅ **Proposal Hash Consistency**: Verify hash giống nhau
6. ✅ **Timestamp Consistency**: Verify timestamp giống nhau

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
# hoặc (với symlink)
./verify_epoch_transition.sh

```bash
./scripts/analysis/verify_epoch_transition.sh
# hoặc (với symlink)
./verify_epoch_transition.sh
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
# hoặc (với symlink)
./verify_epoch_transition.sh
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
# hoặc (với symlink)
./restart_node.sh 1
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

## Tham khảo

- [EPOCH.md](./EPOCH.md) - Epoch transition mechanism
- [QUORUM_LOGIC.md](./QUORUM_LOGIC.md) - Quorum logic
- [EPOCH_PRODUCTION.md](./EPOCH_PRODUCTION.md) - Production best practices

