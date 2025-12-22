# Execution Flow: Go Sub Node → Rust Consensus → Go Executor

## Tổng quan luồng

```
┌─────────────────┐
│  Go Sub Node    │
│  (block_processor)│
└────────┬────────┘
         │ 1. Gửi transactions
         │    (TxsProcessor2)
         │    via UDS/HTTP
         ▼
┌─────────────────┐
│  Rust Consensus │
│  (MetaNode)     │
│                 │
│  - RPC Server   │ ← Nhận transactions từ Go
│  - UDS Server   │ ← Nhận transactions từ Go (local)
│  - Consensus    │ ← Xử lý đồng thuận
│  - Commit       │ ← Commit blocks
│  - Executor     │ → Gửi committed blocks đến Go
│    Client       │
└────────┬────────┘
         │ 2. Gửi committed blocks
         │    (executor_client)
         │    via UDS
         ▼
┌─────────────────┐
│  Go Executor    │
│  (executor.Listener)│
│                 │
│  - Nhận blocks  │ ← Nhận từ Rust
│  - Thực thi     │ ← Execute transactions
│  - Cập nhật state│
└─────────────────┘
```

## Chi tiết từng bước

### Bước 1: Go Sub Node → Rust Consensus (Transaction Submission)

**File:** `block_processor.go` → `TxsProcessor2()`

**Luồng:**
1. Go sub node tạo transactions từ transaction pool
2. Marshal transactions thành bytes
3. Gửi đến Rust consensus qua:
   - **Ưu tiên:** Unix Domain Socket (`/tmp/metanode-tx-{node_id}.sock`)
   - **Fallback:** HTTP POST (`http://127.0.0.1:10100/submit`)

**Code:**
```go
// block_processor.go:1984
func (bp *BlockProcessor) TxsProcessor2() {
    // ...
    txClient, err := txsender.NewClient("127.0.0.1:10100", 0) // nodeID = 0
    // ...
    err := bp.txClient.SendTransaction(bTransaction) // Gửi qua UDS/HTTP
}
```

**Rust nhận:**
- `rpc.rs`: HTTP server nhận transactions
- `tx_socket_server.rs`: UDS server nhận transactions (nhanh hơn)

### Bước 2: Rust Consensus xử lý

**File:** `rpc.rs`, `tx_socket_server.rs` → `commit_processor.rs`

**Luồng:**
1. Rust nhận transaction từ Go
2. Submit vào consensus authority
3. Consensus xử lý và commit blocks
4. `CommitProcessor` xử lý commits theo thứ tự

**Code:**
```rust
// commit_processor.rs
async fn process_commit(
    subdag: &CommittedSubDag,
    global_exec_index: u64,
    epoch: u64,
    executor_client: Option<Arc<ExecutorClient>>,
) -> Result<()> {
    // ... xử lý commit ...
    
    // Gửi đến Go executor nếu enabled
    if let Some(ref client) = executor_client {
        client.send_committed_subdag(subdag, epoch).await?;
    }
}
```

### Bước 3: Rust Consensus → Go Executor (Block Execution)

**File:** `executor_client.rs` → `executor/listener.go`

**Luồng:**
1. Rust `ExecutorClient` convert `CommittedSubDag` → Protobuf `CommittedEpochData`
2. Gửi qua Unix Domain Socket (`/tmp/executor{node_id}.sock`)
3. Go `executor.Listener` nhận và decode protobuf
4. Go executor xử lý blocks và execute transactions

**Code:**
```rust
// executor_client.rs
pub async fn send_committed_subdag(&self, subdag: &CommittedSubDag, epoch: u64) -> Result<()> {
    let epoch_data_bytes = self.convert_to_protobuf(subdag, epoch)?;
    // Gửi qua UDS với Uvarint length prefix
    stream.write_all(&len_buf).await?;
    stream.write_all(&epoch_data_bytes).await?;
}
```

**Go nhận:**
```go
// executor/listener.go
func (l *Listener) handleConnection(conn net.Conn) {
    msgLen, err := binary.ReadUvarint(reader) // Đọc Uvarint length
    buf := make([]byte, msgLen)
    io.ReadFull(reader, buf)
    proto.Unmarshal(buf, &epochData) // Decode protobuf
    l.dataChan <- &epochData // Gửi đến processor
}
```

### Bước 4: Go Executor xử lý blocks

**File:** `block_processor.go` → `runSocketExecutor()`

**Luồng:**
1. Nhận `CommittedEpochData` từ `executor.Listener`
2. Unmarshal transactions từ blocks
3. Process transactions (execute)
4. Tạo blocks và cập nhật state

**Code:**
```go
// block_processor.go:216
func (bp *BlockProcessor) runSocketExecutor(socketID int) {
    listener := executor.NewListener(socketID)
    listener.Start()
    dataChan := listener.DataChannel()
    
    for epochData := range dataChan {
        for _, block := range epochData.Blocks {
            // Unmarshal transactions
            transactions, _ := transaction.UnmarshalTransactions(ms.Digest)
            // Process transactions
            accumulatedResults, _ := bp.transactionProcessor.ProcessTransactions(transactions)
            // Tạo block
            newBlock := bp.createBlockFromResults(accumulatedResults, ...)
        }
    }
}
```

## Điểm kết nối

### 1. Transaction Submission (Go → Rust)
- **Go:** `txsender.Client.SendTransaction()` 
- **Protocol:** UDS (ưu tiên) hoặc HTTP (fallback)
- **Rust:** `tx_socket_server.rs` hoặc `rpc.rs`
- **Socket:** `/tmp/metanode-tx-{node_id}.sock` hoặc `http://127.0.0.1:10100/submit`

### 2. Block Execution (Rust → Go)
- **Rust:** `executor_client.rs.send_committed_subdag()`
- **Protocol:** Unix Domain Socket với Uvarint + Protobuf
- **Go:** `executor.Listener.handleConnection()`
- **Socket:** `/tmp/executor{node_id}.sock`

## Cấu hình

### Enable Executor (chỉ node 0)
```bash
# Tạo file config cho node 0
touch config/enable_executor.toml
```

### Node ID mapping
- **Node 0:** 
  - Transaction UDS: `/tmp/metanode-tx-0.sock`
  - Executor UDS: `/tmp/executor0.sock`
- **Node 1:**
  - Transaction UDS: `/tmp/metanode-tx-1.sock`
  - Executor UDS: `/tmp/executor1.sock` (nếu enabled)

## Luồng hoàn chỉnh

```
1. Go Sub Node (TxsProcessor2)
   ↓
   Tạo transactions từ pool
   ↓
   MarshalTransactions() → []byte (protobuf Transactions)
   ↓
   SendTransaction() → UDS/HTTP
   ↓
2. Rust Consensus (MetaNode)
   ↓
   Nhận transactions (tx_socket_server/rpc)
   ↓
   Submit vào consensus authority
   ↓
   Consensus commit blocks
   ↓
   CommitProcessor.process_commit()
   ↓
   ExecutorClient.send_committed_subdag()
   ↓
   Convert to Protobuf:
     - Extract tx.data() (raw bytes) từ mỗi transaction
     - Gửi trong TransactionExe.digest field (thực tế là tx data)
   ↓
   Gửi qua UDS (/tmp/executor0.sock)
   ↓
3. Go Executor (executor.Listener)
   ↓
   Nhận CommittedEpochData
   ↓
   UnmarshalTransactions(ms.Digest) → transaction data
   ↓
   ProcessTransactions()
   ↓
   Tạo blocks và cập nhật state
   ↓
   Hoàn tất execution
```

## ⚠️ Lưu ý quan trọng về Transaction Data

### Rust → Go: Transaction Data Format

**Rust gửi:**
- `TransactionExe.digest` field chứa **transaction data (raw bytes)**, không phải digest/hash
- Transaction data là raw bytes từ `tx.data()` trong Rust
- Format: Protobuf `Transactions` message (giống như Go gửi lên Rust)

**Go nhận:**
- `ms.Digest` chứa transaction data (raw bytes)
- `transaction.UnmarshalTransactions(ms.Digest)` unmarshal thành `[]types.Transaction`
- Format phải khớp với `MarshalTransactions()` format (protobuf `Transactions`)

### Data Flow

```
Go Sub Node:
  transactions []types.Transaction
  ↓ MarshalTransactions()
  bTransaction []byte (protobuf Transactions)
  ↓ SendTransaction()
  
Rust Consensus:
  Nhận bTransaction []byte
  ↓ Submit vào consensus
  ↓ Commit blocks
  ↓ Extract tx.data() (raw bytes - chính là bTransaction)
  ↓ Gửi trong TransactionExe.digest
  
Go Executor:
  Nhận ms.Digest []byte (chính là bTransaction)
  ↓ UnmarshalTransactions()
  transactions []types.Transaction
  ↓ ProcessTransactions()
  Execute transactions
```

## Kiểm tra luồng

### 1. Transaction Flow
- ✅ Go sub node gửi transactions → Rust (UDS/HTTP)
- ✅ Rust nhận và submit vào consensus
- ✅ Rust commit blocks

### 2. Execution Flow
- ✅ Rust gửi committed blocks → Go executor (UDS)
- ✅ Go executor nhận và process
- ✅ Go executor execute transactions

### 3. Cấu hình
- ✅ Chỉ node 0 có `config/enable_executor.toml`
- ✅ Executor client chỉ enable khi có file config
- ✅ Socket paths đúng format

## Lưu ý

1. **Transaction Submission:**
   - Go sub node gửi transactions đến Rust consensus
   - Rust xử lý đồng thuận và commit blocks

2. **Block Execution:**
   - Rust gửi committed blocks đến Go executor
   - Go executor thực thi transactions và cập nhật state

3. **Node Configuration:**
   - Chỉ node 0 có executor enabled (có file config)
   - Các node khác không có executor → không gửi blocks

4. **Error Handling:**
   - Executor connection failure không block commit
   - Transaction submission failure được log và retry

