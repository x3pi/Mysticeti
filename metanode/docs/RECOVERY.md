# Recovery và Commit Replay

## Tổng quan

Khi node khởi động lại, hệ thống sẽ thực hiện recovery process để khôi phục trạng thái từ database. Một phần quan trọng của quá trình này là **replay các commits cũ** - điều này có nghĩa là các commits đã được execute trước đó sẽ được execute lại.

## Commit Replay khi Khởi động

### Câu hỏi: Executing commit có chạy lại không? Có trùng với cái cũ không?

**Trả lời:** **CÓ**, các commits cũ sẽ được execute lại khi khởi động, và **CÓ trùng** với các commits đã execute trước đó.

### Quá trình Recovery

Khi node khởi động, bạn sẽ thấy trong log:

```
INFO: Recovering commit observer in the range [1..=29945]
INFO: Recovering 250 unsent commits in range [1..=250]
INFO: 🔷 Executing commit #1 (ordered): leader=..., 1 blocks, 0 transactions
INFO: 🔷 Executing commit #2 (ordered): leader=..., 3 blocks, 0 transactions
INFO: 🔷 Executing commit #3 (ordered): leader=..., 3 blocks, 0 transactions
...
```

**Giải thích:**

1. **Recovering commit observer**: Load lại trạng thái commit observer từ database
2. **Recovering unsent commits**: Phát hiện các commits chưa được gửi đến commit consumer (250 commits trong ví dụ)
3. **Executing commit #1, #2, #3...**: Các commits cũ được gửi lại và execute lại

### Tại sao lại Replay?

**Lý do kỹ thuật:**

1. **Commit Observer State**: Commit observer lưu trạng thái về commits đã được gửi đến commit consumer
2. **Unsent Commits**: Khi node shutdown, có thể có một số commits đã được finalize nhưng chưa được gửi đến commit consumer
3. **Recovery**: Khi restart, commit observer recover các "unsent commits" và gửi lại chúng

**Ví dụ:**
```
Trước khi shutdown:
  - Commit #1-100: Đã được gửi và execute
  - Commit #101-250: Đã được finalize nhưng chưa gửi đến consumer
  - Commit #251+: Chưa được finalize

Sau khi restart:
  - Commit observer recover: "Có 250 unsent commits [1..=250]"
  - Gửi lại tất cả 250 commits đến commit processor
  - Commit processor execute lại tất cả: #1, #2, #3, ..., #250
```

### CommitConsumerArgs::new(0, 0)

Trong code:
```rust
let (commit_consumer, commit_receiver, mut block_receiver) = CommitConsumerArgs::new(0, 0);
```

**Tham số:**
- `0`: `last_processed_commit_index` - Commit index cuối cùng đã được process
- `0`: `last_sent_commit_index` - Commit index cuối cùng đã được gửi

**Với giá trị (0, 0):**
- Commit consumer nghĩ rằng chưa có commit nào được process
- Commit observer sẽ gửi lại tất cả commits từ đầu
- Dẫn đến replay tất cả commits

## Ảnh hưởng của Commit Replay

### 1. Logs bị Trùng

**Vấn đề:**
- Mỗi lần restart, bạn sẽ thấy lại các commits cũ trong log
- Logs sẽ có nhiều dòng "Executing commit #1", "#2", "#3" giống nhau

**Ví dụ:**
```
# Lần khởi động đầu tiên
2025-12-17T02:20:17.550462Z  INFO: 🔷 Executing commit #1
2025-12-17T02:20:17.550531Z  INFO: 🔷 Executing commit #2

# Sau khi restart
2025-12-17T03:08:01.637002Z  INFO: 🔷 Executing commit #1  ← Trùng!
2025-12-17T03:08:01.637031Z  INFO: 🔷 Executing commit #2  ← Trùng!
```

### 2. Transaction Execution

**Hiện tại:**
- Commit processor chỉ **log** commits, chưa thực sự execute transactions
- Code có TODO: `// TODO: Here you can execute transactions in order`

**Nếu implement transaction execution:**
- Transactions sẽ được execute lại mỗi lần restart
- Có thể gây duplicate execution
- Cần implement idempotency để tránh duplicate

### 3. Performance

**Ảnh hưởng:**
- Replay commits tốn thời gian
- Với 250 commits: ~1-2 giây
- Với 1M+ commits: ~30-50 giây

## Giải pháp

### 1. Track Last Processed Commit Index

**Vấn đề hiện tại:**
```rust
CommitConsumerArgs::new(0, 0)  // Luôn bắt đầu từ 0
```

**Giải pháp:**
- Lưu `last_processed_commit_index` vào storage
- Load khi khởi động
- Chỉ replay commits sau index này

**Ví dụ:**
```rust
// Load last processed index từ storage
let last_processed = load_last_processed_commit_index()?;

// Chỉ replay commits sau index này
let (commit_consumer, commit_receiver, _) = 
    CommitConsumerArgs::new(last_processed, last_processed);
```

### 2. Skip Replay cho Commits đã Execute

**Cách 1: Check trong Commit Processor**

```rust
impl CommitProcessor {
    pub fn new(receiver: UnboundedReceiver<CommittedSubDag>) -> Self {
        // Load last processed index
        let next_expected_index = load_last_processed_index().unwrap_or(1);
        
        Self {
            receiver,
            next_expected_index,
            pending_commits: BTreeMap::new(),
        }
    }
    
    async fn process_commit(subdag: &CommittedSubDag) -> Result<()> {
        let commit_index = subdag.commit_ref.index;
        
        // Check if already processed
        if is_already_processed(commit_index)? {
            // Skip - đã execute rồi
            return Ok(());
        }
        
        // Process commit
        // ...
        
        // Mark as processed
        mark_as_processed(commit_index)?;
        
        Ok(())
    }
}
```

**Cách 2: Filter trong Commit Observer**

- Commit observer chỉ gửi commits chưa được process
- Cần track last processed index

### 3. Idempotent Transaction Execution

**Nếu implement transaction execution:**

```rust
async fn process_commit(subdag: &CommittedSubDag) -> Result<()> {
    let commit_index = subdag.commit_ref.index;
    
    // Check if already executed
    if is_commit_executed(commit_index)? {
        // Skip execution, chỉ log
        info!("Commit #{} already executed, skipping", commit_index);
        return Ok(());
    }
    
    // Execute transactions (idempotent)
    for block in &subdag.blocks {
        for tx in block.transactions() {
            execute_transaction_idempotent(tx).await?;
        }
    }
    
    // Mark as executed
    mark_commit_executed(commit_index)?;
    
    Ok(())
}
```

## Best Practices

### 1. Track Execution State

- Lưu trạng thái execution vào database
- Check trước khi execute
- Tránh duplicate execution

### 2. Idempotent Operations

- Thiết kế transactions để idempotent
- Có thể execute nhiều lần mà không ảnh hưởng
- Sử dụng transaction hash để check duplicate

### 3. Logging

- Log commits đã được replay
- Distinguish giữa new commits và replayed commits
- Có thể filter logs để chỉ xem new commits

### 4. Performance

- Chỉ replay commits cần thiết
- Skip commits đã được execute
- Cache execution state

## Ví dụ từ Logs

### Lần khởi động đầu tiên:

```
2025-12-17T02:20:17.550462Z  INFO: 🔷 Executing commit #1
2025-12-17T02:20:17.550531Z  INFO: 🔷 Executing commit #2
...
2025-12-17T02:32:19.593768Z  INFO: Consensus authority started
```

### Sau khi restart:

```
2025-12-17T03:08:01.634644Z  INFO: Recovering 250 unsent commits in range [1..=250]
2025-12-17T03:08:01.637002Z  INFO: 🔷 Executing commit #1  ← Replay!
2025-12-17T03:08:01.637031Z  INFO: 🔷 Executing commit #2  ← Replay!
...
2025-12-17T03:08:01.641825Z  INFO: 🔷 Executing commit #250  ← Replay!
2025-12-17T03:08:01.642043Z  INFO: 🔷 Executing commit #251  ← New commit
```

## Tóm tắt

### Câu trả lời ngắn gọn:

**Q: Executing commit có chạy lại không?**
- **A: CÓ**, các commits cũ sẽ được execute lại khi khởi động

**Q: Có trùng với cái cũ không?**
- **A: CÓ**, các commits đã execute trước đó sẽ được execute lại (trùng)

### Lý do:

1. **CommitConsumerArgs::new(0, 0)**: Luôn bắt đầu từ commit index 0
2. **Unsent commits recovery**: Commit observer recover và gửi lại các commits chưa được gửi
3. **No tracking**: Không có cơ chế track commits đã được execute

### Giải pháp:

1. **Track last processed index**: Lưu và load commit index cuối cùng đã process
2. **Skip already processed**: Check và skip commits đã được execute
3. **Idempotent execution**: Thiết kế transactions để có thể execute nhiều lần an toàn

## References

- [FAQ.md](./FAQ.md) - Câu hỏi về recovery
- [ARCHITECTURE.md](./ARCHITECTURE.md) - Kiến trúc commit processor
- [TRANSACTIONS.md](./TRANSACTIONS.md) - Xử lý transactions

