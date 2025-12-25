# FIX: Skip Commits Past Barrier - Ngăn Duplicate Global Exec Index

## Vấn đề:

Commits past barrier từ epoch cũ có thể có `global_exec_index` duplicate với commits trong epoch mới:
1. **Commit past barrier từ epoch 0**: commit_index=1281 → `global_exec_index=1281` (epoch 0: global_exec_index = commit_index)
2. **Commit trong epoch 1**: commit_index=4 → `global_exec_index=1276+4+1=1281` (epoch 1: global_exec_index = last_global_exec_index + commit_index + 1) → **DUPLICATE!**

Go Master sẽ skip commit thứ hai vì duplicate, gây mất transaction.

## Giải pháp đơn giản:

**KHÔNG gửi commits past barrier từ epoch cũ đến Go Master.**

### Logic:

1. **Khi commit past barrier được xử lý:**
   - Kiểm tra barrier có còn > 0 không
   - Nếu barrier = 0 → epoch transition đã xảy ra → **skip commit hoàn toàn**
   - Nếu barrier > 0 → **vẫn skip commit** (không gửi đến Go Master) để tránh duplicate

2. **Fork-safety:**
   - Tất cả nodes sẽ skip commits past barrier từ epoch cũ (deterministic)
   - Transactions trong commits past barrier sẽ được retry trong epoch mới
   - Không có fork vì logic giống nhau ở tất cả nodes

### Code Changes:

```rust
if is_past_barrier {
    // Skip commit completely - don't send to Go Master
    // This prevents duplicate global_exec_index
    warn!("⚠️ [TX FLOW] Commit past barrier will be skipped (NOT sent to Go Master) to prevent duplicate global_exec_index");
    next_expected_index += 1;
    continue; // Skip commit
}
```

## Kết quả:

- ✅ Không duplicate `global_exec_index`
- ✅ Fork-safe (tất cả nodes skip commits past barrier)
- ✅ Logic đơn giản (skip commits past barrier hoàn toàn)
- ✅ Transactions sẽ được retry trong epoch mới

## Lưu ý:

- Transactions trong commits past barrier sẽ bị mất và cần retry
- Go-sub sẽ timeout và retry transaction tự động
- Không ảnh hưởng đến commits bình thường (trước barrier)

