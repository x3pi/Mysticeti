# Logic Quorum cho Epoch Transition

## Công thức Quorum

### Với 4 Nodes (mỗi node có stake = 1)

```
total_stake = 4
f = (total_stake - 1) / 3 = (4 - 1) / 3 = 1
quorum_threshold = total_stake - f = 4 - 1 = 3
```

**Kết luận:** Cần **3/4 votes** để đạt quorum (không phải 4/4)

### Tại sao không cần 4/4?

- **Byzantine Fault Tolerance (BFT)**: Hệ thống được thiết kế để chịu được `f` nodes lỗi
- Với 4 nodes: `f = 1` (có thể chịu 1 node lỗi)
- Quorum = `2f + 1 = 3` đảm bảo:
  - **Safety**: Không thể có 2 quorum khác nhau cùng lúc
  - **Liveness**: Vẫn hoạt động được khi 1 node offline

## Vấn đề khi tắt 1 Node

### Scenario: 4 Nodes → Tắt 1 Node → Còn 3 Nodes Online

```
total_stake = 4 (vẫn tính từ committee, không phải nodes online)
quorum_threshold = 3 (vẫn tính từ total_stake = 4)
nodes_online = 3
stake_online = 3 (mỗi node có stake = 1)
```

**Vấn đề:**
- Quorum threshold vẫn = **3** (tính từ `total_stake = 4`)
- Nhưng chỉ có **3 nodes online**
- Cần **3 votes** từ **3 nodes online**
- → **Cần 100% nodes online phải vote** (3/3)

### Tại sao không chuyển đổi được?

1. **Nếu 1 node offline không vote:**
   - Chỉ có 2 nodes vote
   - `approve_stake = 2 < quorum_threshold = 3`
   - → **Không đạt quorum**

2. **Nếu 1 node online nhưng không vote (hoặc vote NO):**
   - Chỉ có 2 nodes vote YES
   - `approve_stake = 2 < quorum_threshold = 3`
   - → **Không đạt quorum**

3. **Cần tất cả 3 nodes online phải vote YES:**
   - `approve_stake = 3 >= quorum_threshold = 3`
   - → **Đạt quorum**

## So sánh: 4 Nodes vs 3 Nodes Online

| Scenario | Total Stake | Quorum Threshold | Nodes Online | Cần Votes | % Nodes Online |
|----------|-------------|------------------|--------------|-----------|----------------|
| **4 nodes (tất cả online)** | 4 | 3 | 4 | 3/4 | 75% |
| **3 nodes online (1 offline)** | 4 | 3 | 3 | 3/3 | **100%** |

## Giải pháp

### Option 1: Đảm bảo tất cả nodes online vote
- Tất cả 3 nodes online phải vote YES
- Không có node nào offline hoặc không vote

### Option 2: Thay đổi committee (không khuyến nghị)
- Giảm `total_stake` xuống 3 khi 1 node offline
- Nhưng điều này yêu cầu epoch transition trước (catch-22)

### Option 3: Chấp nhận rủi ro (không khuyến nghị)
- Giảm quorum threshold khi có node offline
- Nhưng mất tính an toàn của BFT

## Code Reference

### Tính quorum threshold:
```rust
// meta-consensus/config/src/committee.rs
let fault_tolerance = (total_stake - 1) / 3;
let quorum_threshold = total_stake - fault_tolerance;
```

### Check quorum:
```rust
// src/epoch_change.rs
let approve_stake: Stake = votes
    .iter()
    .filter(|v| v.approve)
    .map(|v| self.committee.authority(v.voter()).stake)
    .sum();

if approve_stake >= quorum_threshold {
    return Some(true); // Quorum reached
}
```

## Kết luận

**Với 4 nodes:**
- ✅ Cần **3/4 votes** (75% nodes) khi tất cả online
- ❌ Cần **3/3 votes** (100% nodes) khi 1 node offline
- → **Khi tắt 1 node, cần 100% nodes online phải vote để chuyển epoch**

**Lý do:**
- Quorum threshold được tính từ `total_stake` của committee (4)
- Không phải từ số nodes online
- Đảm bảo tính an toàn của Byzantine Fault Tolerance

