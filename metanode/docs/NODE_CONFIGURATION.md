# Node Configuration: Node 0 Full Flow vs Other Nodes Consensus Only

## Tổng quan

Hệ thống được cấu hình để:
- **Node 0**: Chạy full luồng (Consensus + Executor) - mặc định
- **Node 1, 2, 3**: Chỉ chạy Consensus (không có Executor)

**Lưu ý:** Executor node có thể thay đổi bằng cách di chuyển file `enable_executor.toml` sang node khác.

## Cấu hình Rust (MetaNode)

### Node 0: Full Flow (Mặc định)

**Files cần có:**
```
Mysticeti/metanode/config/enable_executor.toml  ← File này enable executor
Mysticeti/metanode/config/committee_node_0.json
```

**Code check:**
```rust
// node.rs:1117
// Check if executor is enabled via config file (config/enable_executor.toml)
// Only the node with this file will have executor enabled
// Default: only node 0 has this file, but can be changed to any node
let config_dir = self.committee_path.parent().unwrap_or_else(|| std::path::Path::new("config"));
let executor_enabled = is_executor_enabled(config_dir);

let executor_client = if executor_enabled {
    let client = Arc::new(ExecutorClient::new(true, node_id));
    Some(client)  // Node có file enable_executor.toml
} else {
    None  // Các node khác không có
};
```

**Executor Client:**
- Enabled: ✅ Có file `config/enable_executor.toml` trong thư mục config
- Socket: `/tmp/executor{node_id}.sock` (ví dụ: node 0 → `/tmp/executor0.sock`)
- Gửi committed blocks đến Go executor

### Node 1, 2, 3: Consensus Only

**Files cần có:**
```
Mysticeti/metanode/config/committee_node_1.json  ← Không có enable_executor.toml
Mysticeti/metanode/config/committee_node_2.json
Mysticeti/metanode/config/committee_node_3.json
```

**Code check:**
```rust
// node.rs:1117
// Check if executor is enabled via config file
let config_dir = self.committee_path.parent().unwrap_or_else(|| std::path::Path::new("config"));
let executor_enabled = is_executor_enabled(config_dir);  // Check file: config/enable_executor.toml

let executor_client = if executor_enabled {
    // Không vào đây (file không tồn tại)
} else {
    None  // Node 1, 2, 3 không có executor client (không có file)
};
```

**Executor Client:**
- Enabled: ❌ Không có file `config/enable_executor.toml`
- Socket: Không tạo
- Không gửi committed blocks

## Cấu hình Go (mtn-simple-2025)

### Node 0: Master Executor

**Service Type:** `ServiceTypeMaster`
**Mode:** `!= MODE_SINGLE`

**Code:**
```go
// block_processor.go:195
if serviceType == p_common.ServiceTypeMaster {
    go bp.commitWorker()
    if bp.chainState.GetConfig().Mode != p_common.MODE_SINGLE {
        go bp.runSocketExecutor(0)  // ← Chỉ node 0 chạy executor
    }
}
```

**Executor Listener:**
- Enabled: ✅ Chạy `runSocketExecutor(0)`
- Socket: `/tmp/executor0.sock`
- Nhận committed blocks từ Rust

### Node 1, 2, 3: Sub Nodes

**Service Type:** `ServiceTypeReadonly` hoặc `ServiceTypeWrite`
**Mode:** `!= MODE_SINGLE`

**Code:**
```go
// app.go:912
case common.ServiceTypeReadonly, common.ServiceTypeWrite:
    // ...
    go app.blockProcessor.TxsProcessor2()  // ← Chỉ gửi transactions
    // Không chạy runSocketExecutor()
}
```

**Executor Listener:**
- Enabled: ❌ Không chạy `runSocketExecutor()`
- Socket: Không tạo
- Không nhận committed blocks

## Luồng hoạt động

### Node 0 (Full Flow)

```
┌─────────────────────────────────────────┐
│         Node 0 (Master)                  │
│                                         │
│  Rust MetaNode:                         │
│    ✅ Consensus (nhận transactions)      │
│    ✅ Executor Client (gửi blocks)      │
│                                         │
│  Go Master:                             │
│    ✅ Executor Listener (nhận blocks)   │
│    ✅ Process & Execute transactions    │
└─────────────────────────────────────────┘
```

**Luồng:**
1. Nhận transactions từ sub nodes (qua UDS/HTTP)
2. Consensus xử lý và commit blocks
3. Executor client gửi committed blocks → Go executor
4. Go executor thực thi transactions

### Node 1, 2, 3 (Consensus Only)

```
┌─────────────────────────────────────────┐
│      Node 1/2/3 (Sub Nodes)             │
│                                         │
│  Rust MetaNode:                         │
│    ✅ Consensus (nhận transactions)      │
│    ❌ Executor Client (không có)         │
│                                         │
│  Go Sub Node:                           │
│    ✅ TxsProcessor2 (gửi transactions)  │
│    ❌ Executor Listener (không có)      │
└─────────────────────────────────────────┘
```

**Luồng:**
1. Go sub node gửi transactions → Rust consensus (node 0 hoặc chính nó)
2. Rust consensus xử lý và commit blocks
3. Không gửi blocks đến executor (không có executor client)

## Kiểm tra cấu hình

### 1. Kiểm tra Rust Executor

```bash
# Node 0: Có file enable_executor.toml
ls -la Mysticeti/metanode/config/enable_executor.toml
# Output: enable_executor.toml exists

# Node 1, 2, 3: Không có file
ls -la Mysticeti/metanode/config/enable_executor.toml
# Output: No such file or directory
```

**Hoặc check trong log khi khởi động:**
- Node 0: `✅ Executor client enabled (node_id=0, socket=/tmp/executor0.sock)`
- Node 1, 2, 3: `ℹ️  Executor client disabled (node_id=X, consensus only - no enable_executor.toml)`

### 2. Kiểm tra Go Executor

```bash
# Node 0: ServiceType = Master
# Check trong config hoặc log

# Node 1, 2, 3: ServiceType = Readonly/Write
# Check trong config hoặc log
```

### 3. Kiểm tra Sockets

```bash
# Node 0: Có executor socket
ls -la /tmp/executor0.sock
# Output: socket exists (khi Go executor đang chạy)

# Node 1, 2, 3: Không có executor socket
ls -la /tmp/executor1.sock
# Output: No such file or directory
```

### 4. Kiểm tra Logs

**Node 0 (Rust):**
```
✅ Executor client enabled for epoch transition (node_id=0, socket=/tmp/executor0.sock)
📤 Sent committed sub-DAG (commit_index=..., blocks=...) to executor
```

**Node 1, 2, 3 (Rust):**
```
ℹ️  Executor client disabled (node_id=1, consensus only - no enable_executor.toml)
(Không có log về executor client)
```

**Node 0 (Go):**
```
Module Listener đang lắng nghe trên: /tmp/executor0.sock
Chương trình nhận được CommittedEpochData
```

**Node 1, 2, 3 (Go):**
```
(Không có log về executor listener)
```

## Cấu hình mặc định

### Node 0

**Rust:**
- File: `config/enable_executor.toml` ✅
- Executor client: Enabled ✅
- Socket: `/tmp/executor0.sock`

**Go:**
- ServiceType: `Master` ✅
- Executor listener: Enabled ✅
- Socket: `/tmp/executor0.sock`

### Node 1, 2, 3

**Rust:**
- File: `config/enable_executor.toml` ❌
- Executor client: Disabled ❌
- Socket: Không tạo

**Go:**
- ServiceType: `Readonly` hoặc `Write` ✅
- Executor listener: Disabled ❌
- Socket: Không tạo

## Thay đổi Executor Node

### Cách thay đổi executor từ node 0 sang node khác

**Ví dụ: Chuyển executor từ node 0 sang node 2**

1. **Rust (MetaNode):**
   ```bash
   # Xóa file ở node 0
   rm Mysticeti/metanode/config/enable_executor.toml
   
   # Tạo file ở node 2 (nếu có cấu trúc thư mục riêng)
   # Hoặc di chuyển file sang thư mục config của node 2
   touch Mysticeti/metanode/config/enable_executor.toml
   # (Code check file trong cùng thư mục với committee_node_X.json)
   ```

2. **Go (mtn-simple-2025):**
   ```go
   // Thay đổi ServiceType trong config
   // Node 0: ServiceType = Readonly/Write
   // Node 2: ServiceType = Master
   
   // Và thay đổi socketID trong runSocketExecutor()
   go bp.runSocketExecutor(2)  // Thay vì runSocketExecutor(0)
   ```

**Lưu ý:**
- File `enable_executor.toml` chỉ cần tồn tại (nội dung không quan trọng)
- Code check file trong thư mục `config/` (cùng thư mục với `committee_node_X.json`)
- Nếu tất cả nodes dùng chung thư mục `config/`, thì chỉ một node có thể enable executor
- Nếu mỗi node có thư mục config riêng, có thể enable executor cho nhiều node (nhưng không khuyến khích)

## Transaction Routing

### Sub Nodes gửi transactions

**Code:**
```go
// block_processor.go:1989
rpcAddress := "127.0.0.1:10100" // Node 0 RPC port
nodeID := 0 // Node ID để xác định socket path
bp.txClient, err = txsender.NewClient(rpcAddress, nodeID)
```

**Mặc định:** Tất cả sub nodes gửi transactions đến Node 0

**Có thể config:** Mỗi sub node gửi đến node consensus riêng (nếu cần)

## Tóm tắt

| Node | Rust Consensus | Rust Executor | Go Sub Node | Go Executor |
|------|---------------|---------------|-------------|-------------|
| **0** | ✅ | ✅ (có file) | ❌ | ✅ (Master) |
| **1** | ✅ | ❌ (không có file) | ✅ | ❌ |
| **2** | ✅ | ❌ (không có file) | ✅ | ❌ |
| **3** | ✅ | ❌ (không có file) | ✅ | ❌ |

**Kết luận:**
- ✅ Node 0: Full flow (Consensus + Executor) - mặc định
- ✅ Node 1, 2, 3: Chỉ Consensus (không có Executor)
- ✅ Có thể thay đổi executor node bằng cách di chuyển file `enable_executor.toml`
