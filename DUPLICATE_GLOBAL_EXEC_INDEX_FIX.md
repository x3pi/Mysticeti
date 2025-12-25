# FIX: Duplicate Global Exec Index - Đơn giản và Fork-Safe

## Vấn đề:

Commits past barrier từ epoch cũ có thể có `global_exec_index` duplicate với commits trong epoch mới:
1. Commit past barrier (epoch=1, commit_index=1223): `global_exec_index=2505` (tính từ epoch 1)
2. Commit trong epoch mới (epoch=2, commit_index=3): `global_exec_index=2505` (tính từ epoch 2) → **DUPLICATE!**

Go Master sẽ skip commit thứ hai vì duplicate, gây mất transaction.

## Giải pháp đơn giản:

**Khi barrier = 0 (epoch transition đã xảy ra), skip commits past barrier từ epoch cũ.**

### Logic:

1. **Khi commit past barrier được xử lý:**
   - Kiểm tra barrier có còn > 0 không
   - Nếu barrier = 0 → epoch transition đã xảy ra → **skip commit này**
   - Nếu barrier > 0 → gửi empty commit như bình thường

2. **Fork-safety:**
   - Tất cả nodes sẽ thấy barrier = 0 sau epoch transition (deterministic)
   - Tất cả nodes sẽ skip commits past barrier từ epoch cũ (deterministic)
   - Không có fork vì logic giống nhau ở tất cả nodes

### Code Changes:

```rust
// Double-check barrier is still set (epoch transition may have reset it)
let barrier_still_set = if let Some(barrier) = transition_barrier.as_ref() {
    barrier.load(Ordering::Relaxed) > 0
} else {
    false
};

// If barrier was reset to 0, epoch transition happened - skip this commit
if !barrier_still_set {
    warn!("⏭️ [FORK-SAFETY] Skipping commit past barrier from old epoch - epoch transition already happened");
    next_expected_index += 1;
    continue; // Skip commit
}
```

## Kết quả:

- ✅ Không duplicate `global_exec_index`
- ✅ Fork-safe (tất cả nodes skip commits cũ)
- ✅ Logic đơn giản (chỉ check barrier = 0)
- ✅ Không ảnh hưởng đến commits bình thường

## Test:

Sau khi rebuild, transaction sẽ không bị mất do duplicate `global_exec_index` nữa.

