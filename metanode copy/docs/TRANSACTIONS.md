# Xử lý Transactions

## Tổng quan

MetaNode xử lý transactions thông qua consensus layer, đảm bảo tất cả nodes đồng thuận về thứ tự transactions và commit chúng theo thứ tự.

## Transaction Lifecycle

```
1. Client Submission
   │
   ├─► RPC Server receives transaction
   │
2. Transaction Pool
   │
   ├─► TransactionClient adds to pool
   │
3. Block Proposal
   │
   ├─► Authority includes transactions in block
   │
4. Consensus
   │
   ├─► Blocks achieve consensus
   │
5. Commit
   │
   ├─► CommittedSubDag created
   │
6. Execution
   │
   ├─► CommitProcessor processes in order
   │
   └─► Application logic executes transactions
```

## Transaction Format

### Raw Bytes

Transactions là raw bytes, không có format cố định:
- Text data: `"Hello, Blockchain!"`
- Binary data: `[0x48, 0x65, 0x6c, 0x6c, 0x6f]`
- Hex string: `"48656c6c6f"`

### Transaction Hash

Transaction hash được tính bằng Blake2b256:
```rust
let tx_hash = Blake2b256::digest(&tx_data);
let tx_hash_hex = hex::encode(&tx_hash[..8]); // First 8 bytes
```

**Ví dụ:**
```
Data: "Hello, Blockchain!"
Hash: 204d69c3943745b5
```

## Transaction Submission

### Via RPC API

```bash
curl -X POST http://127.0.0.1:10100/submit \
  -H "Content-Type: text/plain" \
  -d "Hello, Blockchain!"
```

### Response

```json
{
  "success": true,
  "tx_hash": "204d69c3943745b5",
  "block_ref": "B1106409([0],OhLiV2iSZBDx7VasA3akz6Erwp5FtymsC7DT5kREUxY=)",
  "indices": [0]
}
```

### Via TransactionClient

```rust
let transaction_client = node.transaction_client();
let (block_ref, indices, status_receiver) = transaction_client
    .submit(vec![tx_data])
    .await?;
```

## Transaction Pool

### Pool Management

TransactionClient quản lý transaction pool:
- Transactions được thêm vào pool khi submit
- Pool được consume bởi block proposers
- Transactions được include trong blocks

### Pool Limits

Default limits (có thể tùy chỉnh):
- Max pending transactions: Unlimited (có thể set limit)
- Transaction size: No limit (có thể set limit)

## Block Inclusion

### Block Proposal

Mỗi authority:
1. Thu thập transactions từ pool
2. Tạo block với transactions
3. Broadcast block đến peers

### Transaction Ordering

Transactions trong block được giữ nguyên thứ tự:
- Thứ tự trong block = thứ tự submit
- Blocks được commit theo thứ tự
- Commits được execute theo thứ tự

## Commit Processing

### CommittedSubDag Structure

```rust
CommittedSubDag {
    commit_ref: CommitRef {
        index: 100,
        digest: ...,
    },
    leader: BlockRef,
    blocks: Vec<Block>,  // Multiple blocks
    timestamp: u64,
}
```

### Multiple Blocks per Commit

Một commit có thể chứa nhiều blocks:
- Leader block: Block được chọn làm leader
- Supporting blocks: Blocks từ các authorities khác

**Ví dụ:**
```
Commit #100:
  Leader: B100([0], ...)
  Blocks:
    - B99([1], ...)    ← 2 transactions
    - B99([2], ...)    ← 0 transactions
    - B99([3], ...)    ← 1 transaction
    - B100([0], ...)   ← 3 transactions (leader)
  Total: 4 blocks, 6 transactions
```

### Ordered Execution

CommitProcessor đảm bảo commits được xử lý theo thứ tự:

```rust
// Commit #1 arrives → Process immediately
// Commit #2 arrives → Process immediately
// Commit #4 arrives → Store in pending (out of order)
// Commit #3 arrives → Process immediately, then process #4
```

## Transaction Verification

### NoopTransactionVerifier

Hiện tại sử dụng NoopTransactionVerifier:
- Không verify transactions
- Accept tất cả transactions
- Dùng cho testing

### Custom Verifier

Có thể implement custom verifier:

```rust
impl TransactionVerifier for MyVerifier {
    fn verify_batch(&self, batch: &[&[u8]]) -> Result<(), ValidationError> {
        for tx in batch {
            // Verify transaction
            if !is_valid(tx) {
                return Err(ValidationError::InvalidTransaction);
            }
        }
        Ok(())
    }
    
    fn verify_and_vote_batch(
        &self,
        block_ref: &BlockRef,
        batch: &[&[u8]],
    ) -> Result<Vec<TransactionIndex>, ValidationError> {
        // Verify and return indices of invalid transactions
        Ok(vec![])
    }
}
```

## Transaction Execution

### Commit Processor

CommitProcessor xử lý commits và extract transactions:

```rust
async fn process_commit(subdag: &CommittedSubDag) -> Result<()> {
    for block in &subdag.blocks {
        for tx in block.transactions() {
            // Execute transaction
            execute_transaction(tx).await?;
        }
    }
    Ok(())
}
```

### Execution Order

Transactions được execute theo thứ tự:
1. Blocks trong commit (theo thứ tự)
2. Transactions trong block (theo thứ tự)
3. Commits (theo commit index)

## Transaction Status

### Status Tracking

TransactionClient trả về status receiver:

```rust
let (block_ref, indices, status_receiver) = transaction_client
    .submit(vec![tx_data])
    .await?;

// Wait for transaction status
while let Some(status) = status_receiver.recv().await {
    match status {
        BlockStatus::Accepted => println!("Transaction accepted"),
        BlockStatus::Committed => println!("Transaction committed"),
        BlockStatus::Rejected(reason) => println!("Transaction rejected: {}", reason),
    }
}
```

### Status Types

- `Accepted`: Transaction được accept vào block
- `Committed`: Transaction được commit
- `Rejected`: Transaction bị reject (invalid, duplicate, etc.)

## Transaction Logging

### Log Format

```
📤 Transaction submitted via RPC: hash=204d69c3943745b5, size=18 bytes
✅ Transaction included in block: hash=204d69c3943745b5, block=B1106409([0],...), indices=[0]
🔷 Executing commit #1106409 (ordered): leader=B1106409([0],...), 4 blocks, 1 total transactions
```

### Log Analysis

```bash
# Xem transactions được submit
grep "Transaction submitted" logs/node_0.log

# Xem transactions được include
grep "Transaction included" logs/node_0.log

# Xem commits có transactions
grep "Executing commit" logs/node_0.log | grep -v "0 transactions"

# Đếm số transactions
grep -c "Transaction submitted" logs/node_0.log
```

## Performance

### Throughput

- **Transaction submission**: ~1000+ tx/s (theoretical)
- **Block inclusion**: ~100-200 tx/s (practical)
- **Commit rate**: ~100-200 commits/s

### Latency

- **Submission to pool**: <10ms
- **Pool to block**: ~50-100ms
- **Block to commit**: ~200-500ms
- **End-to-end**: ~300-600ms

### Bottlenecks

1. **Network latency**: Ảnh hưởng đến consensus
2. **Block size**: Blocks lớn mất nhiều thời gian hơn
3. **Transaction pool**: Pool đầy có thể delay submission

## Best Practices

### Transaction Design

1. **Keep transactions small**: Transactions nhỏ hơn = throughput cao hơn
2. **Batch when possible**: Submit nhiều transactions cùng lúc
3. **Handle errors**: Implement retry logic
4. **Monitor status**: Track transaction status

### Error Handling

```rust
match transaction_client.submit(tx_data).await {
    Ok((block_ref, indices, _)) => {
        // Success
    }
    Err(e) => {
        // Handle error
        // Retry if needed
    }
}
```

### Monitoring

1. Monitor transaction submission rate
2. Monitor commit rate
3. Monitor transaction pool size
4. Monitor error rate

## Limitations

1. **No transaction size limit**: Có thể submit transactions rất lớn
2. **No rate limiting**: Có thể spam transactions
3. **No deduplication**: Duplicate transactions được accept
4. **No transaction history**: Không có API để query transaction history

## Future Improvements

- [ ] Transaction size limits
- [ ] Rate limiting
- [ ] Deduplication
- [ ] Transaction history API
- [ ] Transaction receipts
- [ ] Event system for transaction status

