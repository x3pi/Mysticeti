# Transaction Flow: Go Sub Node → Rust Consensus → Go Master Executor

## Tổng quan

Luồng giao dịch hoàn chỉnh từ sub node (Go) → consensus (Rust) → executor (Go):

```
┌─────────────────────────────────────────────────────────────────┐
│                    GO SUB NODE                                 │
│  (ServiceType: Readonly/Write, Mode: != SINGLE)                │
│                                                                 │
│  block_processor.go: TxsProcessor2()                           │
│    ↓                                                            │
│  1. ProcessTransactionsInPoolSub()                             │
│     → Lấy transactions từ pool                                 │
│    ↓                                                            │
│  2. MarshalTransactions(txs)                                    │
│     → []byte (protobuf Transactions message)                   │
│    ↓                                                            │
│  3. txClient.SendTransaction(bTransaction)                      │
│     → Gửi qua UDS (/tmp/metanode-tx-0.sock) hoặc HTTP          │
└───────────────────────┬─────────────────────────────────────────┘
                        │
                        │ Transaction Data (protobuf Transactions)
                        │
                        ▼
┌─────────────────────────────────────────────────────────────────┐
│                  RUST CONSENSUS (MetaNode)                     │
│                                                                 │
│  tx_socket_server.rs hoặc rpc.rs                                │
│    ↓                                                            │
│  Nhận transaction data (raw bytes)                              │
│    ↓                                                            │
│  transaction_client.submit(vec![tx_data])                       │
│    ↓                                                            │
│  Consensus Authority xử lý và commit blocks                    │
│    ↓                                                            │
│  commit_processor.rs: process_commit()                         │
│    ↓                                                            │
│  Calculate global_exec_index (deterministic)                   │
│    - Epoch 0: global_exec_index = commit_index                 │
│    - Epoch N: global_exec_index = last_global_exec_index + commit_index │
│    ↓                                                            │
│  executor_client.send_committed_subdag(subdag, epoch, global_exec_index) │
│    ↓                                                            │
│  convert_to_protobuf():                                         │
│    - Extract tx.data() (raw bytes) từ mỗi transaction         │
│    - Tạo CommittedEpochData protobuf với:                      │
│      * global_exec_index (CRITICAL FORK-SAFETY)                 │
│      * commit_index (epoch-local)                              │
│      * epoch                                                    │
│    - Gửi qua UDS (/tmp/executor0.sock)                         │
└───────────────────────┬─────────────────────────────────────────┘
                        │
                        │ CommittedEpochData (protobuf)
                        │
                        ▼
┌─────────────────────────────────────────────────────────────────┐
│                  GO MASTER EXECUTOR                             │
│  (ServiceType: Master, Mode: != SINGLE)                         │
│                                                                 │
│  block_processor.go: runSocketExecutor(0)                       │
│    ↓                                                            │
│  executor.NewListener(0)                                        │
│    → Lắng nghe /tmp/executor0.sock                             │
│    ↓                                                            │
│  Nhận CommittedEpochData từ channel                             │
│    ↓                                                            │
│  for epochData := range dataChan {                              │
│    for _, block := range epochData.Blocks {                     │
│      for _, ms := range block.Transactions {                    │
│        transactions, _ := UnmarshalTransactions(ms.Digest)      │
│        → ms.Digest chứa transaction data (raw bytes)           │
│      }                                                          │
│      ProcessTransactions(allTransactions)                       │
│      → Execute transactions và cập nhật state                  │
│    }                                                            │
│  }                                                              │
└─────────────────────────────────────────────────────────────────┘
```

## Chi tiết từng bước

### Bước 1: Go Sub Node gửi transactions

**File:** `mtn-simple-2025/cmd/simple_chain/processor/block_processor.go`

**Function:** `TxsProcessor2()`

**Luồng:**
1. Lấy transactions từ pool: `ProcessTransactionsInPoolSub()`
2. Marshal thành protobuf: `transaction.MarshalTransactions(txs)`
   - Tạo `pb.Transactions` message chứa danh sách `pb.Transaction`
   - Encode thành bytes
3. Gửi đến Rust:
   - **Ưu tiên:** Unix Domain Socket `/tmp/metanode-tx-{node_id}.sock`
   - **Fallback:** HTTP POST `http://127.0.0.1:10100/submit`

**Code:**
```go
// block_processor.go:1984
func (bp *BlockProcessor) TxsProcessor2() {
    // ...
    if bp.txClient == nil {
        nodeID := 0 // Node ID để xác định socket path
        bp.txClient, err = txsender.NewClient("127.0.0.1:10100", nodeID)
    }
    
    txs := bp.transactionProcessor.ProcessTransactionsInPoolSub(false)
    bTransaction, err := transaction.MarshalTransactions(txs)
    
    // Gửi transaction (Mode != SINGLE)
    if bp.chainState.GetConfig().Mode != p_common.MODE_SINGLE {
        err := bp.txClient.SendTransaction(bTransaction)
    }
}
```

**Protocol:**
- **UDS:** Length-prefixed binary (4 bytes BE length + data)
- **HTTP:** POST với hex-encoded payload

### Bước 2: Rust Consensus nhận và xử lý

**Files:** 
- `Mysticeti/metanode/src/tx_socket_server.rs` (UDS)
- `Mysticeti/metanode/src/rpc.rs` (HTTP)

**Luồng:**
1. Nhận transaction data (raw bytes)
2. Tính hash chính thức: `calculate_transaction_hash_hex(tx_data)`
   - Parse protobuf `Transactions` hoặc `Transaction`
   - Tạo `TransactionHashData` và tính Keccak256
3. Submit vào consensus: `transaction_client.submit(vec![tx_data])`
4. Consensus xử lý và commit blocks

**Code:**
```rust
// tx_socket_server.rs
async fn handle_connection(...) {
    // Đọc transaction data
    let tx_data = vec![0u8; data_len];
    stream.read_exact(&mut tx_data).await?;
    
    // Tính hash chính thức
    let tx_hash_hex = calculate_transaction_hash_hex(&tx_data);
    
    // Submit vào consensus
    client.submit(vec![tx_data]).await?;
}
```

### Bước 3: Rust Commit và gửi đến Go Executor

**Files:**
- `Mysticeti/metanode/src/commit_processor.rs`
- `Mysticeti/metanode/src/executor_client.rs`

**Luồng:**
1. `CommitProcessor` nhận `CommittedSubDag` từ consensus
2. Xử lý commits theo thứ tự (đảm bảo ordering)
3. Gọi `executor_client.send_committed_subdag(subdag, epoch)`
4. Convert sang protobuf:
   - Extract `tx.data()` (raw bytes) từ mỗi transaction
   - Tạo `CommittedEpochData` protobuf message
5. Gửi qua UDS: `/tmp/executor{node_id}.sock`

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

// executor_client.rs
fn convert_to_protobuf(&self, subdag: &CommittedSubDag, epoch: u64) -> Result<Vec<u8>> {
    for block in &subdag.blocks {
        for tx in block.transactions() {
            let tx_data = tx.data(); // Raw bytes (protobuf Transactions)
            
            // Tạo TransactionExe với tx_data trong digest field
            // Field 1: digest (bytes) - chứa transaction data
            prost::encoding::encode_key(1, ..., &mut tx_exe);
            prost::encoding::encode_varint(tx_data.len() as u64, &mut tx_exe);
            tx_exe.extend_from_slice(tx_data);
        }
    }
    // Build CommittedEpochData...
}
```

**Protocol:**
- Uvarint length prefix + Protobuf `CommittedEpochData` message

### Bước 4: Go Master Executor nhận và thực thi (Sequential Processing)

**Files:**
- `mtn-simple-2025/executor/listener.go`
- `mtn-simple-2025/cmd/simple_chain/processor/block_processor.go`

**Luồng:**
1. `executor.Listener` lắng nghe trên `/tmp/executor0.sock`
2. **CRITICAL FORK-SAFETY: Initialize nextExpectedGlobalExecIndex từ DB**
   - `nextExpectedGlobalExecIndex = lastBlockNumber + 1`
   - Đảm bảo continuous progress sau restart
3. Nhận `CommittedEpochData` protobuf message với:
   - `global_exec_index` (CRITICAL FORK-SAFETY)
   - `commit_index` (epoch-local)
   - `epoch`
4. **CRITICAL FORK-SAFETY: Sequential processing**
   - Chỉ xử lý khi `global_exec_index == nextExpectedGlobalExecIndex`
   - Out-of-order blocks → buffer trong `pendingBlocks`
   - Skipped commits → buffer trong `skippedCommitsWithTxs` (retention: 100 commits)
5. Unmarshal và extract transactions:
   - `ms.Digest` chứa transaction data (raw bytes)
   - `transaction.UnmarshalTransactions(ms.Digest)` → `[]types.Transaction`
6. Process transactions: `ProcessTransactions(allTransactions)`
7. Execute và cập nhật state
8. **Update nextExpectedGlobalExecIndex**
   - `nextExpectedGlobalExecIndex = globalExecIndex + 1`
   - Process pending blocks nếu có

**Code:**
```go
// block_processor.go:216
func (bp *BlockProcessor) runSocketExecutor(socketID int) {
    listener := executor.NewListener(socketID) // socketID = 0
    listener.Start() // Lắng nghe /tmp/executor0.sock
    
    // CRITICAL FORK-SAFETY: Initialize nextExpectedGlobalExecIndex from DB
    var nextExpectedGlobalExecIndex uint64
    lastBlockFromDB := bp.GetLastBlock()
    if lastBlockFromDB != nil {
        nextExpectedGlobalExecIndex = lastBlockFromDB.Header().BlockNumber() + 1
    }
    
    // Buffering for out-of-order and skipped commits
    pendingBlocks := make(map[uint64]*pb.CommittedEpochData)
    skippedCommitsWithTxs := make(map[uint64]*pb.CommittedEpochData)
    
    dataChan := listener.DataChannel()
    for epochData := range dataChan {
        globalExecIndex := epochData.GetGlobalExecIndex()
        commitIndex := epochData.GetCommitIndex()
        
        // CRITICAL FORK-SAFETY: Sequential processing
        if globalExecIndex == nextExpectedGlobalExecIndex {
            // Process immediately
            // ... extract transactions and process ...
            nextExpectedGlobalExecIndex = globalExecIndex + 1
            // Process pending blocks if any
        } else if globalExecIndex > nextExpectedGlobalExecIndex {
            // Out-of-order: buffer for later
            pendingBlocks[globalExecIndex] = epochData
        } else {
            // Skipped commit: buffer if within retention window
            if globalExecIndex >= nextExpectedGlobalExecIndex - 100 {
                skippedCommitsWithTxs[globalExecIndex] = epochData
            }
        }
    }
}
```

## Điểm kết nối

### 1. Transaction Submission (Go → Rust)

| Component | Value |
|-----------|-------|
| **Go Client** | `txsender.Client.SendTransaction()` |
| **Protocol** | UDS (ưu tiên) hoặc HTTP (fallback) |
| **Rust Server** | `tx_socket_server.rs` (UDS) hoặc `rpc.rs` (HTTP) |
| **Socket Path** | `/tmp/metanode-tx-{node_id}.sock` |
| **HTTP Endpoint** | `http://127.0.0.1:10100/submit` |
| **Data Format** | Length-prefixed binary (UDS) hoặc hex-encoded (HTTP) |

### 2. Block Execution (Rust → Go)

| Component | Value |
|-----------|-------|
| **Rust Client** | `executor_client.rs.send_committed_subdag()` |
| **Protocol** | Unix Domain Socket với Uvarint + Protobuf |
| **Go Server** | `executor.Listener.handleConnection()` |
| **Socket Path** | `/tmp/executor{node_id}.sock` |
| **Data Format** | Uvarint length + Protobuf `CommittedEpochData` |

## Cấu hình

### Node Types

1. **Sub Node (Go):**
   - `ServiceType`: `Readonly` hoặc `Write`
   - `Mode`: `!= SINGLE`
   - Chạy `TxsProcessor2()` để gửi transactions

2. **Consensus Node (Rust):**
   - Nhận transactions từ sub nodes
   - Xử lý đồng thuận và commit blocks
   - Gửi committed blocks đến executor (nếu enabled)

3. **Master Executor (Go):**
   - `ServiceType`: `Master`
   - `Mode`: `!= SINGLE`
   - Chạy `runSocketExecutor(0)` để nhận và thực thi blocks

### Transaction Queuing (Barrier Phase)

**CRITICAL:** Transactions được queue trong barrier phase để tránh mất giao dịch:

1. **Barrier Detection:**
   - Khi `transition_barrier` được set, tất cả transactions được queue
   - Transactions được queue với reason: "Barrier phase: barrier=X is set"

2. **Queue Processing:**
   - Transactions được lưu trong `pending_transactions_queue`
   - Sau epoch transition, transactions được submit lại
   - Transactions được sắp xếp theo hash để đảm bảo fork-safety

3. **Deterministic Ordering:**
   - Transactions được sort theo hash (lexicographic order)
   - Deduplicate để tránh submit trùng lặp
   - Tất cả nodes submit transactions trong cùng thứ tự

### Executor Configuration

**LƯU Ý:** `executor_enabled` chỉ ảnh hưởng đến việc gửi commits đến Go Master:
- `executor_enabled = true`: Node gửi commits đến Go Master qua Unix Domain Socket
- `executor_enabled = false`: Node không gửi commits đến Go Master (consensus only)

**Tất cả nodes đều lấy committee từ Go**, không phụ thuộc vào `executor_enabled`.

### Socket Paths

| Node ID | Transaction UDS | Executor UDS |
|---------|-----------------|--------------|
| 0 | `/tmp/metanode-tx-0.sock` | `/tmp/executor0.sock` |
| 1 | `/tmp/metanode-tx-1.sock` | `/tmp/executor1.sock` (nếu enabled) |
| 2 | `/tmp/metanode-tx-2.sock` | `/tmp/executor2.sock` (nếu enabled) |
| 3 | `/tmp/metanode-tx-3.sock` | `/tmp/executor3.sock` (nếu enabled) |

## Transaction Data Format

### Go → Rust (Submission)

**Format:** Protobuf `Transactions` message
```protobuf
message Transactions {
  repeated Transaction Transactions = 1;
}
```

**Marshaling:**
```go
bTransaction, _ := transaction.MarshalTransactions(txs)
// bTransaction là []byte chứa protobuf-encoded Transactions
```

### Rust → Go (Execution)

**Format:** Protobuf `CommittedEpochData` message
```protobuf
message CommittedEpochData {
  repeated CommittedBlock blocks = 1;
}

message CommittedBlock {
  uint64 epoch = 1;
  uint64 height = 2;
  repeated TransactionExe transactions = 3;
}

message TransactionExe {
  bytes data = 1; // Chứa transaction data (protobuf Transactions)
  uint32 worker_id = 2;
}
```

**Unmarshaling:**
```go
// ms.Digest chứa transaction data (raw bytes)
transactions, _ := transaction.UnmarshalTransactions(ms.Digest)
// transactions là []types.Transaction
```

## Lưu ý quan trọng

1. **Transaction Data Consistency:**
   - Go gửi: `MarshalTransactions(txs)` → `[]byte` (protobuf `Transactions`)
   - Rust lưu: `tx.data()` → `[]byte` (chính là protobuf `Transactions`)
   - Rust gửi: `tx.data()` trong `TransactionExe.data` field
   - Go nhận: `UnmarshalTransactions(ms.Digest)` → `[]types.Transaction`
   - ✅ Transaction data được giữ nguyên qua toàn bộ luồng

2. **Hash Calculation:**
   - Tất cả transaction hash được tính bằng **Keccak256 từ TransactionHashData**
   - Khớp hoàn toàn giữa Go và Rust
   - Xem `docs/TRANSACTION_HASH_VERIFICATION.md` để biết chi tiết

3. **Error Handling:**
   - Executor connection failure không block commit
   - Transaction submission failure được log và retry
   - Invalid transaction data được skip (không crash)

4. **Ordering:**
   - `CommitProcessor` đảm bảo commits được xử lý theo thứ tự
   - `global_exec_index` đảm bảo deterministic ordering

## Kiểm tra luồng

### 1. Transaction Submission
- ✅ Go sub node gửi transactions → Rust (UDS/HTTP)
- ✅ Rust nhận và submit vào consensus
- ✅ Rust commit blocks theo thứ tự

### 2. Block Execution
- ✅ Rust gửi committed blocks → Go executor (UDS)
- ✅ Go executor nhận và unmarshal transactions
- ✅ Go executor execute transactions và cập nhật state

### 3. Data Integrity
- ✅ Transaction data giữ nguyên qua toàn bộ luồng
- ✅ Hash calculation khớp giữa Go và Rust
- ✅ Protobuf encoding/decoding đúng format

