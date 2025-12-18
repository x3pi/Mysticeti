# Thời Gian Chuyển Đổi Epoch

## Tổng quan

Thời gian chuyển đổi epoch phụ thuộc vào nhiều yếu tố:
1. **Epoch Duration**: Thời gian một epoch kéo dài (time-based trigger)
2. **Proposal & Voting**: Thời gian để proposal được broadcast và đạt quorum
3. **Commit Index Barrier**: Thời gian đợi đến commit index được chỉ định

## Các Giai Đoạn

### 1. Epoch Duration (Time-based Trigger)

**Cấu hình:** `epoch_duration_seconds` trong `node_*.toml`

```toml
# Ví dụ: 24 giờ
epoch_duration_seconds = 86400  # 24 * 60 * 60
```

**Thời gian:** 
- **Mặc định:** Disabled (None) - không tự động trigger
- **Khi enabled:** Sau `epoch_duration_seconds` giây từ epoch start

**Ví dụ:**
- `epoch_duration_seconds = 86400` → Propose sau **24 giờ**
- `epoch_duration_seconds = 3600` → Propose sau **1 giờ**

### 2. Proposal & Voting Phase

**Thời gian:** Phụ thuộc vào consensus speed và network

**Các bước:**
1. **Propose**: Authority tạo proposal và include trong block
2. **Broadcast**: Block được broadcast đến tất cả nodes
3. **Vote**: Nodes nhận proposal, validate, và vote
4. **Quorum**: Đạt 2f+1 votes (quorum threshold)

**Thời gian ước tính:**

Với `speed_multiplier = 0.05` (20x slower):
- `leader_timeout = 4s` (200ms * 20)
- `min_round_delay = 1s` (50ms * 20)
- Mỗi round: ~4-5 giây
- Mỗi commit: ~1-2 rounds

**Ước tính:**
- **Proposal broadcast**: 1-2 rounds = **5-10 giây**
- **Voting phase**: 2-4 rounds = **10-20 giây**
- **Quorum đạt**: **15-30 giây** (tổng)

Với `speed_multiplier = 1.0` (normal speed):
- `leader_timeout = 200ms`
- `min_round_delay = 50ms`
- Mỗi round: ~200-300ms
- **Quorum đạt**: **1-3 giây**

### 3. Commit Index Barrier

**Cơ chế:** Đợi đến commit index được chỉ định để đảm bảo fork-safe

**Implementation:**
```rust
let transition_commit_index = proposal.proposal_commit_index.saturating_add(10);
// Transition chỉ khi current_commit_index >= transition_commit_index
```

**Thời gian:** Phụ thuộc vào commit rate

**Commit rate phụ thuộc vào:**
- Consensus speed (`speed_multiplier`)
- Network latency
- Transaction load
- Number of authorities

**Ước tính:**

Với `speed_multiplier = 0.05`:
- Mỗi commit: ~5-10 giây
- 10 commits buffer: **50-100 giây**

Với `speed_multiplier = 1.0`:
- Mỗi commit: ~200-500ms
- 10 commits buffer: **2-5 giây**

## Tổng Thời Gian Chuyển Đổi

### Scenario 1: Time-based với speed_multiplier = 0.05

```
1. Epoch Duration: 86400 giây (24h)
   ↓
2. Proposal & Voting: 15-30 giây
   ↓
3. Commit Index Barrier: 50-100 giây
   ↓
Tổng: 24h + 1-2 phút
```

**Kết quả:** Epoch change xảy ra sau **~24 giờ + 1-2 phút**

### Scenario 2: Time-based với speed_multiplier = 1.0

```
1. Epoch Duration: 86400 giây (24h)
   ↓
2. Proposal & Voting: 1-3 giây
   ↓
3. Commit Index Barrier: 2-5 giây
   ↓
Tổng: 24h + 3-8 giây
```

**Kết quả:** Epoch change xảy ra sau **~24 giờ + vài giây**

### Scenario 3: Manual Proposal (không có time-based)

```
1. Manual trigger: 0 giây (ngay lập tức)
   ↓
2. Proposal & Voting: 15-30 giây (với speed_multiplier = 0.05)
   ↓
3. Commit Index Barrier: 50-100 giây
   ↓
Tổng: 1-2 phút
```

**Kết quả:** Epoch change xảy ra sau **1-2 phút** (nếu manual trigger)

## Các Yếu Tố Ảnh Hưởng

### 1. Speed Multiplier

| Speed Multiplier | Round Time | Commit Time | Voting Phase | Barrier Phase |
|------------------|------------|-------------|--------------|---------------|
| 0.05 (20x slower) | 4-5s | 5-10s | 15-30s | 50-100s |
| 0.1 (10x slower) | 2-2.5s | 2-5s | 6-15s | 20-50s |
| 1.0 (normal) | 200-300ms | 200-500ms | 1-3s | 2-5s |

### 2. Network Conditions

- **Low latency**: Nhanh hơn
- **High latency**: Chậm hơn
- **Network partition**: Có thể delay hoặc timeout

### 3. Transaction Load

- **High load**: Có thể chậm hơn do blocks lớn
- **Low load**: Nhanh hơn

### 4. Number of Authorities

- **More authorities**: Cần nhiều votes hơn → lâu hơn
- **Fewer authorities**: Nhanh hơn

## Cấu Hình Hiện Tại

Dựa trên `config/node_0.toml`:

```toml
speed_multiplier = 0.05  # 20x slower
# epoch_duration_seconds không được set → Disabled
```

**Kết quả:**
- **Time-based trigger**: Disabled (không tự động)
- **Manual proposal**: 1-2 phút để transition
- **Nếu enable time-based**: 24h + 1-2 phút

## Khuyến Nghị

### Cho Production

1. **Enable time-based epoch change:**
   ```toml
   time_based_epoch_change = true
   epoch_duration_seconds = 86400  # 24 hours
   ```

2. **Set speed_multiplier phù hợp:**
   - **Development/Testing**: 0.05 (20x slower) để dễ debug
   - **Production**: 1.0 (normal speed) để nhanh hơn

3. **Monitor commit rate:**
   - Theo dõi số commits/giây
   - Điều chỉnh `proposal_commit_index` buffer nếu cần

### Tính Toán Thời Gian

**Công thức:**
```
Total Time = Epoch Duration + Voting Time + Barrier Time

Với speed_multiplier = 0.05:
- Voting Time = 15-30 giây
- Barrier Time = 50-100 giây
- Total = Epoch Duration + 1-2 phút

Với speed_multiplier = 1.0:
- Voting Time = 1-3 giây
- Barrier Time = 2-5 giây
- Total = Epoch Duration + 3-8 giây
```

## Ví Dụ Cụ Thể

### Ví dụ 1: 24h Epoch với Normal Speed

```toml
time_based_epoch_change = true
epoch_duration_seconds = 86400
speed_multiplier = 1.0
```

**Timeline:**
- T0: Epoch start
- T0 + 24h: Time-based trigger → Propose
- T0 + 24h + 1-3s: Quorum đạt
- T0 + 24h + 3-8s: Commit index barrier passed → **Transition**

**Kết quả:** Epoch change sau **24 giờ + 3-8 giây**

### Ví dụ 2: 1h Epoch với Slow Speed

```toml
time_based_epoch_change = true
epoch_duration_seconds = 3600
speed_multiplier = 0.05
```

**Timeline:**
- T0: Epoch start
- T0 + 1h: Time-based trigger → Propose
- T0 + 1h + 15-30s: Quorum đạt
- T0 + 1h + 1-2min: Commit index barrier passed → **Transition**

**Kết quả:** Epoch change sau **1 giờ + 1-2 phút**

## Kết Luận

**Thời gian chuyển đổi epoch:**

1. **Time-based (enabled):**
   - **Epoch duration** (config) + **1-2 phút** (voting + barrier)
   - Ví dụ: 24h epoch → **24h + 1-2 phút**

2. **Manual proposal:**
   - **1-2 phút** (voting + barrier)
   - Không phụ thuộc epoch duration

3. **Phụ thuộc vào:**
   - `speed_multiplier`: Quyết định consensus speed
   - `epoch_duration_seconds`: Thời gian epoch (nếu enabled)
   - Network conditions: Latency, load
   - Commit rate: Số commits/giây

**Khuyến nghị:** Enable time-based với `epoch_duration_seconds = 86400` (24h) cho production.

