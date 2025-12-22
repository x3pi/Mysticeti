# Hướng dẫn Triển khai: 1 Go Sub + 1 Go Master + 4 Rust Consensus Nodes

## Tổng quan

Hệ thống bao gồm:
- **4 Rust Consensus Nodes** (Node 0, 1, 2, 3)
  - Node 0: Full flow (Consensus + Executor)
  - Node 1, 2, 3: Consensus only
- **1 Go Sub Node**: Gửi transactions đến Rust Node 0
- **1 Go Master Node**: Nhận và thực thi blocks từ Rust Node 0

## Kiến trúc

```
┌─────────────────────────────────────────────────────────────────┐
│                    GO SUB NODE                                 │
│  ServiceType: Readonly/Write                                    │
│  Mode: != SINGLE                                               │
│                                                                 │
│  - TxsProcessor2() gửi transactions                            │
│  - Target: Rust Node 0                                         │
│    • UDS: /tmp/metanode-tx-0.sock                              │
│    • HTTP: http://127.0.0.1:10100/submit                       │
└───────────────────────┬─────────────────────────────────────────┘
                        │
                        │ Transactions
                        │
        ┌───────────────┴───────────────┐
        │                               │
        ▼                               ▼
┌───────────────────┐         ┌───────────────────┐
│  Rust Node 0      │         │  Rust Node 1      │
│  (Full Flow)      │         │  (Consensus Only) │
│                   │         │                   │
│  ✅ Consensus    │         │  ✅ Consensus     │
│  ✅ Executor      │         │  ❌ Executor      │
│                   │         │                   │
│  Socket:          │         │  Socket:          │
│  /tmp/executor0   │         │  (none)           │
│  .sock            │         │                   │
└───────┬───────────┘         └───────────────────┘
        │
        │ Committed Blocks
        │
        ▼
┌─────────────────────────────────────────────────────────────────┐
│                    GO MASTER NODE                               │
│  ServiceType: MASTER                                            │
│  Mode: Multi (!= SINGLE)                                       │
│                                                                 │
│  - runSocketExecutor(0) nhận blocks                             │
│  - Socket: /tmp/executor0.sock                                 │
│  - Process & Execute transactions                               │
└─────────────────────────────────────────────────────────────────┘

┌───────────────────┐         ┌───────────────────┐
│  Rust Node 2      │         │  Rust Node 3      │
│  (Consensus Only) │         │  (Consensus Only) │
│                   │         │                   │
│  ✅ Consensus     │         │  ✅ Consensus     │
│  ❌ Executor      │         │  ❌ Executor      │
└───────────────────┘         └───────────────────┘
```

## Bước 1: Cấu hình Rust Consensus Nodes

### 1.1. Tạo cấu hình cho 4 nodes

```bash
cd /home/abc/chain-new/Mysticeti/metanode

# Generate config cho 4 nodes
cargo run --release --bin metanode -- generate --nodes 4 --output config
```

**Kết quả:**
```
config/
├── committee_node_0.json
├── committee_node_1.json
├── committee_node_2.json
├── committee_node_3.json
├── node_0.toml
├── node_1.toml
├── node_2.toml
├── node_3.toml
└── enable_executor.toml  ← Chỉ node 0 có file này
```

### 1.2. Enable Executor cho Node 0

```bash
# Tạo file enable_executor.toml (chỉ node 0)
touch config/enable_executor.toml

# Hoặc nếu file đã tồn tại, đảm bảo chỉ node 0 có
ls -la config/enable_executor.toml
```

**File:** `config/enable_executor.toml`
```toml
# Enable executor for node 0
# This file enables sending committed blocks to Go executor via Unix Domain Socket
# Only node 0 should have this file
enabled = true
```

### 1.3. Kiểm tra cấu hình nodes

**Node 0:**
- File: `config/node_0.toml`
- Metrics port: `9100`
- RPC port: `10100` (metrics_port + 1000)
- Network port: `9000`
- Executor: ✅ Enabled (có file `enable_executor.toml`)

**Node 1, 2, 3:**
- Files: `config/node_1.toml`, `node_2.toml`, `node_3.toml`
- Metrics ports: `9101`, `9102`, `9103`
- RPC ports: `10101`, `10102`, `10103`
- Network ports: `9001`, `9002`, `9003`
- Executor: ❌ Disabled (không có file `enable_executor.toml`)

## Bước 2: Khởi động Rust Consensus Nodes

### 2.1. Khởi động tất cả 4 nodes

```bash
cd /home/abc/chain-new/Mysticeti/metanode

# Sử dụng script có sẵn
./scripts/node/run_nodes.sh

# Hoặc khởi động từng node thủ công:
# Node 0
cargo run --release --bin metanode -- start --config config/node_0.toml

# Node 1
cargo run --release --bin metanode -- start --config config/node_1.toml

# Node 2
cargo run --release --bin metanode -- start --config config/node_2.toml

# Node 3
cargo run --release --bin metanode -- start --config config/node_3.toml
```

### 2.2. Kiểm tra nodes đã khởi động

```bash
# Check processes
ps aux | grep metanode

# Check ports
netstat -tuln | grep -E "9000|9001|9002|9003|9100|9101|9102|9103|10100|10101|10102|10103"

# Check sockets
ls -la /tmp/metanode-tx-*.sock
ls -la /tmp/executor*.sock
```

**Expected output:**
```
/tmp/metanode-tx-0.sock  ← Node 0 transaction socket
/tmp/metanode-tx-1.sock  ← Node 1 transaction socket
/tmp/metanode-tx-2.sock  ← Node 2 transaction socket
/tmp/metanode-tx-3.sock  ← Node 3 transaction socket
/tmp/executor0.sock      ← Node 0 executor socket (chỉ node 0 có)
```

### 2.3. Kiểm tra logs

**Node 0:**
```
✅ Executor client enabled (node_id=0, socket=/tmp/executor0.sock)
RPC server started on http://127.0.0.1:10100
🔌 Transaction UDS server started on /tmp/metanode-tx-0.sock
```

**Node 1, 2, 3:**
```
ℹ️  Executor client disabled (node_id=X, consensus only - no enable_executor.toml)
RPC server started on http://127.0.0.1:1010X
🔌 Transaction UDS server started on /tmp/metanode-tx-X.sock
```

## Bước 3: Cấu hình Go Sub Node

### 3.1. Tạo file config cho Go Sub Node

**File:** `mtn-simple-2025/cmd/simple_chain/config_sub.json`

**Ví dụ config đầy đủ:**
```json
{
  "debug": true,
  "private_key": "YOUR_PRIVATE_KEY_HERE",
  "address": "YOUR_ADDRESS_HERE",
  "log_path": "./logs/sub",
  "backup_path": "./backup/sub",
  "connection_address": "0.0.0.0:4202",
  "version": "0.0.1.0",
  "node_type": "child_node",
  "service_type": "SUB-READ",  // hoặc "SUB-WRITE"
  "rpc_port": ":8647",
  "db_type": 1,
  "genesis_file_path": "genesis.json",
  "mode": "Multi",  // Quan trọng: != "SINGLE"
  "Databases": {
    "RootPath": "./data/sub",
    "DBEngine": "sharded",
    "NodeType": "STORAGE_LOCAL",
    "Version": "0.0.1.0",
    "BLSPrivateKey": "YOUR_BLS_PRIVATE_KEY",
    "AccountState": { "Path": "/account_state/" },
    "Trie": { "Path": "/trie_database/" },
    "SmartContractCode": { "Path": "/smart_contract_code/" },
    "SmartContractStorage": { "Path": "/smart_contract_storage/" },
    "Blocks": { "Path": "/blocks/" },
    "Receipts": { "Path": "/receipts/" },
    "TransactionState": { "Path": "/transaction_state/" }
  },
  "nodes": {
    "privateKey": "YOUR_NODE_PRIVATE_KEY",
    "master": "",
    "listen_port": 9004,
    "list_sub_node": [],
    "master_address": ""
  }
}
```

**Hoặc sử dụng config mẫu có sẵn:**
```bash
cd /home/abc/chain-new/mtn-simple-2025
cp cmd/simple_chain/config_sv/config-sub-1.json cmd/simple_chain/config_sub.json
# Sau đó chỉnh sửa các field cần thiết
```

### 3.2. Cấu hình ServiceType

**Quan trọng:** Đảm bảo `service_type` là `"SUB-READ"` hoặc `"SUB-WRITE"` (không phải `"MASTER"`)

**Giá trị ServiceType trong Go:**
- `"SUB-READ"` → Readonly node (chỉ đọc, có thể gửi transactions)
- `"SUB-WRITE"` → Write node (có thể gửi transactions)
- `"MASTER"` → Master node (thực thi blocks)

**Check trong code:**
```go
// app.go:912
case common.ServiceTypeReadonly, common.ServiceTypeWrite:
    // ServiceTypeReadonly = "SUB-READ"
    // ServiceTypeWrite = "SUB-WRITE"
    go app.blockProcessor.TxsProcessor2()  // ← Chỉ chạy khi ServiceType != Master
```

### 3.3. Cấu hình Target Rust Node

**File:** `block_processor.go:1989`

**Mặc định:** Đã hardcode gửi đến Node 0
```go
rpcAddress := "127.0.0.1:10100" // Node 0 RPC port
nodeID := 0                      // Node 0 UDS socket
```

**Không cần thay đổi** - đã đúng cấu hình.

## Bước 4: Khởi động Go Sub Node

### 4.1. Khởi động Go Sub Node

```bash
cd /home/abc/chain-new/mtn-simple-2025

# Build (nếu chưa build)
go build -o bin/simple_chain ./cmd/simple_chain

# Chạy với config sub node
./bin/simple_chain --config cmd/simple_chain/config_sub.json
```

### 4.2. Kiểm tra Go Sub Node

**Check log:**
```
App is running
TxsProcessor2: Giao dịch #2 đã gửi thành công.
```

**Check connection:**
```bash
# Check socket connection
ls -la /tmp/metanode-tx-0.sock

# Test gửi transaction (nếu có API)
curl -X POST http://127.0.0.1:4202/submit \
  -H "Content-Type: application/json" \
  -d '{"data": "test transaction"}'
```

## Bước 5: Cấu hình Go Master Node

### 5.1. Tạo file config cho Go Master Node

**File:** `mtn-simple-2025/cmd/simple_chain/config_master.json`

**Ví dụ config đầy đủ:**
```json
{
  "debug": true,
  "private_key": "YOUR_PRIVATE_KEY_HERE",
  "address": "YOUR_ADDRESS_HERE",
  "log_path": "./logs/master",
  "backup_path": "./backup/master",
  "connection_address": "0.0.0.0:4201",
  "version": "0.0.1.0",
  "node_type": "master_read_only",
  "service_type": "MASTER",  // Quan trọng: phải là "MASTER"
  "rpc_port": ":8646",
  "db_type": 1,
  "genesis_file_path": "genesis.json",
  "mode": "Multi",  // Quan trọng: != "SINGLE"
  "Databases": {
    "RootPath": "./data/master",
    "DBEngine": "sharded",
    "NodeType": "STORAGE_LOCAL",
    "Version": "0.0.1.0",
    "BLSPrivateKey": "YOUR_BLS_PRIVATE_KEY",
    "AccountState": { "Path": "/account_state/" },
    "Trie": { "Path": "/trie_database/" },
    "SmartContractCode": { "Path": "/smart_contract_code/" },
    "SmartContractStorage": { "Path": "/smart_contract_storage/" },
    "Blocks": { "Path": "/blocks/" },
    "Receipts": { "Path": "/receipts/" },
    "TransactionState": { "Path": "/transaction_state/" }
  },
  "nodes": {
    "privateKey": "YOUR_NODE_PRIVATE_KEY",
    "master": "",
    "listen_port": 9005,
    "list_sub_node": [],
    "master_address": ""
  }
}
```

**Lưu ý quan trọng:**
- `service_type`: Phải là `"MASTER"` (không phải `"SUB-READ"` hoặc `"SUB-WRITE"`)
- `mode`: Phải là `"Multi"` (không phải `"SINGLE"`)
- Code sẽ tự động nhận blocks từ Rust Node 0 qua `/tmp/executor0.sock`

**Hoặc sử dụng config mẫu có sẵn:**
```bash
cd /home/abc/chain-new/mtn-simple-2025
cp cmd/simple_chain/config_sv/config-master.json cmd/simple_chain/config_master.json
# Sau đó chỉnh sửa các field cần thiết
```

### 5.2. Cấu hình ServiceType

**Quan trọng:** Đảm bảo `service_type` là `"MASTER"`

**Check trong code:**
```go
// app.go:896
case common.ServiceTypeMaster:
    // ServiceTypeMaster = "MASTER"
    // ...

// block_processor.go:195
if serviceType == p_common.ServiceTypeMaster {
    go bp.commitWorker()
    if bp.chainState.GetConfig().Mode != p_common.MODE_SINGLE {
        go bp.runSocketExecutor(0)  // ← Chỉ chạy khi ServiceType == Master
    }
}
```

### 5.3. Cấu hình Socket ID

**File:** `block_processor.go:198`

**Mặc định:** Đã hardcode socket ID = 0 (Node 0)
```go
go bp.runSocketExecutor(0)  // Socket ID = 0 → /tmp/executor0.sock
```

**Không cần thay đổi** - đã đúng cấu hình.

## Bước 6: Khởi động Go Master Node

### 6.1. Khởi động Go Master Node

```bash
cd /home/abc/chain-new/mtn-simple-2025

# Chạy với config master node
./bin/simple_chain --config cmd/simple_chain/config_master.json
```

### 6.2. Kiểm tra Go Master Node

**Check log:**
```
App is running
Module Listener đang lắng nghe trên: /tmp/executor0.sock
Chương trình nhận được CommittedEpochData
```

**Check socket:**
```bash
ls -la /tmp/executor0.sock
# Output: socket exists
```

## Bước 7: Kiểm tra toàn bộ hệ thống

### 7.1. Kiểm tra tất cả components

```bash
# 1. Rust Nodes (4 nodes)
ps aux | grep metanode | grep -v grep
# Expected: 4 processes

# 2. Go Sub Node
ps aux | grep simple_chain | grep -v grep | grep -v master
# Expected: 1 process (sub node)

# 3. Go Master Node
ps aux | grep simple_chain | grep -v grep | grep master
# Expected: 1 process (master node)

# 4. Sockets
ls -la /tmp/metanode-tx-*.sock
ls -la /tmp/executor*.sock
```

### 7.2. Kiểm tra luồng giao dịch

**1. Go Sub Node gửi transaction:**
```bash
# Check log Go Sub Node
tail -f /path/to/go-sub-node.log | grep "Giao dịch"
# Expected: "Giao dịch #2 đã gửi thành công."
```

**2. Rust Node 0 nhận transaction:**
```bash
# Check log Rust Node 0
tail -f /path/to/rust-node-0.log | grep "Transaction submitted"
# Expected: "📤 Transaction submitted via UDS: hash=..."
```

**3. Rust Node 0 commit và gửi block:**
```bash
# Check log Rust Node 0
tail -f /path/to/rust-node-0.log | grep "Sent committed sub-DAG"
# Expected: "📤 Sent committed sub-DAG (commit_index=..., blocks=...) to executor"
```

**4. Go Master nhận và thực thi:**
```bash
# Check log Go Master
tail -f /path/to/go-master.log | grep "Chương trình nhận được"
# Expected: "Chương trình nhận được CommittedEpochData"
```

### 7.3. Test end-to-end

**1. Gửi transaction từ Go Sub Node:**
```bash
# Nếu có API endpoint
curl -X POST http://127.0.0.1:4202/submit \
  -H "Content-Type: application/json" \
  -d '{"data": "test transaction"}'
```

**2. Kiểm tra transaction được commit:**
```bash
# Check Rust Node 0 log
tail -f rust-node-0.log | grep "Executing commit"
```

**3. Kiểm tra transaction được thực thi:**
```bash
# Check Go Master log
tail -f go-master.log | grep "ProcessTransactions"
```

## Cấu hình chi tiết

### Rust Node 0

**File:** `config/node_0.toml`
```toml
node_id = 0
network_address = "127.0.0.1:9000"
metrics_port = 9100
# RPC port = 9100 + 1000 = 10100
```

**Executor:**
- File: `config/enable_executor.toml` ✅
- Socket: `/tmp/executor0.sock`

### Rust Node 1, 2, 3

**Files:** `config/node_1.toml`, `node_2.toml`, `node_3.toml`
```toml
node_id = 1  # hoặc 2, 3
network_address = "127.0.0.1:9001"  # hoặc 9002, 9003
metrics_port = 9101  # hoặc 9102, 9103
```

**Executor:**
- File: `config/enable_executor.toml` ❌ (không có)

### Go Sub Node

**File:** `config_sub.json`
```json
{
  "ServiceType": "Readonly",  // hoặc "Write"
  "Mode": "Multi"             // != "SINGLE"
}
```

**Target Rust Node:**
- RPC: `127.0.0.1:10100` (Node 0)
- UDS: `/tmp/metanode-tx-0.sock` (Node 0)

### Go Master Node

**File:** `config_master.json`
```json
{
  "ServiceType": "Master",
  "Mode": "Multi"  // != "SINGLE"
}
```

**Source Rust Node:**
- Socket: `/tmp/executor0.sock` (Node 0)

## Ports và Sockets

| Component | Port/Socket | Description |
|-----------|-------------|-------------|
| **Rust Node 0** | Network | `9000` | Consensus network |
| **Rust Node 0** | Metrics | `9100` | Prometheus metrics |
| **Rust Node 0** | RPC | `10100` | HTTP RPC server |
| **Rust Node 0** | UDS TX | `/tmp/metanode-tx-0.sock` | Transaction submission |
| **Rust Node 0** | UDS Exec | `/tmp/executor0.sock` | Block execution |
| **Rust Node 1** | Network | `9001` | Consensus network |
| **Rust Node 1** | Metrics | `9101` | Prometheus metrics |
| **Rust Node 1** | RPC | `10101` | HTTP RPC server |
| **Rust Node 1** | UDS TX | `/tmp/metanode-tx-1.sock` | Transaction submission |
| **Rust Node 2** | Network | `9002` | Consensus network |
| **Rust Node 2** | Metrics | `9102` | Prometheus metrics |
| **Rust Node 2** | RPC | `10102` | HTTP RPC server |
| **Rust Node 2** | UDS TX | `/tmp/metanode-tx-2.sock` | Transaction submission |
| **Rust Node 3** | Network | `9003` | Consensus network |
| **Rust Node 3** | Metrics | `9103` | Prometheus metrics |
| **Rust Node 3** | RPC | `10103` | HTTP RPC server |
| **Rust Node 3** | UDS TX | `/tmp/metanode-tx-3.sock` | Transaction submission |
| **Go Sub Node** | Connection | `4202` | P2P connection (nếu có) |
| **Go Master Node** | Connection | `4201` | P2P connection (nếu có) |

## Scripts hỗ trợ

### Khởi động tất cả Rust nodes

```bash
cd /home/abc/chain-new/Mysticeti/metanode
./scripts/node/run_nodes.sh
```

### Dừng tất cả Rust nodes

```bash
cd /home/abc/chain-new/Mysticeti/metanode
./scripts/node/stop_nodes.sh
```

### Khởi động lại một node

```bash
cd /home/abc/chain-new/Mysticeti/metanode
./scripts/node/restart_node.sh 0  # Node 0
```

### Kiểm tra trạng thái

```bash
cd /home/abc/chain-new/Mysticeti/metanode
./scripts/analysis/check_epoch_status.sh
```

## Troubleshooting

### 1. Go Sub Node không gửi được transactions

**Kiểm tra:**
```bash
# Check Rust Node 0 đang chạy
ps aux | grep metanode | grep node_0

# Check socket
ls -la /tmp/metanode-tx-0.sock

# Check RPC port
curl http://127.0.0.1:10100/ready
```

**Fix:**
- Đảm bảo Rust Node 0 đang chạy
- Check `ServiceType` trong Go config (phải là `Readonly` hoặc `Write`)
- Check `Mode` trong Go config (phải là `Multi`, không phải `SINGLE`)

### 2. Go Master không nhận được blocks

**Kiểm tra:**
```bash
# Check Rust Node 0 executor enabled
ls -la config/enable_executor.toml

# Check socket
ls -la /tmp/executor0.sock

# Check Go Master service_type
grep service_type config_master.json
```

**Fix:**
- Đảm bảo file `config/enable_executor.toml` tồn tại trong thư mục `config/`
- Check `service_type` trong Go config (phải là `"MASTER"`, không phải `"SUB-READ"` hoặc `"SUB-WRITE"`)
- Check `mode` trong Go config (phải là `"Multi"`, không phải `"SINGLE"`)
- Check log Go Master: `tail -f go-master.log | grep "runSocketExecutor"`
- Check log Rust Node 0: `tail -f rust-node-0.log | grep "Executor client enabled"`

### 3. Rust nodes không đồng thuận

**Kiểm tra:**
```bash
# Check tất cả nodes đang chạy
ps aux | grep metanode

# Check ports không bị conflict
netstat -tuln | grep -E "9000|9001|9002|9003"

# Check logs
tail -f rust-node-*.log | grep -i error
```

**Fix:**
- Đảm bảo tất cả 4 nodes đang chạy
- Check network ports không bị conflict
- Check `committee_node_X.json` files đúng format

## Tóm tắt

✅ **Cấu hình đúng:**

1. **Go Sub Node:**
   - `ServiceType`: `Readonly` hoặc `Write`
   - Gửi transactions đến Rust Node 0 (`127.0.0.1:10100`)

2. **Rust Node 0:**
   - Có file `config/enable_executor.toml`
   - Executor client enabled
   - Gửi blocks đến Go master qua `/tmp/executor0.sock`

3. **Go Master Node:**
   - `ServiceType`: `Master`
   - Nhận blocks từ Rust Node 0 qua `/tmp/executor0.sock`

4. **Rust Node 1, 2, 3:**
   - Chỉ consensus (không có executor)
   - Tham gia đồng thuận với Node 0

**Luồng hoàn chỉnh:**
```
Go Sub → Rust Node 0 (Consensus) → Go Master (Execution)
         + Rust Node 1, 2, 3 (Consensus)
```

