# Cấu hình Tóm tắt: Go Sub → Rust Node 0 → Go Master

## Luồng mong muốn

```
┌─────────────────────────────────────────────────────────────┐
│  Go Sub Nodes (1, 2, 3)                                     │
│  ServiceType: Readonly/Write                                │
│                                                             │
│  TxsProcessor2()                                            │
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
│  ✅ Consensus (nhận transactions từ Go)                    │
│  ✅ Executor Client (gửi blocks về Go)                      │
│    - File: config/enable_executor.toml                      │
│    - Socket: /tmp/executor0.sock                            │
└───────────────────────┬─────────────────────────────────────┘
                        │
                        │ Committed Blocks (protobuf)
                        │
                        ▼
┌─────────────────────────────────────────────────────────────┐
│  Go Master Node                                             │
│  ServiceType: Master                                        │
│                                                             │
│  runSocketExecutor(0)                                       │
│    ↓                                                        │
│  Nhận blocks từ Rust Node 0                                 │
│    - Socket: /tmp/executor0.sock                           │
│    ↓                                                        │
│  Process & Execute transactions                             │
└─────────────────────────────────────────────────────────────┘
```

## Cấu hình chi tiết

### 1. Go Sub Nodes → Rust Node 0 (Transaction Submission)

**File:** `mtn-simple-2025/cmd/simple_chain/processor/block_processor.go`

**Code:**
```go
// block_processor.go:1989
rpcAddress := "127.0.0.1:10100" // Node 0 RPC port = 9100 + 1000
nodeID := 0 // Node ID để xác định UDS socket path

bp.txClient, err = txsender.NewClient(rpcAddress, nodeID)
// Client sẽ:
// - Ưu tiên: UDS /tmp/metanode-tx-0.sock
// - Fallback: HTTP http://127.0.0.1:10100/submit
```

**Chạy khi:**
- `ServiceType == Readonly` hoặc `ServiceType == Write`
- `Mode != SINGLE`
- Function: `TxsProcessor2()`

**Kết quả:**
- ✅ Tất cả Go sub nodes gửi transactions đến Rust Node 0

### 2. Rust Node 0 (Consensus + Executor)

**File:** `Mysticeti/metanode/src/node.rs`**

**Executor Client:**
```rust
// node.rs:1117
let config_dir = self.committee_path.parent().unwrap_or_else(|| std::path::Path::new("config"));
let executor_enabled = is_executor_enabled(config_dir);  // Check: config/enable_executor.toml

let executor_client = if executor_enabled {
    let client = Arc::new(ExecutorClient::new(true, node_id));
    // Socket: /tmp/executor0.sock
    Some(client)
} else {
    None
};
```

**Cấu hình:**
- File: `Mysticeti/metanode/config/enable_executor.toml` ✅ (chỉ node 0 có)
- Executor client: Enabled ✅
- Socket: `/tmp/executor0.sock`

**Kết quả:**
- ✅ Rust Node 0 nhận transactions từ Go sub nodes
- ✅ Rust Node 0 xử lý consensus và commit blocks
- ✅ Rust Node 0 gửi committed blocks đến Go executor

### 3. Rust Node 0 → Go Master (Block Execution)

**File:** `Mysticeti/metanode/src/executor_client.rs`

**Code:**
```rust
// executor_client.rs
pub async fn send_committed_subdag(&self, subdag: &CommittedSubDag, epoch: u64) -> Result<()> {
    // Convert to protobuf CommittedEpochData
    let epoch_data_bytes = self.convert_to_protobuf(subdag, epoch)?;
    
    // Send via UDS: /tmp/executor0.sock
    stream.write_all(&len_buf).await?;
    stream.write_all(&epoch_data_bytes).await?;
}
```

**Kết quả:**
- ✅ Rust Node 0 gửi committed blocks đến Go master qua `/tmp/executor0.sock`

### 4. Go Master nhận và thực thi

**File:** `mtn-simple-2025/cmd/simple_chain/processor/block_processor.go`

**Code:**
```go
// block_processor.go:195
if serviceType == p_common.ServiceTypeMaster {
    go bp.commitWorker()
    if bp.chainState.GetConfig().Mode != p_common.MODE_SINGLE {
        go bp.runSocketExecutor(0)  // ← Nhận từ /tmp/executor0.sock
    }
}
```

**Chạy khi:**
- `ServiceType == Master`
- `Mode != SINGLE`
- Function: `runSocketExecutor(0)`

**Kết quả:**
- ✅ Go master nhận committed blocks từ Rust Node 0
- ✅ Go master process và execute transactions

## Kiểm tra cấu hình

### 1. Go Sub Nodes gửi đến Node 0

**Check code:**
```go
// block_processor.go:1989
rpcAddress := "127.0.0.1:10100" // ✅ Node 0
nodeID := 0                      // ✅ Node 0
```

**Check log:**
```
Giao dịch #2 đã gửi thành công.
```

### 2. Rust Node 0 có Executor

**Check file:**
```bash
ls -la Mysticeti/metanode/config/enable_executor.toml
# Output: enable_executor.toml exists ✅
```

**Check log:**
```
✅ Executor client enabled for epoch transition (node_id=0, socket=/tmp/executor0.sock)
📤 Sent committed sub-DAG (commit_index=..., blocks=...) to executor
```

### 3. Go Master nhận từ Node 0

**Check code:**
```go
// block_processor.go:198
go bp.runSocketExecutor(0)  // ✅ Socket ID = 0 (Node 0)
```

**Check log:**
```
Module Listener đang lắng nghe trên: /tmp/executor0.sock
Chương trình nhận được CommittedEpochData
```

## Tóm tắt cấu hình

| Component | Config | Value | Status |
|-----------|--------|-------|--------|
| **Go Sub → Rust** | `rpcAddress` | `127.0.0.1:10100` | ✅ Node 0 |
| **Go Sub → Rust** | `nodeID` | `0` | ✅ Node 0 |
| **Rust Node 0** | `enable_executor.toml` | File exists | ✅ Enabled |
| **Rust → Go** | `socket_id` | `0` | ✅ Node 0 |
| **Go Master** | `runSocketExecutor` | `0` | ✅ Node 0 |

## Kết luận

✅ **Cấu hình đúng như mong muốn:**

1. ✅ Go sub nodes gửi transactions đến Rust Node 0
2. ✅ Rust Node 0 xử lý consensus và commit blocks
3. ✅ Rust Node 0 gửi committed blocks về Go master để thực thi

**Luồng hoàn chỉnh:**
```
Go Sub (1,2,3) → Rust Node 0 (Consensus) → Go Master (Execution)
```

