# FIX: Duplicate global_exec_index - Log Message và Validation

## 🔍 Vấn đề

Có duplicate `global_exec_index` từ 2 epochs khác nhau:
- Commit #1221 (epoch=1) có `global_exec_index=2498`
- Commit #2 (epoch=2) có `global_exec_index=2498` (duplicate!)

Điều này khiến Go Master skip commit thứ 2, dẫn đến transactions bị mất.

## ✅ Đã Sửa

### 1. Sửa Log Message (node.rs:1379-1395)

**Trước:**
```rust
info!(
    "    - Calculation: {} (old epoch) + {} (barrier commit) = {}",
    self.last_global_exec_index,
    last_commit_index,
    new_last_global_exec_index
);
```

**Sau:**
```rust
// Calculate expected result using correct formula for display
let expected_result = if old_epoch == 0 {
    last_commit_index as u64
} else {
    self.last_global_exec_index + last_commit_index as u64 + 1
};
info!(
    "    - Calculation: {} (last_global_exec_index) + {} (barrier commit_index) + {} (epoch offset) = {}",
    self.last_global_exec_index,
    last_commit_index,
    if old_epoch == 0 { 0 } else { 1 },
    expected_result
);
```

**Lý do:**
- Log message cũ hiển thị công thức SAI (chỉ cộng 2 số, không có +1 cho epoch offset)
- Log message mới hiển thị đúng công thức theo từng epoch:
  - Epoch 0: `global_exec_index = commit_index` (epoch offset = 0)
  - Epoch N: `global_exec_index = last_global_exec_index + commit_index + 1` (epoch offset = 1)

### 2. Thêm Validation (node.rs:1392-1395)

Thêm validation để phát hiện bug trong debug mode:

```rust
#[cfg(debug_assertions)]
{
    if new_last_global_exec_index != expected_result {
        warn!(
            "⚠️  BUG DETECTED: new_last_global_exec_index calculation mismatch! Expected {}, got {}. This may cause duplicate global_exec_index!",
            expected_result, new_last_global_exec_index
        );
    }
}
```

**Lý do:**
- Phát hiện sớm nếu có bug trong logic tính toán
- Chỉ chạy trong debug mode để tránh panic trong production

## 📊 Công Thức Đúng

### Epoch 0:
```
global_exec_index = commit_index
```

### Epoch N (N > 0):
```
global_exec_index = last_global_exec_index + commit_index + 1
```

### Khi Epoch Transition:
```
new_last_global_exec_index = calculate_global_exec_index(
    old_epoch,
    barrier_commit_index,
    old_last_global_exec_index
)
```

**Ví dụ:**
- Epoch 0 kết thúc tại commit_index=2497, `last_global_exec_index=2497`
- Epoch 1, commit_index=0: `global_exec_index = 2497 + 0 + 1 = 2498` ✓
- Epoch 1 kết thúc tại commit_index=1221, `new_last_global_exec_index = 2497 + 1221 + 1 = 3719`
- Epoch 2, commit_index=0: `global_exec_index = 3719 + 0 + 1 = 3720` ✓

## ⚠️ Lưu Ý

1. **Logic tính toán trong code là ĐÚNG** - công thức `calculate_global_exec_index` đúng
2. **Vấn đề duplicate có thể do:**
   - Commits past barrier được gửi với global_exec_index từ epoch cũ
   - Hoặc có race condition trong epoch transition
3. **Cần monitor log** để xem có warning về calculation mismatch không
4. **Nếu vẫn có duplicate**, cần kiểm tra:
   - Cách CommitProcessor được khởi tạo cho epoch mới
   - Cách `last_global_exec_index` được lưu và load từ committee.json
   - Timing của epoch transition và commit processing

## 🔧 Cần Làm Thêm (Nếu Vẫn Có Vấn Đề)

1. **Thêm logging chi tiết** khi CommitProcessor tính global_exec_index cho mỗi commit
2. **Kiểm tra committee.json** để xem `last_global_exec_index` có đúng không
3. **Đảm bảo commits past barrier không được gửi** hoặc được tính lại với epoch mới

