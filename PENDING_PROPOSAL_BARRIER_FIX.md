# Fix: Pending Proposal Barrier Check

## Vấn đề

Transaction `a33ec3461315eed0d45f977e796d1a969f4a74218b091524fa1a7414f56130c6` bị mất do race condition:

**Timeline:**
1. 11:45:11.797982Z - Transaction được submit vào consensus
2. 11:45:11.832103Z - Transaction included vào block B1284
3. 11:45:12.105485Z - Barrier được set (barrier=1281) ⚠️ **SAU KHI transaction đã được included!**
4. 11:45:12.339325Z - Commit #1284 > barrier=1281 → Empty commit → Transaction bị mất

**Nguyên nhân:**
- Barrier được set khi proposal được **APPROVE** (có quorum và đạt barrier commit_index)
- Nhưng có thể có transaction đã được submit và included vào block **TRƯỚC KHI** barrier được set
- Khi commit xảy ra với commit_index > barrier, transaction bị mất trong empty commit

## Giải pháp

Thêm logic check **pending proposals với quorum reached** và queue transactions nếu `current_commit_index` gần barrier (within 3 commits):

```rust
// 6. CRITICAL RACE CONDITION FIX: Check pending proposals with quorum reached
// If there's a pending proposal for next epoch with quorum reached and current_commit_index
// is close to the barrier (within 3 commits), queue transactions to prevent loss.
let current_commit_index = self.current_commit_index.load(Ordering::SeqCst);
let next_epoch_proposals: Vec<_> = all_pending_proposals.iter()
    .filter(|p| p.new_epoch == current_epoch + 1)
    .collect();

for proposal in next_epoch_proposals {
    // Check if proposal has quorum (2f+1 votes)
    let quorum_status = manager.check_proposal_quorum(proposal);
    if quorum_status == Some(true) {
        // Quorum reached - proposal will be approved soon
        let transition_commit_index = proposal.proposal_commit_index.saturating_add(10);
        // Queue transactions if we're within 3 commits of barrier
        if current_commit_index >= transition_commit_index.saturating_sub(3) {
            // Queue transactions for next epoch
            return (false, true, format!("Barrier phase (pending): proposal with quorum reached, barrier={} will be set soon", transition_commit_index));
        }
    }
}
```

## Fork-Safety

Giải pháp này **fork-safe** vì:

1. **Deterministic proposal**: Tất cả nodes nhận cùng proposal với cùng quorum status
2. **Deterministic barrier**: Barrier value được tính từ `proposal.proposal_commit_index + 10` (deterministic cho tất cả nodes)
3. **Deterministic queue point**: Tất cả nodes sẽ queue khi `current_commit_index >= barrier - 3` (cùng logic point)
4. **Deterministic submission**: Queued transactions được sort by hash và submit deterministically trong epoch tiếp theo

## Kết quả

- Transaction sẽ được queue nếu có pending proposal với quorum reached và `current_commit_index >= barrier - 3`
- Tránh race condition nơi transaction được submit trước khi barrier được set
- Fork-safe: Tất cả nodes sẽ queue tại cùng một điểm logic

