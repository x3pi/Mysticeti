# Quick Start: Triển khai 1 Go Sub + 1 Go Master + 4 Rust Nodes

## Tổng quan nhanh

Hệ thống gồm:
- **4 Rust Consensus Nodes** (Node 0, 1, 2, 3)
- **1 Go Sub Node** → Gửi transactions đến Rust Node 0
- **1 Go Master Node** ← Nhận blocks từ Rust Node 0

## Bước 1: Khởi động Rust Nodes (4 nodes)

```bash
cd /home/abc/chain-new/Mysticeti/metanode

# 1. Generate config (nếu chưa có)
cargo run --release --bin metanode -- generate --nodes 4 --output config

# 2. Enable executor cho Node 0
touch config/enable_executor.toml

# 3. Khởi động tất cả nodes
./scripts/node/run_nodes.sh
```

**Kiểm tra:**
```bash
# Check processes
ps aux | grep metanode | grep -v grep
# Expected: 4 processes

# Check sockets
ls -la /tmp/metanode-tx-*.sock
ls -la /tmp/executor0.sock  # Chỉ node 0 có
```

## Bước 2: Cấu hình và khởi động Go Sub Node

### 2.1. Tạo config file

**File:** `mtn-simple-2025/cmd/simple_chain/config_sub.json`

```json
{
  "service_type": "SUB-READ",
  "mode": "Multi",
  "connection_address": "0.0.0.0:4202",
  "Databases": {
    "RootPath": "./data/sub",
    "DBEngine": "sharded"
  }
}
```

**Hoặc copy từ mẫu:**
```bash
cd /home/abc/chain-new/mtn-simple-2025
cp cmd/simple_chain/config_sv/config-sub-1.json cmd/simple_chain/config_sub.json
# Chỉnh sửa: "service_type": "SUB-READ" hoặc "SUB-WRITE"
```

### 2.2. Khởi động Go Sub Node

```bash
cd /home/abc/chain-new/mtn-simple-2025
go run . -config=config_sub.json
```

**Kiểm tra log:**
```
App is running
Giao dịch #2 đã gửi thành công.
```

## Bước 3: Cấu hình và khởi động Go Master Node

### 3.1. Tạo config file

**File:** `mtn-simple-2025/cmd/simple_chain/config_master.json`

```json
{
  "service_type": "MASTER",
  "mode": "Multi",
  "connection_address": "0.0.0.0:4201",
  "Databases": {
    "RootPath": "./data/master",
    "DBEngine": "sharded"
  }
}
```

**Hoặc copy từ mẫu:**
```bash
cd /home/abc/chain-new/mtn-simple-2025
cp cmd/simple_chain/config_sv/config-master.json cmd/simple_chain/config_master.json
# Chỉnh sửa: "service_type": "MASTER"
```

### 3.2. Khởi động Go Master Node

```bash
cd /home/abc/chain-new/mtn-simple-2025
go run . -config=config_master.json
```

**Kiểm tra log:**
```
App is running
Module Listener đang lắng nghe trên: /tmp/executor0.sock
Chương trình nhận được CommittedEpochData
```

## Bước 4: Kiểm tra toàn bộ hệ thống

### 4.1. Check tất cả components

```bash
# Rust nodes
ps aux | grep metanode | grep -v grep
# Expected: 4 processes

# Go Sub Node
ps aux | grep "simple_chain.*config_sub" | grep -v grep
# Expected: 1 process

# Go Master Node
ps aux | grep "simple_chain.*config_master" | grep -v grep
# Expected: 1 process
```

### 4.2. Check luồng giao dịch

**1. Go Sub gửi transaction:**
```bash
# Check log Go Sub
tail -f go-sub.log | grep "Giao dịch"
```

**2. Rust Node 0 nhận và commit:**
```bash
# Check log Rust Node 0
tail -f rust-node-0.log | grep "Executing commit"
```

**3. Go Master nhận và thực thi:**
```bash
# Check log Go Master
tail -f go-master.log | grep "Chương trình nhận được"
```

## Cấu hình tóm tắt

| Component | ServiceType | Target/Source | Socket/Port |
|-----------|-------------|---------------|-------------|
| **Go Sub** | `SUB-READ` hoặc `SUB-WRITE` | Rust Node 0 | `/tmp/metanode-tx-0.sock` / `:10100` |
| **Rust Node 0** | - | Go Master | `/tmp/executor0.sock` |
| **Go Master** | `MASTER` | Rust Node 0 | `/tmp/executor0.sock` |
| **Rust Node 1,2,3** | - | - | Chỉ consensus |

## Troubleshooting nhanh

### Go Sub không gửi được transactions
- Check `service_type` = `"SUB-READ"` hoặc `"SUB-WRITE"`
- Check `mode` = `"Multi"` (không phải `"SINGLE"`)
- Check Rust Node 0 đang chạy: `ps aux | grep metanode`

### Go Master không nhận được blocks
- Check `service_type` = `"MASTER"`
- Check `mode` = `"Multi"` (không phải `"SINGLE"`)
- Check file `config/enable_executor.toml` tồn tại
- Check socket: `ls -la /tmp/executor0.sock`

### Rust nodes không đồng thuận
- Check tất cả 4 nodes đang chạy
- Check ports không conflict: `netstat -tuln | grep -E "9000|9001|9002|9003"`

## Xem thêm

- Chi tiết đầy đủ: `docs/DEPLOYMENT_GUIDE.md`
- Cấu hình nodes: `docs/NODE_CONFIGURATION.md`
- Luồng giao dịch: `docs/TRANSACTION_FLOW.md`

