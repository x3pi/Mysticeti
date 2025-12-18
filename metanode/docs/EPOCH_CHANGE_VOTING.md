# Epoch Change Voting Mechanism

## Tổng quan

**CÓ, chuyển đổi epoch PHẢI được vote** trước khi transition. Đây là cơ chế consensus-based để đảm bảo tất cả nodes đồng ý với epoch change.

## Flow Vote hoàn chỉnh

```
1. Node A tạo EpochChangeProposal
   ↓
2. Broadcast proposal trong blocks (tạm thời disabled do BCS issue)
   ↓
3. Các nodes khác nhận proposal
   ↓
4. Mỗi node tự động vote (approve/reject) dựa trên validation
   ↓
5. Broadcast votes trong blocks
   ↓
6. Collect votes và check quorum
   ↓
7. Nếu quorum reached (2f+1 approve) → Transition
   ↓
8. Nếu quorum reject (2f+1 reject) → Reject proposal
```

## Quorum Requirement

### Quorum Threshold: **2f+1** (2/3 + 1)

- **f = số nodes có thể fail** (Byzantine fault tolerance)
- **Total nodes = 4** → f = 1 → quorum = 3
- **Cần ít nhất 3 nodes approve** để transition

### Ví dụ với 4 nodes:

```
Total stake: 4
Quorum threshold: 3 (2f+1 = 2*1+1 = 3)

Scenarios:
✅ 3 approve, 0 reject → APPROVED (transition)
✅ 3 approve, 1 reject → APPROVED (transition)
❌ 2 approve, 2 reject → PENDING (chưa đủ quorum)
❌ 1 approve, 3 reject → REJECTED (không transition)
```

## Vote Mechanism

### 1. Proposal Creation

**Location:** `src/epoch_change.rs::propose_epoch_change()`

```rust
pub fn propose_epoch_change(
    &mut self,
    new_committee: Committee,
    new_epoch_timestamp_ms: u64,
    proposal_commit_index: u32,
    proposer: AuthorityIndex,
    proposer_keypair: &ProtocolKeyPair,
) -> Result<EpochChangeProposal>
```

**Điều kiện:**
- Proposer phải là valid authority trong current committee
- Proposal phải được signed bởi proposer
- Proposal phải valid (epoch increment, valid committee, etc.)

### 2. Vote Creation

**Location:** `src/epoch_change.rs::vote_on_proposal()`

```rust
pub fn vote_on_proposal(
    &mut self,
    proposal: &EpochChangeProposal,
    voter: AuthorityIndex,
    voter_keypair: &ProtocolKeyPair,
) -> Result<EpochChangeVote>
```

**Quy trình:**
1. **Validate proposal** - Kiểm tra proposal có hợp lệ không
2. **Decide vote** - Quyết định approve/reject dựa trên `should_approve_proposal()`
3. **Sign vote** - Ký vote với voter's keypair
4. **Store vote** - Lưu vote vào `proposal_votes`

**Vote decision logic:**
```rust
fn should_approve_proposal(&self, proposal: &EpochChangeProposal) -> Result<bool> {
    // Approve nếu:
    // - Committee valid
    // - Epoch increment correct
    // - Timestamp valid
    // - Signature valid
    self.validate_new_committee(&proposal.new_committee)?;
    Ok(true)
}
```

### 3. Quorum Check

**Location:** `src/epoch_change.rs::check_proposal_quorum()`

```rust
pub fn check_proposal_quorum(
    &self,
    proposal: &EpochChangeProposal,
) -> Option<bool>  // Some(true) = approved, Some(false) = rejected, None = pending
```

**Logic:**
1. Tính tổng stake của **approve votes**
2. Tính tổng stake của **reject votes**
3. So sánh với quorum threshold:
   - `approve_stake >= quorum_threshold` → `Some(true)` (APPROVED)
   - `reject_stake >= quorum_threshold` → `Some(false)` (REJECTED)
   - Cả hai đều < threshold → `None` (PENDING)

### 4. Transition Trigger

**Location:** `src/node.rs::transition monitoring task`

```rust
// Check quorum status
if let Some(approved) = manager.check_proposal_quorum(proposal) {
    if approved {
        // Check commit index barrier
        if current_commit_index >= transition_commit_index {
            // Trigger transition
        }
    }
}
```

**Điều kiện transition:**
1. ✅ **Quorum reached** (2f+1 approve votes)
2. ✅ **Commit index barrier passed** (current >= proposal_commit_index + 10)

## Auto-Vote Mechanism (Cần implement)

### Hiện tại

**Status:** ⚠️ **CHƯA IMPLEMENT ĐẦY ĐỦ**

- Code có `vote_on_proposal()` method
- Nhưng chưa có mechanism tự động vote khi nhận proposal
- Votes chưa được broadcast trong blocks (do BCS issue)

### Cần implement

**Location:** `src/node.rs` hoặc `src/epoch_change_bridge.rs`

```rust
// Khi nhận proposal từ block
if let Some(proposal) = block.epoch_change_proposal() {
    // 1. Process proposal
    epoch_change_manager.write().await.process_proposal(proposal)?;
    
    // 2. Auto-vote nếu proposal valid
    let mut manager = epoch_change_manager.write().await;
    if manager.validate_proposal(&proposal).is_ok() {
        // Tự động vote approve
        let vote = manager.vote_on_proposal(
            &proposal,
            own_index,
            &protocol_keypair,
        )?;
        
        // Broadcast vote trong next block
        // (tạm thời disabled do BCS issue)
    }
}
```

## Vote Broadcast

### Hiện tại

**Status:** ⚠️ **TẠM THỜI DISABLED** (do BCS deserialization issue)

Votes được thiết kế để broadcast trong blocks:
- `block.epoch_change_votes()` - Chứa votes
- `block.set_epoch_change_votes()` - Set votes vào block

### Khi BCS issue được fix

```rust
// Khi tạo block
let (proposal, votes) = epoch_change_bridge
    .get_epoch_change_data_for_block_creation(&epoch_change_manager)
    .await;

block.set_epoch_change_proposal(proposal);
block.set_epoch_change_votes(votes);
```

## Security và Validation

### 1. Vote Signature

Mỗi vote phải được signed bởi voter's keypair:

```rust
let vote_message = format!(
    "epoch_vote:{}:{}:{}",
    hex::encode(&proposal_hash[..8]),
    approve,
    stake
);
let signature = voter_keypair.sign(vote_message.as_bytes());
```

### 2. Vote Validation

**Location:** `src/epoch_change.rs::validate_vote()`

```rust
pub fn validate_vote(&self, vote: &EpochChangeVote) -> Result<()> {
    // 1. Check proposal exists
    ensure!(
        self.pending_proposals.contains_key(&vote.proposal_hash),
        "Vote for unknown proposal"
    );
    
    // 2. Validate voter
    ensure!(
        self.committee.is_valid_index(vote.voter),
        "Invalid voter authority index"
    );
    
    // 3. Verify signature
    let voter_auth = self.committee.authority(vote.voter);
    let public_key = &voter_auth.protocol_key;
    public_key.verify(vote_message.as_bytes(), &vote.signature)?;
    
    Ok(())
}
```

### 3. Duplicate Vote Prevention

- Mỗi voter chỉ có thể vote 1 lần cho mỗi proposal
- Vote được store trong `proposal_votes` map
- Duplicate votes sẽ bị reject

## Monitoring và Logging

### Vote Events

```rust
// Vote created
info!("🗳️  Voted on epoch change proposal: proposal_hash={}, epoch {} -> {}, voter={}, approve={}");

// Quorum progress
info!("📊 Quorum progress: proposal_hash={}, approve_stake={}/{}, reject_stake={}/{}, threshold={}, votes={}");

// Quorum reached
info!("✅ QUORUM REACHED (APPROVE): proposal_hash={}, approve_stake={}/{}, threshold={}, votes={}");
info!("❌ QUORUM REACHED (REJECT): proposal_hash={}, reject_stake={}/{}, threshold={}, votes={}");
```

### Metrics

- `epoch_change_votes_total{proposal_hash,approve}` - Tổng số votes
- `epoch_change_quorum_progress{proposal_hash}` - Quorum progress (0-100%)
- `epoch_change_quorum_reached_total{epoch}` - Số lần quorum reached

## FAQ

### Q: Ai có thể vote?

**A:** Chỉ các authorities trong current committee có thể vote. Mỗi authority có 1 vote (với stake = 1).

### Q: Có thể vote nhiều lần không?

**A:** Không. Mỗi voter chỉ có thể vote 1 lần cho mỗi proposal. Duplicate votes sẽ bị reject.

### Q: Vote có thể thay đổi không?

**A:** Không. Vote là immutable và được signed. Không thể thay đổi sau khi đã vote.

### Q: Nếu quorum không đạt thì sao?

**A:** Proposal sẽ ở trạng thái PENDING. Có thể:
- Đợi thêm votes
- Proposal timeout (sau 5 phút)
- Tạo proposal mới

### Q: Có thể reject proposal không?

**A:** Có. Nếu `2f+1` nodes reject → proposal bị REJECTED và không transition.

## Kết luận

**✅ Epoch change PHẢI được vote:**

1. **Proposal** được tạo bởi một node
2. **Tất cả nodes vote** (approve/reject) trên proposal
3. **Quorum check** (2f+1 approve) → Transition
4. **Fork-safe** - Tất cả nodes transition cùng lúc tại commit index barrier

**⚠️ Lưu ý:**
- Auto-vote mechanism chưa được implement đầy đủ
- Vote broadcast trong blocks tạm thời disabled (do BCS issue)
- Cần implement auto-vote khi nhận proposal để hoàn thiện cơ chế

