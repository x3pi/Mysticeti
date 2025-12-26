# Cải thiện: Phát hiện Node Chậm và Chế Độ Đồng Bộ Tự Động

## Tổng quan

Đã bổ sung logic phát hiện node chậm và tự động chuyển sang chế độ đồng bộ (sync mode) khi node lag quá xa so với quorum. Hệ thống sẽ tự động tăng tốc độ sync để catch-up nhanh hơn.

## Các cải thiện đã implement

### 1. Phát hiện Node Chậm với Threshold Động

**File**: `metanode/meta-consensus/core/src/commit_syncer.rs`

**Thay đổi**:
- Thêm các threshold để phát hiện lag:
  - **MODERATE_LAG_THRESHOLD**: 50 commits hoặc 5% behind quorum → Enter sync mode
  - **SEVERE_LAG_THRESHOLD**: 200 commits hoặc 10% behind quorum → Aggressive sync mode

**Logic**:
```rust
let should_be_in_sync_mode = lag > MODERATE_LAG_THRESHOLD || lag_percentage > MODERATE_LAG_PERCENTAGE;
let is_severe_lag = lag > SEVERE_LAG_THRESHOLD || lag_percentage > SEVERE_LAG_PERCENTAGE;
```

### 2. Adaptive Sync Mode

**Khi node vào sync mode**, hệ thống tự động:

#### a. Tăng Batch Size
- **Moderate lag** (50-200 commits hoặc 5-10%): Batch size = 1.5x base batch size
- **Severe lag** (>200 commits hoặc >10%): Batch size = 2x base batch size

#### b. Tăng Parallel Fetches
- Tăng parallelism lên 1.5x base parallel fetches (capped at committee size)
- Cho phép nhiều fetches đồng thời hơn để catch-up nhanh hơn

#### c. Giảm Check Interval
- **Normal mode**: Check mỗi 2 giây
- **Sync mode**: Check mỗi 1 giây (2x faster response)

#### d. Tăng Unhandled Commits Threshold
- Cho phép nhiều unhandled commits hơn (2x threshold) để không block aggressive fetching

### 3. Logging Chi Tiết

**Logs khi chuyển mode**:
- `🔄 [SYNC-MODE] Entering sync mode`: Khi node vào sync mode
- `✅ [SYNC-MODE] Exiting sync mode`: Khi node thoát sync mode
- `⚡ [SYNC-MODE] Switching to fast sync interval`: Khi chuyển sang interval 1s
- `✅ [SYNC-MODE] Switching back to normal interval`: Khi chuyển về interval 2s

**Logs khi lag**:
- `⚠️  [LAG-DETECTION] Node is lagging significantly`: Moderate lag
- `🚨 [LAG-DETECTION] Node is severely lagging`: Severe lag

### 4. State Tracking

**Thêm state variables**:
- `is_sync_mode: bool`: Track xem node có đang trong sync mode không
- `last_sync_mode_log_at: Instant`: Throttle logging để tránh spam

## Cách hoạt động

### Normal Mode (No Lag)
- Check interval: 2 giây
- Batch size: Base batch size (từ config)
- Parallel fetches: Base parallel fetches (từ config)
- Unhandled threshold: Base threshold

### Sync Mode (Lag > 50 commits hoặc > 5%)
- Check interval: 1 giây (2x faster)
- Batch size: 1.5x base (moderate) hoặc 2x base (severe)
- Parallel fetches: 1.5x base (capped at committee size)
- Unhandled threshold: 2x base (more lenient)

### Severe Lag Mode (Lag > 200 commits hoặc > 10%)
- Tất cả optimizations của sync mode
- Batch size: 2x base (most aggressive)
- Error-level logging để alert

## Lợi ích

1. **Tự động phát hiện**: Node tự động phát hiện khi bị lag và chuyển sang sync mode
2. **Catch-up nhanh hơn**: Tăng tốc độ sync khi lag lớn
3. **Adaptive**: Tự động điều chỉnh parameters dựa trên mức độ lag
4. **Observable**: Logs chi tiết để theo dõi sync progress
5. **Không ảnh hưởng normal operation**: Chỉ tăng tốc khi cần thiết

## Example Logs

### Khi node vào sync mode:
```
⚠️  [LAG-DETECTION] Node is lagging significantly: lag=75 commits (6.2% behind quorum), local_commit=500, quorum_commit=575, synced_commit=500
🔄 [SYNC-MODE] Entering sync mode: lag=75 commits (6.2% behind quorum), local_commit=500, quorum_commit=575, synced_commit=500
⚡ [SYNC-MODE] Switching to fast sync interval (1s) due to lag=75
```

### Khi node catch-up:
```
✅ [SYNC-MODE] Exiting sync mode: lag=25 commits (2.1% behind quorum), local_commit=550, quorum_commit=575
✅ [SYNC-MODE] Switching back to normal interval (2s) - lag reduced to 25
```

### Khi severe lag:
```
🚨 [LAG-DETECTION] Node is severely lagging: lag=250 commits (12.5% behind quorum), local_commit=500, quorum_commit=750, synced_commit=500
🔄 [SYNC-MODE] Entering sync mode: lag=250 commits (12.5% behind quorum), local_commit=500, quorum_commit=750, synced_commit=500
⚡ [SYNC-MODE] Switching to fast sync interval (1s) due to lag=250
```

## Configuration

Các parameters có thể config trong `config/node_X.toml`:
- `commit_sync_batch_size`: Base batch size (default: 200)
- `commit_sync_parallel_fetches`: Base parallel fetches (default: 16)
- `commit_sync_batches_ahead`: Batches ahead (default: 64)

Sync mode sẽ tự động scale các parameters này dựa trên lag.

## Testing

Để test sync mode:
1. Tạm dừng một node (node_0)
2. Để các nodes khác chạy và tạo commits
3. Restart node_0
4. Quan sát logs để thấy node_0 tự động vào sync mode và catch-up

## Next Steps (Optional)

1. **Metrics**: Thêm metrics để track sync mode duration, catch-up speed
2. **Alerting**: Alert khi node lag quá lâu (>5 phút trong sync mode)
3. **Tuning**: Fine-tune thresholds dựa trên production data

