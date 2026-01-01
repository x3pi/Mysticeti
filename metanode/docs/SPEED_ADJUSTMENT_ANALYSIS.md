# Phân tích Cơ chế Điều chỉnh Tốc độ Node

## Tổng quan

Tài liệu này phân tích cơ chế hiện tại để node tự động điều chỉnh tốc độ dựa trên performance so với mạng:
- **Node chậm**: Tăng tốc (không delay, ưu tiên sync)
- **Node nhanh**: Delay một chút để đồng bộ với tốc độ chung

---

## 1. Cơ chế hiện tại cho Node Chậm ✅

### 1.1 Phát hiện Node Chậm

**File**: `meta-consensus/core/src/commit_syncer.rs`

**Logic phát hiện lag**:
```rust
let lag = quorum_commit_index.saturating_sub(local_commit_index);
let lag_percentage = (lag as f64 / quorum_commit_index as f64) * 100.0;
```

**Thresholds**:
- **MODERATE_LAG_THRESHOLD**: 50 commits hoặc 5% behind quorum → Enter sync mode
- **SEVERE_LAG_THRESHOLD**: 200 commits hoặc 10% behind quorum → Aggressive sync mode

### 1.2 Adaptive Sync Mode (Khi Node Chậm)

Khi node vào sync mode, hệ thống tự động:

#### a. Tăng Batch Size
- **Moderate lag** (50-200 commits hoặc 5-10%): Batch size = **1.5x** base batch size
- **Severe lag** (>200 commits hoặc >10%): Batch size = **2x** base batch size

```rust
let effective_batch_size = if self.is_sync_mode {
    if lag > 200 || lag_percentage_for_batch > 10.0 {
        base_batch_size * 2 // Aggressive: 2x batch size
    } else {
        base_batch_size + base_batch_size / 2 // Moderate: 1.5x batch size
    }
} else {
    base_batch_size // Normal mode
};
```

#### b. Tăng Parallel Fetches
- Tăng parallelism lên **1.5x** base parallel fetches (capped at committee size)
- Cho phép nhiều fetches đồng thời hơn để catch-up nhanh hơn

#### c. Giảm Check Interval
- **Normal mode**: Check mỗi **2 giây**
- **Sync mode**: Check mỗi **1 giây** (2x faster response)

```rust
let base_interval = Duration::from_secs(2);
let fast_interval = Duration::from_secs(1);
let should_use_fast_interval = lag > 50 || lag_percentage > 5.0;
```

#### d. Tăng Unhandled Commits Threshold
- Cho phép nhiều unhandled commits hơn (**2x threshold**) để không block aggressive fetching

```rust
let effective_threshold = if self.is_sync_mode {
    unhandled_commits_threshold * 2 // Allow more unhandled commits in sync mode
} else {
    unhandled_commits_threshold
};
```

### 1.3 Skip Consensus khi Lag (Ưu tiên Sync)

**File**: `meta-consensus/core/src/core.rs`

**Logic**: Khi node lag quá nhiều, tạm dừng consensus (không tạo blocks mới) và ưu tiên sync:

```rust
// Skip consensus if lag > 100 commits or > 10% of quorum
const MODERATE_LAG_THRESHOLD: u32 = 100;
const MODERATE_LAG_PERCENTAGE: f64 = 10.0;

let should_skip_consensus = lag > MODERATE_LAG_THRESHOLD || lag_percentage > MODERATE_LAG_PERCENTAGE;

if should_skip_consensus {
    // Skip proposing new blocks to focus on syncing commits
    return false;
}
```

**Hysteresis**: Để tránh oscillation, khi lag giảm xuống, cần lag < 80% threshold mới resume consensus.

---

## 2. Cơ chế hiện tại cho Node Nhanh ❌

### 2.1 Speed Multiplier (Cố định)

**File**: `metanode/src/config.rs`, `metanode/src/node.rs`

**Hiện tại**: `speed_multiplier` là **cố định** trong config (mặc định: 0.2 = 5x slower)

```rust
let speed_multiplier = config.speed_multiplier; // Fixed value from config
if speed_multiplier != 1.0 {
    let leader_timeout = Duration::from_millis((200.0 / speed_multiplier) as u64);
    let min_round_delay = Duration::from_millis((50.0 / speed_multiplier) as u64);
    // ...
}
```

**Vấn đề**:
- ❌ Không tự động điều chỉnh dựa trên performance
- ❌ Không so sánh với tốc độ trung bình của mạng
- ❌ Tất cả nodes dùng cùng `speed_multiplier` (không adaptive)

### 2.2 Không có cơ chế Delay khi Node Nhanh

**Hiện tại**: 
- ❌ Không có logic phát hiện node nhanh hơn trung bình
- ❌ Không có cơ chế tự động delay khi node ahead of quorum
- ❌ Node nhanh sẽ tiếp tục tạo blocks nhanh, không đợi các node khác

---

## 3. Phân tích: Thiếu gì?

### 3.1 Metrics cần thiết

Để implement cơ chế adaptive delay cho node nhanh, cần:

1. **Quorum commit index**: ✅ Đã có (`commit_sync_quorum_index`)
2. **Local commit index**: ✅ Đã có (`last_commit_index()`)
3. **Lead (ahead)**: ❌ Chưa có - cần tính `local_commit_index - quorum_commit_index`
4. **Average network speed**: ❌ Chưa có - cần track tốc độ commit của quorum
5. **Node speed**: ❌ Chưa có - cần track tốc độ commit của node

### 3.2 Logic cần implement

**Khi node nhanh hơn trung bình**:
```rust
let lead = local_commit_index.saturating_sub(quorum_commit_index);
let lead_percentage = if quorum_commit_index > 0 {
    (lead as f64 / quorum_commit_index as f64) * 100.0
} else {
    0.0
};

// Nếu node ahead > threshold, delay một chút
if lead > LEAD_THRESHOLD || lead_percentage > LEAD_PERCENTAGE {
    // Tăng min_round_delay để chậm lại
    let adaptive_delay = calculate_adaptive_delay(lead, lead_percentage);
    parameters.min_round_delay += adaptive_delay;
}
```

**Khi node chậm hơn trung bình**:
```rust
// Đã có: giảm delay, tăng batch size, skip consensus
// ✅ Đã implement đầy đủ
```

---

## 4. Đề xuất Implementation

### 4.1 Thêm Metrics cho Lead

**File**: `meta-consensus/core/src/metrics.rs`

```rust
pub struct NodeMetrics {
    // ... existing metrics ...
    
    /// How many commits this node is ahead of quorum (negative = lagging)
    pub commit_sync_lead: IntGauge,
    
    /// Average commit rate of quorum (commits per second)
    pub quorum_commit_rate: Gauge,
    
    /// Average commit rate of this node (commits per second)
    pub local_commit_rate: Gauge,
}
```

### 4.2 Adaptive Delay Logic

**File**: `meta-consensus/core/src/core.rs` hoặc `metanode/src/node.rs`

```rust
fn calculate_adaptive_delay(
    lead: u32,
    lead_percentage: f64,
    base_min_round_delay: Duration,
) -> Duration {
    const MODERATE_LEAD_THRESHOLD: u32 = 50; // Ahead by 50 commits
    const SEVERE_LEAD_THRESHOLD: u32 = 100; // Ahead by 100 commits
    const MODERATE_LEAD_PERCENTAGE: f64 = 5.0; // Ahead by 5%
    const SEVERE_LEAD_PERCENTAGE: f64 = 10.0; // Ahead by 10%
    
    // Nếu node ahead quá nhiều, delay nhiều hơn
    if lead > SEVERE_LEAD_THRESHOLD || lead_percentage > SEVERE_LEAD_PERCENTAGE {
        // Severe lead: delay 2x base delay
        base_min_round_delay * 2
    } else if lead > MODERATE_LEAD_THRESHOLD || lead_percentage > MODERATE_LEAD_PERCENTAGE {
        // Moderate lead: delay 1.5x base delay
        base_min_round_delay + base_min_round_delay / 2
    } else {
        // Normal: no extra delay
        Duration::ZERO
    }
}
```

### 4.3 Apply trong Consensus Loop

**File**: `meta-consensus/core/src/core.rs`

```rust
pub(crate) fn should_propose(&self) -> bool {
    // ... existing lag check ...
    
    // NEW: Check if node is ahead and apply adaptive delay
    let lead = local_commit_index.saturating_sub(quorum_commit_index);
    let lead_percentage = if quorum_commit_index > 0 {
        (lead as f64 / quorum_commit_index as f64) * 100.0
    } else {
        0.0
    };
    
    // If node is ahead, add adaptive delay before proposing
    if lead > MODERATE_LEAD_THRESHOLD || lead_percentage > MODERATE_LEAD_PERCENTAGE {
        // Apply adaptive delay (implement in propose logic)
        // This will slow down the node to match network speed
    }
    
    // ... rest of logic ...
}
```

### 4.4 Track Commit Rate

**File**: `meta-consensus/core/src/commit_syncer.rs`

```rust
// Track commit rate over sliding window (e.g., last 10 seconds)
struct CommitRateTracker {
    commits: VecDeque<(Instant, CommitIndex)>,
    window_duration: Duration,
}

impl CommitRateTracker {
    fn update(&mut self, commit_index: CommitIndex) {
        let now = Instant::now();
        self.commits.push_back((now, commit_index));
        
        // Remove old entries outside window
        while let Some((time, _)) = self.commits.front() {
            if now.duration_since(*time) > self.window_duration {
                self.commits.pop_front();
            } else {
                break;
            }
        }
    }
    
    fn rate(&self) -> f64 {
        if self.commits.len() < 2 {
            return 0.0;
        }
        let (first_time, first_index) = self.commits.front().unwrap();
        let (last_time, last_index) = self.commits.back().unwrap();
        
        let duration = last_time.duration_since(*first_time).as_secs_f64();
        if duration > 0.0 {
            ((*last_index - *first_index) as f64) / duration
        } else {
            0.0
        }
    }
}
```

---

## 5. Tóm tắt

### ✅ Đã có (Node Chậm)
1. Phát hiện lag (50 commits hoặc 5%)
2. Adaptive sync mode (tăng batch size, parallel fetches)
3. Giảm check interval (1s thay vì 2s)
4. Skip consensus khi lag quá nhiều (ưu tiên sync)

### ❌ Chưa có (Node Nhanh)
1. Phát hiện lead (ahead of quorum)
2. Adaptive delay khi node nhanh hơn trung bình
3. Track commit rate để so sánh với network
4. Tự động điều chỉnh `min_round_delay` dựa trên lead

### 📋 Cần implement
1. Thêm metrics cho lead và commit rate
2. Implement `calculate_adaptive_delay()` function
3. Apply adaptive delay trong consensus loop
4. Track commit rate với sliding window
5. Logging và monitoring cho adaptive delay

---

## 6. Lợi ích khi implement

1. **Đồng bộ tốt hơn**: Node nhanh sẽ chậm lại để đợi các node khác
2. **Giảm fork risk**: Tất cả nodes đồng bộ tốc độ → ít fork hơn
3. **Tự động điều chỉnh**: Không cần manual tuning `speed_multiplier`
4. **Performance tốt hơn**: Node chậm tăng tốc, node nhanh chậm lại → cân bằng

---

## 7. Next Steps

1. **Phase 1**: Thêm metrics cho lead và commit rate
2. **Phase 2**: Implement `calculate_adaptive_delay()`
3. **Phase 3**: Apply trong consensus loop
4. **Phase 4**: Testing và tuning thresholds
5. **Phase 5**: Monitoring và logging

