# Xác minh Luồng: Go Sub → Rust Node 0 → Go Master

## Luồng mong muốn

```
┌─────────────────────────────────────────────────────────────┐
│  Go Sub Node (config-sub-write.json)                       │
│  - service_type: "SUB-WRITE"                               │
│  - mode: "Multi"                                           │
│                                                             │
│  TxsProcessor2()                                           │
│    ↓                                                        │
│  Gửi transactions → Rust Node 0                           │
│    - UDS: /tmp/metanode-tx-0.sock                          │
│    - HTTP: http://127.0.0.1:10100/submit                   │
└───────────────────────┬─────────────────────────────────────┘
                        │
                        │ Transactions (protobuf)
                        │
                        ▼
┌─────────────────────────────────────────────────────────────┐
│  Rust Node 0 (MetaNode)                                     │
│                                                             │
│  ✅ Consensus (nhận transactions từ Go Sub)                │
│  ✅ Executor Client (gửi blocks về Go Master)               │
│    - File: config/enable_executor.toml ✅                   │
│    - Socket: /tmp/executor0.sock                           │
└───────────────────────┬─────────────────────────────────────┘
                        │
                        │ Committed Blocks (protobuf)
                        │
                        ▼
┌─────────────────────────────────────────────────────────────┐
│  Go Master Node (config-master.json)                        │
│  - service_type: "MASTER"                                   │
│  - mode: "Multi"                                            │
│                                                             │
│  runSocketExecutor(0)                                       │
│    ↓                                                        │
│  Nhận blocks từ Rust Node 0                                 │
│    - Socket: /tmp/executor0.sock                          │
│    ↓                                                        │
│  Process & Execute transactions                             │
└─────────────────────────────────────────────────────────────┘
```

## Kiểm tra từng bước

### Bước 1: Go Sub Node gửi transactions

**File:** `mtn-simple-2025/cmd/simple_chain/config-sub-write.json`
```json
{
  "service_type": "SUB-WRITE",  ✅
  "mode": "Multi"                ✅
}
```

**Code:** `app.go:912-919`
```go
case common.ServiceTypeReadonly, common.ServiceTypeWrite:
    // ServiceTypeWrite = "SUB-WRITE"
    go app.blockProcessor.TxsProcessor2()  ✅ Chạy TxsProcessor2
```

**Code:** `block_processor.go:1984-2026`
```go
func (bp *BlockProcessor) TxsProcessor2() {
    rpcAddress := "127.0.0.1:10100"  ✅ Node 0 RPC port
    nodeID := 0                       ✅ Node 0 UDS socket
    
    if bp.txClient == nil && bp.chainState.GetConfig().Mode != p_common.MODE_SINGLE {
        bp.txClient, err = txsender.NewClient(rpcAddress, nodeID)  ✅
    }
    
    // Gửi transaction cho mode không phải SINGLE
    if bp.chainState.GetConfig().Mode != p_common.MODE_SINGLE {
        bp.txClient.SendTransaction(bTransaction)  ✅ Gửi đến Rust Node 0
    }
}
```

**Kết quả:** ✅ Go Sub Node gửi transactions đến Rust Node 0

### Bước 2: Rust Node 0 nhận và đồng thuận

**File:** `Mysticeti/metanode/config/enable_executor.toml`
```bash
$ ls -la config/enable_executor.toml
-rw-rw-r-- 1 abc abc 164 Dec 21 15:45 config/enable_executor.toml  ✅
```

**Code:** `node.rs:1117-1129`
```rust
let executor_enabled = is_executor_enabled(config_dir);  // Check: config/enable_executor.toml
let executor_client = if executor_enabled {
    let client = Arc::new(ExecutorClient::new(true, node_id));
    // Socket: /tmp/executor0.sock
    Some(client)  ✅ Executor enabled
} else {
    None
};
```

**Kết quả:** ✅ Rust Node 0 nhận transactions, xử lý consensus, và có executor client enabled

### Bước 3: Rust Node 0 gửi committed blocks

**Code:** `executor_client.rs:81-102`
```rust
pub async fn send_committed_subdag(&self, subdag: &CommittedSubDag, epoch: u64) -> Result<()> {
    // Convert to protobuf CommittedEpochData
    let epoch_data_bytes = self.convert_to_protobuf(subdag, epoch)?;
    
    // Send via UDS: /tmp/executor0.sock
    stream.write_all(&len_buf).await?;
    stream.write_all(&epoch_data_bytes).await?;  ✅ Gửi đến Go Master
}
```

**Kết quả:** ✅ Rust Node 0 gửi committed blocks đến Go Master qua `/tmp/executor0.sock`

### Bước 4: Go Master nhận và thực thi

**File:** `mtn-simple-2025/cmd/simple_chain/config-master.json`
```json
{
  "service_type": "MASTER",  ✅
  "mode": "Multi"             ✅
}
```

**Code:** `block_processor.go:192-186`
```go
if serviceType == p_common.ServiceTypeMaster {
    go bp.commitWorker()
    if bp.chainState.GetConfig().Mode != p_common.MODE_SINGLE {
        go bp.runSocketExecutor(0)  ✅ Socket ID = 0 (Node 0)
    }
}
```

**Code:** `block_processor.go:213-283`
```go
func (bp *BlockProcessor) runSocketExecutor(socketID int) {
    listener := executor.NewListener(socketID)  // socketID = 0 → /tmp/executor0.sock
    listener.Start()  ✅ Lắng nghe /tmp/executor0.sock
    
    for epochData := range dataChan {
        // Xử lý dữ liệu nhận được
        for _, block := range epochData.Blocks {
            for _, ms := range block.Transactions {
                // ms.Digest chứa raw transaction data
                transactions, err := transaction.UnmarshalTransactions(ms.Digest)  ✅
                allTransactions = append(allTransactions, transactions...)
            }
            
            // Process và execute transactions
            accumulatedResults, err := bp.transactionProcessor.ProcessTransactions(allTransactions)  ✅
            newBlock := bp.createBlockFromResults(accumulatedResults, currentBlockNumber, true, "single_block")  ✅
        }
    }
}
```

**Kết quả:** ✅ Go Master nhận committed blocks từ Rust Node 0 và thực thi transactions

## Tóm tắt kiểm tra

| Bước | Component | Config/Code | Status |
|------|-----------|-------------|--------|
| **1. Go Sub → Rust** | `config-sub-write.json` | `service_type: "SUB-WRITE"`, `mode: "Multi"` | ✅ |
| **1. Go Sub → Rust** | `app.go:919` | `go app.blockProcessor.TxsProcessor2()` | ✅ |
| **1. Go Sub → Rust** | `block_processor.go:1989` | `rpcAddress := "127.0.0.1:10100"`, `nodeID := 0` | ✅ |
| **2. Rust Node 0** | `config/enable_executor.toml` | File exists | ✅ |
| **2. Rust Node 0** | `node.rs:1120` | `executor_enabled = true` | ✅ |
| **3. Rust → Go** | `executor_client.rs:92` | `stream.write_all(&epoch_data_bytes)` | ✅ |
| **4. Go Master** | `config-master.json` | `service_type: "MASTER"`, `mode: "Multi"` | ✅ |
| **4. Go Master** | `block_processor.go:186` | `go bp.runSocketExecutor(0)` | ✅ |
| **4. Go Master** | `block_processor.go:252` | `transaction.UnmarshalTransactions(ms.Digest)` | ✅ |

## Kết luận

✅ **Luồng hoàn chỉnh và đúng:**

1. ✅ Go Sub Node (`config-sub-write.json`) gửi transactions đến Rust Node 0
2. ✅ Rust Node 0 nhận transactions, xử lý consensus, và commit blocks
3. ✅ Rust Node 0 gửi committed blocks đến Go Master qua `/tmp/executor0.sock`
4. ✅ Go Master Node (`config-master.json`) nhận và thực thi transactions

**Luồng hoàn chỉnh:**
```
Go Sub (SUB-WRITE) 
    ↓ Transactions → Rust Node 0
Rust Node 0 (Consensus + Executor)
    ↓ Committed Blocks → Go Master
Go Master (MASTER)
    ↓ Execution
State Updated
```

