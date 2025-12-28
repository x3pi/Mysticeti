# Commit Processor Stuck Fix

## Vấn đề

Node 0 bị stuck: Commit processor dừng xử lý commits sau commit #50242, trong khi consensus layer vẫn tạo commits đến #168144. Gap: **117,902 commits chưa được xử lý**.

### Nguyên nhân

1. **Executor client send bị stuck**: `send_committed_subdag()` không có timeout, có thể bị stuck nếu Go executor không phản hồi
2. **Không có monitoring**: Không có cơ chế detect khi commit processor bị stuck
3. **Không có auto-recovery**: Khi commit processor stuck, không có cơ chế tự động recover

## Giải pháp

### 1. Thêm Timeout cho Executor Client Send

**File**: `src/executor_client.rs`

- Thêm timeout 10 giây cho `send_committed_subdag()`
- Nếu timeout, đóng connection và skip send (không fail commit)
- Retry cũng có timeout

```rust
const SEND_TIMEOUT: Duration = Duration::from_secs(10); // 10 seconds timeout

let send_result = timeout(SEND_TIMEOUT, async {
    stream.write_all(&len_buf).await?;
    stream.write_all(&epoch_data_bytes).await?;
    stream.flush().await?;
    Ok::<(), std::io::Error>(())
}).await;
```

### 2. Thêm Heartbeat Monitoring

**File**: `src/commit_processor.rs`

- Log heartbeat mỗi 1000 commits
- Detect stuck: Nếu không có progress trong 5 phút, log warning
- Log final stats khi receiver đóng

```rust
const HEARTBEAT_INTERVAL: u32 = 1000; // Log every 1000 commits
const HEARTBEAT_TIMEOUT_SECS: u64 = 300; // 5 minutes timeout

if commit_index >= last_heartbeat_commit + HEARTBEAT_INTERVAL {
    info!("💓 [COMMIT PROCESSOR HEARTBEAT] Processed {} commits...", commit_index);
    last_heartbeat_commit = commit_index;
    last_heartbeat_time = std::time::Instant::now();
}
```

### 3. Cải thiện Error Handling

**File**: `src/node.rs`

- Thêm log khi spawn commit processor
- Log khi commit processor exit (bình thường hoặc lỗi)
- Dễ dàng debug vấn đề

```rust
info!("🚀 [COMMIT PROCESSOR] Starting commit processor for node {}...", node_id);
match commit_processor.run().await {
    Ok(()) => info!("✅ [COMMIT PROCESSOR] Commit processor exited normally"),
    Err(e) => error!("❌ [COMMIT PROCESSOR] Commit processor error: {}", e),
}
```

## Cách sử dụng

### 1. Rebuild

```bash
cd /home/abc/chain-new/Mysticeti/metanode
cargo build --release --bin metanode
```

### 2. Chạy lại hệ thống

```bash
./scripts/run_full_system.sh
```

### 3. Monitor log

```bash
# Xem heartbeat
tail -f logs/latest/node_0.log | grep "COMMIT PROCESSOR HEARTBEAT"

# Xem timeout warnings
tail -f logs/latest/node_0.log | grep "timeout"

# Phân tích node stuck
./scripts/analyze_node_stuck.sh logs/latest 0
```

## Kết quả mong đợi

1. **Commit processor không bị stuck**: Timeout đảm bảo không bị block vô hạn
2. **Dễ dàng detect stuck**: Heartbeat log giúp phát hiện sớm vấn đề
3. **Tự động recover**: Khi timeout, connection được đóng và retry, không fail commit
4. **Better debugging**: Log chi tiết giúp debug dễ dàng hơn

## Lưu ý

- Timeout 10 giây có thể cần điều chỉnh tùy theo môi trường
- Heartbeat interval 1000 commits có thể cần điều chỉnh tùy theo tần suất commit
- Nếu Go executor chậm, có thể cần tăng timeout hoặc optimize Go executor

