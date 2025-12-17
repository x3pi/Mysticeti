# Epoch và Epoch Transition

## Tổng quan

**Epoch** là một khái niệm quan trọng trong consensus protocol, đại diện cho một giai đoạn hoạt động của network với một committee cố định. Mỗi epoch có:
- Một committee (tập hợp authorities) cố định
- Một epoch number (bắt đầu từ 0)
- Một epoch start timestamp

## Epoch trong Hệ thống

### 1. Epoch trong Committee

**File:** `config/committee.json`

```json
{
  "epoch": 0,
  "total_stake": 4,
  "quorum_threshold": 3,
  "validity_threshold": 2,
  "authorities": [...]
}
```

**Ý nghĩa:**
- `epoch`: Số epoch hiện tại (u64)
- Tất cả nodes phải dùng cùng epoch
- Blocks phải match epoch của committee

### 2. Epoch trong Blocks

**Block Verification:**

Mỗi block phải có epoch matching với committee:

```rust
// Trong block_verifier.rs
if block.epoch() != committee.epoch() {
    return Err(ConsensusError::WrongEpoch {
        expected: committee.epoch(),
        actual: block.epoch(),
    });
}
```

**Kết quả:**
- Blocks với epoch khác sẽ bị reject
- Chỉ blocks cùng epoch mới được accept
- Đảm bảo consensus chỉ xử lý blocks của epoch hiện tại

### 3. Epoch Start Timestamp

**File:** `config/epoch_timestamp.txt`

**Mục đích:**
- Timestamp khi epoch bắt đầu
- Tất cả nodes phải dùng cùng timestamp
- Được sử dụng trong block creation

**Code:**
```rust
// Trong node.rs
let epoch_start_timestamp = config.load_epoch_timestamp()?;
info!("Using epoch start timestamp: {}", epoch_start_timestamp);

// Trong context.rs
pub struct Context {
    pub epoch_start_timestamp_ms: u64,
    pub committee: Arc<Committee>,
    // ...
}
```

## Khi nào cần Next Epoch?

### 1. Thay đổi Committee

**Lý do:**
- Thêm nodes mới vào network
- Xóa nodes cũ khỏi network
- Thay đổi stake của nodes
- Rotate keys (security)

**Ví dụ:**
```
Epoch 0: 4 nodes (node_0, node_1, node_2, node_3)
Epoch 1: 5 nodes (node_0, node_1, node_2, node_3, node_4)  ← Thêm node
Epoch 2: 3 nodes (node_0, node_1, node_2)  ← Xóa 2 nodes
```

### 2. Time-based Epoch Change

**Lý do:**
- Rotate keys định kỳ (security)
- Reset reputation scores
- Cleanup old state

**Ví dụ:**
```
Epoch 0: 2025-01-01 00:00:00 - 2025-01-31 23:59:59
Epoch 1: 2025-02-01 00:00:00 - 2025-02-28 23:59:59
Epoch 2: 2025-03-01 00:00:00 - ...
```

### 3. Block-based Epoch Change

**Lý do:**
- Sau N commits
- Sau N rounds
- Sau khi đạt milestone

**Ví dụ:**
```
Epoch 0: Commit #1 - #10000
Epoch 1: Commit #10001 - #20000
Epoch 2: Commit #20001 - ...
```

### 4. Manual Epoch Change

**Lý do:**
- Admin trigger
- Emergency change
- Testing

## Hiện trạng: Epoch 0 Only

### Vấn đề hiện tại

**1. Hardcoded Epoch 0:**

```rust
// Trong config.rs
let committee = Committee::new(0, authorities);  // ← Luôn epoch 0
```

**2. Không có Epoch Transition:**

- Không có cơ chế để detect khi nào cần next epoch
- Không có logic để migrate state
- Không có cách để stop/start consensus authority với committee mới

**3. Epoch Check chỉ để Reject:**

```rust
// Chỉ reject blocks sai epoch, không có logic để accept epoch mới
if block.epoch() != committee.epoch() {
    return Err(ConsensusError::WrongEpoch { ... });
}
```

## Triển khai Next Epoch: Khó khăn và Giải pháp

### Đánh giá: Có thể triển khai, nhưng cần nhiều công việc

### ✅ Dễ dàng (Đã có sẵn)

**1. Committee Structure:**
- `Committee` đã support epoch field
- Có thể tạo committee mới với epoch + 1
- Epoch check đã được implement

**2. Epoch Timestamp:**
- Đã có `epoch_timestamp.txt`
- Đã có `load_epoch_timestamp()`
- Có thể generate timestamp mới

**3. Block Epoch:**
- Blocks đã có epoch field
- Block creation tự động set epoch từ committee

### ⚠️ Cần triển khai

**1. Epoch Transition Trigger**

**Option A: Time-based**
```rust
// Check nếu đã đến lúc next epoch
fn should_transition_epoch(current_epoch: u64, epoch_duration: Duration) -> bool {
    let epoch_start = load_epoch_start_timestamp(current_epoch)?;
    let now = SystemTime::now();
    let elapsed = now.duration_since(UNIX_EPOCH)? - epoch_start;
    elapsed >= epoch_duration
}
```

**Option B: Block-based**
```rust
// Check nếu đã đủ commits
fn should_transition_epoch(current_commit_index: u32, commits_per_epoch: u32) -> bool {
    current_commit_index % commits_per_epoch == 0
}
```

**Option C: Manual**
```rust
// Admin trigger qua RPC hoặc config
POST /admin/next_epoch
```

**2. Committee Update**

**Cần:**
- Cơ chế để update committee (thêm/xóa nodes)
- Validate committee mới (quorum threshold, validity threshold)
- Generate keys cho nodes mới (nếu có)

**Ví dụ:**
```rust
fn create_next_epoch_committee(
    current_committee: &Committee,
    new_authorities: Vec<Authority>,  // Có thể thêm/xóa
) -> Result<Committee> {
    let next_epoch = current_committee.epoch() + 1;
    let new_committee = Committee::new(next_epoch, new_authorities);
    
    // Validate
    validate_committee(&new_committee)?;
    
    Ok(new_committee)
}
```

**3. State Migration**

**Vấn đề:**
- Consensus authority có state (DAG, commits, etc.)
- Cần quyết định: migrate hay reset?

**Option A: Reset State**
```rust
// Đơn giản nhất: Start fresh với epoch mới
fn transition_to_next_epoch_reset(committee: Committee) -> Result<()> {
    // Stop current authority
    authority.stop().await?;
    
    // Clear old state (optional)
    // clear_consensus_db()?;
    
    // Start với committee mới
    let new_authority = ConsensusAuthority::new(committee)?;
    new_authority.start().await?;
    
    Ok(())
}
```

**Option B: Migrate State**
```rust
// Phức tạp hơn: Migrate state từ epoch cũ
fn transition_to_next_epoch_migrate(
    old_committee: Committee,
    new_committee: Committee,
) -> Result<()> {
    // Stop current authority
    authority.stop().await?;
    
    // Migrate DAG state
    migrate_dag_state(&old_committee, &new_committee)?;
    
    // Migrate commit state
    migrate_commit_state(&old_committee, &new_committee)?;
    
    // Start với committee mới
    let new_authority = ConsensusAuthority::new(new_committee)?;
    new_authority.start().await?;
    
    Ok(())
}
```

**4. Consensus Authority Restart**

**Vấn đề:**
- `ConsensusAuthority` được start một lần trong `ConsensusNode::new()`
- Cần cơ chế để stop và restart với committee mới

**Giải pháp:**
```rust
// Option 1: Shutdown và restart node
// - Stop node
// - Update committee.json
// - Restart node

// Option 2: Hot reload committee
impl ConsensusAuthority {
    pub async fn transition_to_epoch(
        &self,
        new_committee: Committee,
        new_epoch_timestamp: u64,
    ) -> Result<()> {
        // Stop current consensus
        self.stop().await?;
        
        // Update context với committee mới
        let new_context = self.context.with_committee(new_committee)
            .with_epoch_start_timestamp_ms(new_epoch_timestamp);
        
        // Restart với context mới
        self.start_with_context(new_context).await?;
        
        Ok(())
    }
}
```

**5. Coordination giữa Nodes**

**Vấn đề:**
- Tất cả nodes phải transition cùng lúc
- Cần consensus về khi nào transition
- Cần đồng bộ committee mới

**Giải pháp:**

**Option A: External Coordination**
- Admin trigger transition cho tất cả nodes
- Hoặc dùng on-chain governance

**Option B: Built-in Consensus**
- Implement epoch change proposal trong consensus
- Nodes vote để transition
- Khi đạt quorum, tất cả transition

**Ví dụ:**
```rust
// Epoch change proposal
struct EpochChangeProposal {
    new_epoch: u64,
    new_committee: Committee,
    new_epoch_timestamp: u64,
}

// Nodes vote
fn propose_epoch_change(proposal: EpochChangeProposal) -> Result<()> {
    // Broadcast proposal
    // Collect votes
    // Khi đạt quorum, transition
}
```

## Implementation Plan: Next Epoch

### Phase 1: Basic Epoch Transition (Manual)

**Mục tiêu:** Cho phép manual transition sang epoch mới

**Steps:**

1. **Update Committee Generation:**
```rust
// Cho phép specify epoch
pub fn generate_committee_with_epoch(
    epoch: u64,
    count: usize,
) -> Result<(Committee, Vec<Keypairs>)> {
    // ... generate authorities ...
    let committee = Committee::new(epoch, authorities);  // ← Use epoch parameter
    Ok((committee, keypairs))
}
```

2. **Add Epoch Transition Command:**
```rust
// Trong main.rs
match args.command {
    Commands::NextEpoch { epoch, committee_path } => {
        // Load committee mới
        let new_committee = load_committee_from_file(committee_path)?;
        
        // Stop current node
        node.stop().await?;
        
        // Update config
        update_committee_config(&new_committee)?;
        update_epoch_timestamp()?;
        
        // Restart node
        // (Cần restart process)
    }
}
```

3. **Update Node to Support Epoch Change:**
```rust
impl ConsensusNode {
    pub async fn transition_to_epoch(
        &mut self,
        new_committee: Committee,
        new_epoch_timestamp: u64,
    ) -> Result<()> {
        // Stop authority
        self.authority.stop().await?;
        
        // Update context
        // ... update với committee mới ...
        
        // Restart authority
        // ... restart với context mới ...
        
        Ok(())
    }
}
```

### Phase 2: Automatic Epoch Transition

**Mục tiêu:** Tự động transition sau N commits hoặc N time

**Steps:**

1. **Add Epoch Transition Config:**
```rust
pub struct NodeConfig {
    // ...
    pub epoch_duration_seconds: Option<u64>,  // Time-based
    pub epoch_commits: Option<u32>,  // Block-based
    pub auto_epoch_transition: bool,  // Enable/disable
}
```

2. **Monitor và Trigger:**
```rust
// Trong node.rs
async fn monitor_epoch_transition(node: &ConsensusNode) -> Result<()> {
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        
        if should_transition_epoch(&node)? {
            let new_committee = generate_next_epoch_committee(&node)?;
            node.transition_to_epoch(new_committee).await?;
        }
    }
}
```

3. **Coordination:**
- Tất cả nodes phải transition cùng lúc
- Cần consensus mechanism hoặc external trigger

### Phase 3: Committee Update

**Mục tiêu:** Cho phép thêm/xóa nodes khi transition

**Steps:**

1. **Committee Update API:**
```rust
// RPC endpoint
POST /admin/update_committee
{
    "new_authorities": [...],
    "remove_authorities": [0, 1],  // Node IDs
    "add_authorities": [...]
}
```

2. **Validate và Generate:**
```rust
fn update_committee(
    current: &Committee,
    changes: CommitteeChanges,
) -> Result<Committee> {
    let mut new_authorities = current.authorities().clone();
    
    // Remove
    for idx in changes.remove_authorities {
        new_authorities.remove(idx);
    }
    
    // Add
    for auth in changes.add_authorities {
        new_authorities.push(auth);
    }
    
    // Validate
    validate_committee_size(&new_authorities)?;
    
    // Create new committee
    let next_epoch = current.epoch() + 1;
    Committee::new(next_epoch, new_authorities)
}
```

## Best Practices

### 1. Epoch Duration

**Khuyến nghị:**
- **Time-based:** 1-4 tuần (để rotate keys, reset reputation)
- **Block-based:** 10K-100K commits (tùy vào throughput)
- **Manual:** Khi cần (emergency, testing)

### 2. Committee Size

**Khuyến nghị:**
- **Minimum:** 4 nodes (fault tolerance = 1)
- **Recommended:** 7-21 nodes (fault tolerance = 2-7)
- **Maximum:** Tùy vào network capacity

### 3. State Migration

**Khuyến nghị:**
- **Reset state:** Đơn giản, phù hợp cho testing
- **Migrate state:** Phức tạp, cần cho production
- **Hybrid:** Migrate quan trọng, reset phần còn lại

### 4. Coordination

**Khuyến nghị:**
- **External trigger:** Admin trigger cho tất cả nodes
- **Consensus-based:** Nodes vote để transition
- **Time-based:** Tất cả nodes dùng cùng schedule

## Ví dụ: Manual Epoch Transition

### Step 1: Generate Committee mới

```bash
# Generate committee với epoch 1
cargo run --bin metanode -- generate-committee \
    --epoch 1 \
    --count 4 \
    --output config/committee_epoch1.json
```

### Step 2: Update Config

```bash
# Backup committee cũ
cp config/committee.json config/committee_epoch0.json

# Copy committee mới
cp config/committee_epoch1.json config/committee.json

# Update epoch timestamp
echo "$(date +%s)000" > config/epoch_timestamp.txt
```

### Step 3: Restart Nodes

```bash
# Stop tất cả nodes
pkill -f metanode

# Start lại với committee mới
cargo run --bin metanode -- start --node-id 0
cargo run --bin metanode -- start --node-id 1
# ...
```

## Tóm tắt

### Câu trả lời: Có thể triển khai, nhưng cần nhiều công việc

**✅ Đã có sẵn:**
- Committee structure với epoch
- Block epoch verification
- Epoch timestamp management

**⚠️ Cần triển khai:**
- Epoch transition trigger (time/block/manual)
- Committee update mechanism
- State migration (hoặc reset)
- Consensus authority restart với committee mới
- Coordination giữa nodes

**📋 Implementation Plan:**
1. **Phase 1:** Manual epoch transition (dễ nhất)
2. **Phase 2:** Automatic transition (cần monitoring)
3. **Phase 3:** Committee update (phức tạp nhất)

**🎯 Khuyến nghị:**
- Bắt đầu với **manual transition** (Phase 1)
- Test kỹ với 2-3 epochs
- Sau đó implement automatic transition nếu cần

## References

- [COMMITTEE.md](./COMMITTEE.md) - Chi tiết về committee structure
- [ARCHITECTURE.md](./ARCHITECTURE.md) - Kiến trúc consensus authority
- [CONFIGURATION.md](./CONFIGURATION.md) - Cấu hình epoch timestamp
- [EPOCH_PRODUCTION.md](./EPOCH_PRODUCTION.md) - **Best practices cho production** ⭐

