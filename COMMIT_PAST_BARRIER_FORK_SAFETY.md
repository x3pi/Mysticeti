# Fork-Safety Analysis: Commits Past Barrier

## Tóm tắt

Logic hiện tại gửi empty commit cho commits past barrier là **FORK-SAFE** vì:
1. **Deterministic**: Tất cả nodes tính cùng `global_exec_index` cho cùng commit
2. **Go Master handles duplicates**: Go Master sẽ skip commits với `global_exec_index < nextExpectedGlobalExecIndex`
3. **No gap**: Empty commit được gửi, Go Master cập nhật `nextExpectedGlobalExecIndex` đúng

## Kịch bản duplicate global_exec_index

### Setup
- **Epoch 6:**
  - `last_global_exec_index = 7366`
  - Barrier = 1219
  - `new_last_global_exec_index = 7366 + 1219 + 1 = 8586`

### Commits past barrier (epoch=6):
- Commit #1220: `global_exec_index = 7366 + 1220 + 1 = 8587`
- Commit #1221: `global_exec_index = 7366 + 1221 + 1 = 8588`

### Commits trong epoch mới (epoch=7):
- Commit #0: `global_exec_index = 8586 + 0 + 1 = 8587` ❌ **DUPLICATE** với #1220
- Commit #1: `global_exec_index = 8586 + 1 + 1 = 8588` ❌ **DUPLICATE** với #1221

## Giải pháp: Go Master xử lý duplicate

### Logic Go Master (block_processor.go):

```go
if globalExecIndex == nextExpectedGlobalExecIndex {
    // Xử lý commit
    nextExpectedGlobalExecIndex = globalExecIndex + 1
} else if globalExecIndex > nextExpectedGlobalExecIndex {
    // Out-of-order: Lưu vào pendingBlocks
    pendingBlocks[globalExecIndex] = epochData
} else {
    // Duplicate/old: SKIP
    logger.Warn("⚠️ [FORK-SAFETY] Received block with global_exec_index=%d which is less than expected %d, skipping",
        globalExecIndex, nextExpectedGlobalExecIndex)
    continue
}
```

### Kịch bản 1: Commit past barrier đến trước

1. **Commit #1220** (epoch=6, `global_exec_index=8587`, empty) đến Go Master
   - `nextExpectedGlobalExecIndex = 8587` → **XỬ LÝ** ✅
   - `nextExpectedGlobalExecIndex = 8588`

2. **Commit #0** (epoch=7, `global_exec_index=8587`) đến Go Master
   - `global_exec_index=8587 < nextExpectedGlobalExecIndex=8588` → **SKIP** ✅

**Kết quả:** Commit past barrier được xử lý, commit duplicate bị skip.

### Kịch bản 2: Commit epoch mới đến trước

1. **Commit #0** (epoch=7, `global_exec_index=8587`) đến Go Master
   - `nextExpectedGlobalExecIndex = 8587` → **XỬ LÝ** ✅
   - `nextExpectedGlobalExecIndex = 8588`

2. **Commit #1220** (epoch=6, `global_exec_index=8587`, empty) đến Go Master
   - `global_exec_index=8587 < nextExpectedGlobalExecIndex=8588` → **SKIP** ✅

**Kết quả:** Commit epoch mới được xử lý, commit duplicate (past barrier) bị skip.

## Fork-Safety Guarantees

### 1. Deterministic Calculation
- Tất cả nodes tính cùng `global_exec_index` cho cùng commit (same epoch, commit_index, last_global_exec_index)
- Formula: `calculate_global_exec_index(epoch, commit_index, last_global_exec_index)`

### 2. Deterministic Empty Commit
- Tất cả nodes gửi cùng empty commit cho commits past barrier
- Same epoch, same commit_index → same `global_exec_index`

### 3. Deterministic Skip Logic
- Tất cả nodes có cùng logic skip duplicate (`global_exec_index < nextExpectedGlobalExecIndex`)
- Commit nào đến trước với `global_exec_index` đúng sẽ được xử lý
- Commit duplicate sẽ bị skip

### 4. Sequential Processing
- `nextExpectedGlobalExecIndex` đảm bảo xử lý tuần tự
- Out-of-order commits được lưu vào `pendingBlocks` và xử lý sau

## Kết luận

✅ **FORK-SAFE**: 
- Tất cả nodes có cùng logic và quyết định giống nhau
- Duplicate commits được xử lý deterministically (skip commit đến sau)

✅ **NO GAP**:
- Empty commit được gửi → Go Master cập nhật `nextExpectedGlobalExecIndex` đúng
- Không còn chờ commit mãi mãi

✅ **SIMPLE & EFFECTIVE**:
- Logic đơn giản: gửi empty commit, để Go Master xử lý duplicate
- Không cần logic phức tạp để tránh duplicate

