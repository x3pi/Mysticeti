# Sync Priority Fix: Ưu tiên đồng bộ khi node bị chậm

## Vấn đề

Khi node bị chậm (lagging behind), node vẫn tiếp tục tạo blocks mới (consensus) thay vì ưu tiên đồng bộ (sync) commits từ các node khác. Điều này làm cho node càng lag xa hơn.

## Giải pháp

Thêm cơ chế **ưu tiên sync khi lag**: Khi node bị lag quá nhiều, tạm dừng consensus (không tạo blocks mới) và ưu tiên sync commits từ peers.

## Các thay đổi

### 1. Thêm Lag Check trong `should_propose()`

**File**: `meta-consensus/core/src/core.rs`

**Logic**:
- Check lag bằng cách so sánh `local_commit_index` với `quorum_commit_index` (từ metrics)
- Skip propose khi lag > threshold:
  - **Moderate lag**: > 100 commits hoặc > 10% behind quorum → Skip consensus, prioritize sync
  - **Severe lag**: > 200 commits hoặc > 15% behind quorum → Aggressively skip consensus

**Code**:
```rust
// CRITICAL: Check if node is lagging and prioritize sync over consensus
let local_commit_index = self.dag_state.read().last_commit_index();
let quorum_commit_index = self.context.metrics.node_metrics.commit_sync_quorum_index.get() as u32;

const MODERATE_LAG_THRESHOLD: u32 = 100; // Skip consensus if lag > 100 commits
const SEVERE_LAG_THRESHOLD: u32 = 200; // Aggressively skip consensus if lag > 200 commits
const MODERATE_LAG_PERCENTAGE: f64 = 10.0; // Skip consensus if lag > 10% of quorum
const SEVERE_LAG_PERCENTAGE: f64 = 15.0; // Aggressively skip if lag > 15% of quorum

let lag = quorum_commit_index.saturating_sub(local_commit_index);
let lag_percentage = if quorum_commit_index > 0 {
    (lag as f64 / quorum_commit_index as f64) * 100.0
} else {
    0.0
};

// Skip consensus if lag is significant (prioritize sync)
let should_skip_consensus = lag > MODERATE_LAG_THRESHOLD || lag_percentage > MODERATE_LAG_PERCENTAGE;

if should_skip_consensus {
    // Skip proposing new blocks to focus on syncing commits
    return false;
}
```

### 2. Metrics Tracking

**File**: `meta-consensus/core/src/core.rs`

**Metrics labels**:
- `moderate_lag_prioritize_sync`: Khi skip consensus do moderate lag
- `severe_lag_prioritize_sync`: Khi skip consensus do severe lag

**Logs**:
- Debug log khi skip consensus do lag
- Hiển thị lag, lag_percentage, local_commit_index, quorum_commit_index

## Cách hoạt động

### Normal Mode (No Lag)
- Node tạo blocks bình thường
- Sync commits song song với consensus

### Moderate Lag Mode (Lag > 100 commits hoặc > 10%)
- **Skip consensus**: Không tạo blocks mới
- **Prioritize sync**: Tập trung sync commits từ peers
- Log: "Skip proposing for round X due to moderate lag: lag=Y commits..."

### Severe Lag Mode (Lag > 200 commits hoặc > 15%)
- **Aggressively skip consensus**: Hoàn toàn dừng tạo blocks
- **Aggressively prioritize sync**: Tập trung hoàn toàn vào sync
- Log: "Skip proposing for round X due to severe lag: lag=Y commits..."

### Recovery
- Khi lag giảm xuống dưới threshold, node tự động quay lại consensus bình thường
- Không cần manual intervention

## Lợi ích

1. **Faster catch-up**: Node tập trung sync thay vì tạo blocks mới khi lag
2. **Resource efficiency**: Tiết kiệm CPU/network cho consensus khi cần sync
3. **Automatic**: Tự động detect và recover, không cần manual intervention
4. **Observable**: Metrics và logs chi tiết để theo dõi

## Tương tác với Sync Mode

Cơ chế này hoạt động song song với **Sync Mode** trong `commit_syncer.rs`:

- **Sync Mode** (commit_syncer): Tăng tốc độ sync (batch size, parallel fetches)
- **Sync Priority** (core.rs): Tạm dừng consensus để ưu tiên sync

Khi cả hai được kích hoạt:
1. Commit syncer tăng tốc độ sync (batch size 2x, parallel fetches 1.5x)
2. Core skip consensus để tập trung resources vào sync
3. Node catch-up nhanh hơn

## Example Logs

### Khi node vào moderate lag mode:
```
Skip proposing for round 1500 due to moderate lag: lag=125 commits (12.5% behind quorum), local_commit=1375, quorum_commit=1500. Prioritizing sync over consensus.
```

### Khi node vào severe lag mode:
```
Skip proposing for round 2000 due to severe lag: lag=250 commits (16.7% behind quorum), local_commit=1750, quorum_commit=2000. Prioritizing sync over consensus.
```

### Khi node catch-up:
```
✅ [SYNC-MODE] Exiting sync mode: lag=25 commits (2.1% behind quorum), local_commit=1975, quorum_commit=2000
```
(Sau đó node tự động quay lại consensus bình thường)

## Configuration

Thresholds có thể được điều chỉnh trong code:
- `MODERATE_LAG_THRESHOLD`: 100 commits (có thể điều chỉnh)
- `SEVERE_LAG_THRESHOLD`: 200 commits (có thể điều chỉnh)
- `MODERATE_LAG_PERCENTAGE`: 10% (có thể điều chỉnh)
- `SEVERE_LAG_PERCENTAGE`: 15% (có thể điều chỉnh)

## Testing

Để test:
1. Tạm dừng một node (kill process)
2. Đợi network tạo nhiều commits
3. Restart node → node sẽ vào sync priority mode
4. Monitor logs để xem node skip consensus và prioritize sync
5. Verify node catch-up nhanh hơn

