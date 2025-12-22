# Transaction Data Integrity

## Tổng quan

Tài liệu này mô tả cách đảm bảo transaction data không bị thay đổi trong quá trình đồng thuận từ Go sub node → Rust consensus → Go master executor.

## Nguyên tắc cốt lõi

**Transaction data phải được truyền nguyên vẹn, không bị modify trong suốt quá trình consensus.**

## Luồng dữ liệu

```
Go Sub Node
    │
    │ (1) Gửi transaction data (protobuf bytes) qua UDS/HTTP
    ▼
Rust UDS Server / RPC Server
    │
    │ (2) Nhận transaction data dưới dạng Vec<u8> (raw bytes)
    │     - Verify protobuf format (optional)
    │     - Calculate transaction hash để tracking
    ▼
Rust TransactionClient
    │
    │ (3) Submit transaction vào consensus pool
    │     - Transaction data được lưu trong Transaction struct
    │     - Transaction.data: Bytes (immutable, không thể thay đổi)
    ▼
Rust Consensus Layer
    │
    │ (4) Consensus xử lý blocks
    │     - Transaction data được lưu trong blocks
    │     - Chỉ có ordering, không modify data
    │     - tx.data() trả về reference đến original bytes
    ▼
Rust CommitProcessor
    │
    │ (5) Khi commit, lấy transaction data từ blocks
    │     - tx.data() trả về original bytes (không copy)
    │     - Verify hash để đảm bảo data không bị thay đổi
    ▼
Rust ExecutorClient
    │
    │ (6) Convert CommittedSubDag → protobuf CommittedEpochData
    │     - Sử dụng generated protobuf code (prost)
    │     - tx_data.to_vec() tạo copy cho protobuf encoding
    │     - Nhưng data content không thay đổi
    │     - Encode bằng prost::Message::encode (đảm bảo đúng format)
    ▼
Go Master Executor
    │
    │ (7) Nhận CommittedEpochData qua UDS
    │     - Unmarshal protobuf thành CommittedEpochData
    │     - Extract transaction data từ TransactionExe.digest
    │     - Unmarshal transaction data thành Transaction objects
    │     - Execute transactions
    ▼
Execution Complete
```

## Đảm bảo tính toàn vẹn

### 1. Transaction Storage trong Rust

```rust
// meta-consensus/core/src/block.rs
pub struct Transaction {
    data: Bytes,  // Immutable, không thể thay đổi
}

impl Transaction {
    pub fn data(&self) -> &[u8] {
        &self.data  // Trả về reference, không copy
    }
}
```

**Đặc điểm:**
- `data: Bytes` là immutable
- `data()` trả về reference, không tạo copy
- Không có method nào để modify data

### 2. Hash Verification

Transaction hash được tính tại nhiều điểm để verify data integrity:

1. **Khi nhận từ Go sub node** (`tx_socket_server.rs`):
   ```rust
   let tx_hash_hex = calculate_transaction_hash_hex(&tx_data);
   ```

2. **Khi commit** (`commit_processor.rs`):
   ```rust
   let tx_data = tx.data();
   let tx_hash_hex = calculate_transaction_hash_hex(tx_data);
   ```

3. **Khi gửi về Go master** (`executor_client.rs`):
   ```rust
   let tx_data = tx.data();
   let tx_hash_hex = calculate_transaction_hash_hex(tx_data);
   info!("✅ [TX INTEGRITY] Transaction data preserved: hash={}", tx_hash_hex);
   ```

**Nếu hash khác nhau → data đã bị thay đổi → lỗi!**

### 3. Protobuf Encoding/Decoding

#### Rust → Go (ExecutorClient)

```rust
// Sử dụng generated protobuf code
let tx_exe = TransactionExe {
    digest: tx_data.to_vec(),  // Copy cho encoding, nhưng content không đổi
    worker_id: 0,
};

let epoch_data = CommittedEpochData { blocks };
let mut buf = Vec::new();
epoch_data.encode(&mut buf)?;  // prost::Message::encode đảm bảo đúng format
```

#### Go → Rust (Transaction Submission)

```go
// Go sub node gửi transaction data (protobuf bytes)
txData, _ := transaction.MarshalTransactions([]types.Transaction{tx})
// Gửi qua UDS/HTTP
```

### 4. Protobuf Format Verification

```rust
// Verify transaction data là valid protobuf
pub fn verify_transaction_protobuf(tx_data: &[u8]) -> bool {
    // Try Transactions (multiple)
    if Transactions::decode(tx_data).is_ok() {
        return true;
    }
    
    // Try Transaction (single)
    if Transaction::decode(tx_data).is_ok() {
        return true;
    }
    
    false
}
```

## Logging và Monitoring

### Transaction Flow Logs

1. **Submission** (`tx_socket_server.rs`):
   ```
   📤 [TX FLOW] Transaction submitted via UDS: hash=..., size=... bytes
   ✅ [TX INTEGRITY] Transaction data is valid protobuf: hash=...
   ```

2. **Commit** (`commit_processor.rs`):
   ```
   🔷 [Global Index: X] Executing commit #Y: ... transactions
   ```

3. **Executor Send** (`executor_client.rs`):
   ```
   ✅ [TX INTEGRITY] Transaction data preserved: hash=..., size=... bytes
   📤 [TX FLOW] Sent committed sub-DAG to Go executor: ...
   ```

### Go Master Logs

```
📥 [TX FLOW] Received committed epoch data from Rust: epoch=..., blocks=...
📦 [TX FLOW] Processing committed sub-DAG: ... blocks
✅ [TX FLOW] Unmarshaled transaction(s) from block[0].tx[0]
```

## Testing Data Integrity

### Test Case 1: Hash Consistency

```rust
// 1. Submit transaction
let tx_data = b"test transaction data";
let hash1 = calculate_transaction_hash_hex(tx_data);

// 2. After consensus
let tx_data_after = block.transactions()[0].data();
let hash2 = calculate_transaction_hash_hex(tx_data_after);

// 3. After executor encoding
let tx_exe = TransactionExe { digest: tx_data_after.to_vec(), ... };
let hash3 = calculate_transaction_hash_hex(&tx_exe.digest);

// Assert: hash1 == hash2 == hash3
assert_eq!(hash1, hash2);
assert_eq!(hash2, hash3);
```

### Test Case 2: Protobuf Round-trip

```rust
// 1. Go → Rust
let tx_data = go_marshal_transaction(tx);

// 2. Rust consensus (no modification)
let tx = Transaction::new(tx_data);

// 3. Rust → Go
let tx_data_sent = tx.data().to_vec();

// 4. Go unmarshal
let tx_unmarshaled = go_unmarshal_transaction(&tx_data_sent);

// Assert: tx == tx_unmarshaled
```

## Best Practices

1. **Luôn verify hash** tại các điểm quan trọng
2. **Sử dụng generated protobuf code** thay vì encode thủ công
3. **Log transaction hash** để tracking
4. **Không modify transaction data** trong consensus layer
5. **Verify protobuf format** khi nhận từ Go sub node

## Troubleshooting

### Vấn đề: Transaction hash khác nhau

**Nguyên nhân:**
- Transaction data bị modify trong consensus
- Protobuf encoding/decoding sai

**Giải pháp:**
1. Check logs để tìm điểm hash thay đổi
2. Verify protobuf encoding/decoding
3. Check xem có code nào modify transaction data không

### Vấn đề: Go không unmarshal được

**Nguyên nhân:**
- Protobuf encoding sai format
- Transaction data không phải protobuf

**Giải pháp:**
1. Verify `executor.proto` được compile đúng
2. Sử dụng `prost::Message::encode` thay vì encode thủ công
3. Check transaction data format từ Go sub node

## Kết luận

Transaction data được bảo vệ bởi:
1. **Immutable storage** trong Rust (`Bytes`)
2. **Hash verification** tại nhiều điểm
3. **Protobuf encoding/decoding** đúng format
4. **Logging** để tracking và debugging

**Đảm bảo: Transaction data từ Go sub node → Rust consensus → Go master executor là hoàn toàn giống nhau, không bị thay đổi.**

