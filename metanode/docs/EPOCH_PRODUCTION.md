# Next Epoch cho Production: Best Practices

## Tổng quan

Triển khai epoch transition cho production đòi hỏi:
- **Zero/Minimal Downtime**: Hệ thống phải hoạt động liên tục
- **Safety**: Không mất dữ liệu, không fork
- **Coordination**: Tất cả nodes phải đồng bộ
- **Rollback Capability**: Có thể rollback nếu có vấn đề
- **Monitoring**: Theo dõi và alerting
- **Automation**: Giảm lỗi human error

## Giải pháp Tốt nhất: Consensus-Based Epoch Change

### Tại sao Consensus-Based?

**Ưu điểm:**
1. **Safety**: Nodes tự vote, đảm bảo quorum đồng ý
2. **Coordination**: Tự động đồng bộ, không cần external coordinator
3. **No Single Point of Failure**: Không phụ thuộc vào admin node
4. **Atomic**: Tất cả nodes transition cùng lúc
5. **Auditable**: Có thể track ai vote gì

**So sánh:**

| Approach | Safety | Coordination | Downtime | Complexity |
|----------|--------|--------------|----------|------------|
| **Consensus-Based** | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | Minimal | High |
| **External Coordinator** | ⭐⭐⭐ | ⭐⭐⭐ | Medium | Medium |
| **Manual (Admin)** | ⭐⭐ | ⭐ | High | Low |
| **Time-based Auto** | ⭐⭐⭐ | ⭐⭐⭐⭐ | Low | Medium |

## Architecture: Consensus-Based Epoch Change

### 1. Epoch Change Proposal

**Structure:**
```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EpochChangeProposal {
    /// Epoch mới
    pub new_epoch: u64,
    
    /// Committee mới
    pub new_committee: Committee,
    
    /// Epoch start timestamp
    pub new_epoch_timestamp_ms: u64,
    
    /// Commit index khi proposal được tạo
    pub proposal_commit_index: u32,
    
    /// Proposer (authority index)
    pub proposer: AuthorityIndex,
    
    /// Signature của proposer
    pub signature: ProtocolKeySignature,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EpochChangeVote {
    /// Proposal hash
    pub proposal_hash: Vec<u8>,
    
    /// Voter (authority index)
    pub voter: AuthorityIndex,
    
    /// Vote: true = approve, false = reject
    pub approve: bool,
    
    /// Signature của voter
    pub signature: ProtocolKeySignature,
}
```

### 2. Epoch Change Protocol

**Flow:**

```
1. Propose Phase:
   - Authority đề xuất epoch change
   - Broadcast proposal đến tất cả nodes
   - Proposal được include trong block

2. Vote Phase:
   - Nodes nhận proposal
   - Validate proposal (committee, epoch, timestamp)
   - Vote approve/reject (auto-vote on valid proposal)
   - Broadcast vote trong blocks
   - **CRITICAL**: Votes tiếp tục được broadcast ngay cả sau khi đạt quorum để đảm bảo tất cả nodes đều thấy quorum

3. Decision Phase:
   - Collect votes từ quorum (2f+1)
   - Nếu đạt quorum approve → transition
   - Nếu đạt quorum reject → discard

4. Transition Phase:
   - Tất cả nodes transition cùng lúc tại commit index barrier
   - Stop consensus authority cũ (graceful shutdown)
   - Start consensus authority mới với committee mới
   - **Fork-Safety**: Tất cả nodes dùng cùng `last_commit_index` (barrier) để tính `global_exec_index`
   - **Deterministic**: Tất cả nodes có cùng `last_global_exec_index` sau transition
```

### 3. Implementation

**Step 1: Add Epoch Change Proposal to Block**

```rust
// Trong block.rs
#[derive(Clone, Debug)]
pub struct Block {
    // ... existing fields ...
    
    /// Optional epoch change proposal
    pub epoch_change_proposal: Option<EpochChangeProposal>,
    
    /// Optional epoch change votes
    pub epoch_change_votes: Vec<EpochChangeVote>,
}
```

**Step 2: Epoch Change Manager**

```rust
pub struct EpochChangeManager {
    /// Pending proposals
    pending_proposals: HashMap<Vec<u8>, EpochChangeProposal>,
    
    /// Votes cho mỗi proposal
    proposal_votes: HashMap<Vec<u8>, Vec<EpochChangeVote>>,
    
    /// Current epoch
    current_epoch: u64,
    
    /// Committee
    committee: Arc<Committee>,
}

impl EpochChangeManager {
    /// Propose epoch change
    pub fn propose_epoch_change(
        &mut self,
        new_committee: Committee,
        new_epoch_timestamp_ms: u64,
        proposer_keypair: &ProtocolKeyPair,
    ) -> Result<EpochChangeProposal> {
        let new_epoch = self.current_epoch + 1;
        
        // Validate new committee
        self.validate_new_committee(&new_committee)?;
        
        let proposal = EpochChangeProposal {
            new_epoch,
            new_committee: new_committee.clone(),
            new_epoch_timestamp_ms,
            proposal_commit_index: self.get_current_commit_index(),
            proposer: self.get_own_index(),
            signature: self.sign_proposal(&new_committee, proposer_keypair)?,
        };
        
        let proposal_hash = self.hash_proposal(&proposal);
        self.pending_proposals.insert(proposal_hash, proposal.clone());
        
        Ok(proposal)
    }
    
    /// Vote on proposal
    pub fn vote_on_proposal(
        &mut self,
        proposal: &EpochChangeProposal,
        voter_keypair: &ProtocolKeyPair,
    ) -> Result<EpochChangeVote> {
        // Validate proposal
        self.validate_proposal(proposal)?;
        
        // Decide vote (có thể thêm logic phức tạp hơn)
        let approve = self.should_approve_proposal(proposal)?;
        
        let vote = EpochChangeVote {
            proposal_hash: self.hash_proposal(proposal),
            voter: self.get_own_index(),
            approve,
            signature: self.sign_vote(proposal, approve, voter_keypair)?,
        };
        
        let proposal_hash = self.hash_proposal(proposal);
        self.proposal_votes
            .entry(proposal_hash)
            .or_insert_with(Vec::new)
            .push(vote.clone());
        
        Ok(vote)
    }
    
    /// Check if proposal has quorum
    pub fn check_proposal_quorum(
        &self,
        proposal: &EpochChangeProposal,
    ) -> Option<bool> {
        let proposal_hash = self.hash_proposal(proposal);
        let votes = self.proposal_votes.get(&proposal_hash)?;
        
        let approve_stake: Stake = votes
            .iter()
            .filter(|v| v.approve)
            .map(|v| self.committee.authority(v.voter).stake)
            .sum();
        
        let reject_stake: Stake = votes
            .iter()
            .filter(|v| !v.approve)
            .map(|v| self.committee.authority(v.voter).stake)
            .sum();
        
        // Check quorum approve
        if approve_stake >= self.committee.quorum_threshold() {
            return Some(true);
        }
        
        // Check quorum reject
        if reject_stake >= self.committee.quorum_threshold() {
            return Some(false);
        }
        
        None // Chưa đạt quorum
    }
    
    /// Transition to new epoch
    pub async fn transition_to_epoch(
        &self,
        proposal: &EpochChangeProposal,
        authority: &mut ConsensusAuthority,
    ) -> Result<()> {
        // Validate quorum
        match self.check_proposal_quorum(proposal) {
            Some(true) => {
                info!("Epoch change approved, transitioning to epoch {}", proposal.new_epoch);
            }
            Some(false) => {
                anyhow::bail!("Epoch change rejected by quorum");
            }
            None => {
                anyhow::bail!("Epoch change proposal has not reached quorum yet");
            }
        }
        
        // Graceful shutdown current authority
        self.graceful_shutdown_authority(authority).await?;
        
        // Start new authority with new committee
        let new_authority = self.start_new_authority(proposal).await?;
        
        Ok(())
    }
}
```

**Step 3: Graceful Shutdown**

```rust
impl EpochChangeManager {
    async fn graceful_shutdown_authority(
        &self,
        authority: &mut ConsensusAuthority,
    ) -> Result<()> {
        info!("Starting graceful shutdown for epoch transition...");
        
        // 1. Stop accepting new transactions
        authority.stop_accepting_transactions().await?;
        
        // 2. Wait for pending transactions to be included
        self.wait_for_pending_transactions(authority).await?;
        
        // 3. Wait for current round to complete
        self.wait_for_current_round_complete(authority).await?;
        
        // 4. Flush all pending commits
        self.flush_pending_commits(authority).await?;
        
        // 5. Stop network
        authority.stop_network().await?;
        
        // 6. Stop consensus authority
        authority.stop().await?;
        
        info!("Graceful shutdown completed");
        Ok(())
    }
    
    async fn wait_for_pending_transactions(
        &self,
        authority: &ConsensusAuthority,
    ) -> Result<()> {
        let timeout = Duration::from_secs(30);
        let start = Instant::now();
        
        loop {
            let pending_count = authority.get_pending_transaction_count().await?;
            if pending_count == 0 {
                break;
            }
            
            if start.elapsed() > timeout {
                warn!("Timeout waiting for pending transactions, proceeding anyway");
                break;
            }
            
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        
        Ok(())
    }
    
    async fn wait_for_current_round_complete(
        &self,
        authority: &ConsensusAuthority,
    ) -> Result<()> {
        let timeout = Duration::from_secs(60);
        let start = Instant::now();
        
        let initial_round = authority.get_current_round().await?;
        
        loop {
            let current_round = authority.get_current_round().await?;
            if current_round > initial_round {
                break;
            }
            
            if start.elapsed() > timeout {
                warn!("Timeout waiting for round completion, proceeding anyway");
                break;
            }
            
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        
        Ok(())
    }
}
```

**Step 4: State Preservation**

```rust
impl EpochChangeManager {
    async fn start_new_authority(
        &self,
        proposal: &EpochChangeProposal,
    ) -> Result<ConsensusAuthority> {
        info!("Starting new authority for epoch {}", proposal.new_epoch);
        
        // 1. Load last commit index từ epoch cũ
        let last_commit_index = self.load_last_commit_index()?;
        
        // 2. Preserve DAG state (nếu cần)
        // - Copy DAG state từ epoch cũ
        // - Hoặc start fresh (tùy vào design)
        
        // 3. Create new context với committee mới
        let new_context = Context::new(
            proposal.new_epoch_timestamp_ms,
            self.get_own_index_in_new_committee(&proposal.new_committee)?,
            proposal.new_committee.clone(),
            // ... other params ...
        );
        
        // 4. Start authority với replay từ last_commit_index
        let authority = ConsensusAuthority::start(
            // ... params ...
            last_commit_index,  // Replay từ commit cuối cùng
        ).await?;
        
        info!("New authority started for epoch {}", proposal.new_epoch);
        Ok(authority)
    }
}
```

## Alternative: Phased Rollout (Simpler)

Nếu consensus-based quá phức tạp, có thể dùng **Phased Rollout**:

### Approach: Rolling Update với Coordination

**Flow:**

```
1. Preparation Phase:
   - Admin tạo committee mới với epoch + 1
   - Distribute committee.json đến tất cả nodes
   - Schedule transition time (ví dụ: 2025-01-15 02:00:00 UTC)

2. Pre-transition Phase:
   - Nodes load committee mới nhưng chưa dùng
   - Validate committee mới
   - Prepare state migration

3. Transition Phase (Coordinated):
   - Tất cả nodes transition tại scheduled time
   - Graceful shutdown
   - Start với committee mới

4. Post-transition Phase:
   - Verify tất cả nodes đã transition
   - Monitor cho issues
   - Rollback nếu cần
```

**Implementation:**

```rust
pub struct PhasedEpochTransition {
    /// Scheduled transition time
    transition_time: SystemTime,
    
    /// New committee (pre-loaded)
    new_committee: Option<Committee>,
    
    /// New epoch timestamp
    new_epoch_timestamp_ms: Option<u64>,
}

impl PhasedEpochTransition {
    /// Schedule epoch transition
    pub fn schedule_transition(
        &mut self,
        new_committee: Committee,
        transition_time: SystemTime,
    ) -> Result<()> {
        // Validate new committee
        self.validate_new_committee(&new_committee)?;
        
        // Calculate new epoch timestamp
        let new_epoch_timestamp_ms = transition_time
            .duration_since(UNIX_EPOCH)?
            .as_millis() as u64;
        
        self.new_committee = Some(new_committee);
        self.new_epoch_timestamp_ms = Some(new_epoch_timestamp_ms);
        self.transition_time = transition_time;
        
        info!(
            "Epoch transition scheduled for: {}",
            DateTime::<Utc>::from(transition_time)
        );
        
        Ok(())
    }
    
    /// Check if it's time to transition
    pub fn should_transition(&self) -> bool {
        SystemTime::now() >= self.transition_time
    }
    
    /// Execute transition
    pub async fn execute_transition(
        &self,
        authority: &mut ConsensusAuthority,
    ) -> Result<()> {
        let new_committee = self.new_committee.as_ref()
            .ok_or_else(|| anyhow!("No transition scheduled"))?;
        let new_epoch_timestamp = self.new_epoch_timestamp_ms
            .ok_or_else(|| anyhow!("No epoch timestamp set"))?;
        
        // Graceful shutdown
        self.graceful_shutdown_authority(authority).await?;
        
        // Start new authority
        let new_authority = self.start_new_authority(
            new_committee,
            new_epoch_timestamp,
        ).await?;
        
        Ok(())
    }
}
```

## Production Checklist

### Pre-Transition

- [ ] **Committee Validation**
  - Validate new committee size (minimum 4 nodes)
  - Validate quorum threshold (2f+1)
  - Validate validity threshold (f+1)
  - Verify all nodes have valid keys

- [ ] **State Backup**
  - Backup consensus database
  - Backup committee.json

- [ ] **Testing**
  - Test transition trên testnet/staging
  - Verify no data loss
  - Verify no fork
  - Test rollback procedure

- [ ] **Coordination**
  - Notify all operators
  - Schedule maintenance window (nếu cần)
  - Prepare rollback plan

### During Transition

- [ ] **Monitoring**
  - Monitor transaction throughput
  - Monitor commit rate
  - Monitor node health
  - Monitor network connectivity

- [ ] **Verification**
  - Verify all nodes transitioned
  - Verify no fork
  - Verify commits continue
  - Verify transactions process

### Post-Transition

- [ ] **Validation**
  - Verify epoch number increased
  - Verify committee updated
  - Verify all nodes in sync
  - Verify no errors in logs

- [ ] **Monitoring**
  - Monitor for 24-48 hours
  - Watch for anomalies
  - Collect metrics

- [ ] **Documentation**
  - Document transition
  - Update runbooks
  - Record lessons learned

## Rollback Procedure

### Khi nào cần Rollback?

- Fork detected
- Data loss
- Performance degradation
- Network partition
- Security issue

### Rollback Steps

```bash
# 1. Stop all nodes
pkill -f metanode

# 2. Restore backup
cp config/committee_epoch0.json config/committee.json
restore_consensus_db_backup.sh

# 3. Restart nodes
cargo run --bin metanode -- start --node-id 0
cargo run --bin metanode -- start --node-id 1
# ...

# 4. Verify
check_node_sync.sh
verify_no_fork.sh
```

## Monitoring và Alerting

### Metrics cần Monitor

```rust
// Epoch transition metrics
epoch_transition_duration_seconds
epoch_transition_success_total
epoch_transition_failure_total
epoch_transition_votes_total
epoch_transition_quorum_reached_total

// Post-transition metrics
epoch_commits_total{epoch="1"}
epoch_transactions_total{epoch="1"}
epoch_rounds_total{epoch="1"}
epoch_network_errors_total{epoch="1"}
```

### Alerts

```yaml
# Alert: Epoch transition failed
- alert: EpochTransitionFailed
  expr: epoch_transition_failure_total > 0
  for: 1m
  annotations:
    summary: "Epoch transition failed"

# Alert: Fork detected after transition
- alert: ForkAfterEpochTransition
  expr: fork_detected_total > 0
  for: 1m
  annotations:
    summary: "Fork detected after epoch transition"

# Alert: Nodes not in sync after transition
- alert: NodesNotInSync
  expr: nodes_in_sync < total_nodes * 0.8
  for: 5m
  annotations:
    summary: "Less than 80% nodes in sync after transition"
```

## Best Practices Summary

### 1. **Consensus-Based là Tốt nhất**

- Safety cao nhất
- Coordination tự động
- No single point of failure
- Atomic transition

### 2. **Graceful Shutdown**

- Stop accepting new transactions
- Wait for pending to complete
- Flush all state
- Preserve data

### 3. **State Preservation**

- Không reset state
- Continue từ last commit
- Preserve DAG state (nếu cần)
- Migrate commit state

### 4. **Phased Approach**

- Test trên staging trước
- Rollout từng phase
- Monitor kỹ
- Có rollback plan

### 5. **Monitoring**

- Monitor trước, trong, và sau transition
- Set up alerts
- Collect metrics
- Log everything

### 6. **Documentation**

- Document procedure
- Create runbooks
- Record lessons learned
- Update configs

## Implementation Priority

### Phase 1: Basic (Manual, Phased Rollout)

**Timeline:** 2-3 weeks

1. Implement graceful shutdown
2. Implement state preservation
3. Implement phased rollout
4. Test trên staging

### Phase 2: Consensus-Based

**Timeline:** 4-6 weeks

1. Implement epoch change proposal
2. Implement voting mechanism
3. Implement quorum checking
4. Integrate với consensus
5. Test thoroughly

### Phase 3: Automation

**Timeline:** 2-3 weeks

1. Auto-detect khi cần transition
2. Auto-propose epoch change
3. Auto-monitor và alert
4. Auto-rollback nếu cần

## Kết luận

**Cho Production, khuyến nghị:**

1. **Short-term:** Phased Rollout với coordination
   - Đơn giản hơn
   - Dễ test
   - Dễ rollback
   - Phù hợp cho early production

2. **Long-term:** Consensus-Based Epoch Change
   - Safety cao nhất
   - Tự động hóa
   - No downtime
   - Phù hợp cho mature production

**Trade-offs:**

- **Phased Rollout:** Đơn giản nhưng cần coordination
- **Consensus-Based:** Phức tạp nhưng tự động và safe hơn

Chọn approach dựa trên:
- Team expertise
- Timeline
- Risk tolerance
- Network size

## References

- [EPOCH.md](./EPOCH.md) - Tổng quan về epoch
- [ARCHITECTURE.md](./ARCHITECTURE.md) - Kiến trúc hệ thống
- [RECOVERY.md](./RECOVERY.md) - Recovery và state management

