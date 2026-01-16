# Mysticeti Blockchain - Hệ thống Blockchain lai Go/Rust

## Tổng quan

Mysticeti là hệ thống blockchain lai sử dụng kiến trúc hybrid Go/Rust với:
- **Go Master**: Quản lý state và execute transactions
- **Rust Metanodes**: Chạy consensus algorithm và tạo blocks
- **Go Sub**: Client applications submit transactions

Hệ thống hỗ trợ dynamic node roles: nodes có thể chuyển đổi giữa **Sync-only** (chỉ đồng bộ) và **Validator** (tham gia consensus) dựa trên committee membership.

## Kiến trúc tổng thể

```
┌─────────────────┐    ┌──────────────────┐    ┌─────────────────┐
│     Go Sub      │────│  Rust Metanodes  │────│    Go Master    │
│ (Applications)  │    │  (Consensus)     │    │   (Execution)   │
└─────────────────┘    └──────────────────┘    └─────────────────┘
        │                        │                        │
        ▼                        ▼                        ▼
   Submit txs ───────────►    Create blocks ───────────► Execute txs
   (RPC/Unix)            DAG Consensus              State updates
```

## Components chính

### 1. Go Master (Go Executor)
- **Chức năng**: Execute transactions, manage state, persist data
- **Cổng**: Unix socket (`/tmp/rust-go.sock_1`)
- **Nhiệm vụ**:
  - Nhận committed blocks từ Rust
  - Execute transactions theo thứ tự
  - Update account balances, contract state
  - Persist to database

### 2. Rust Metanodes (Consensus Nodes)
- **Chức năng**: Chạy consensus, tạo và commit blocks
- **2 chế độ hoạt động**:
  - **Validator**: Tham gia consensus đầy đủ
  - **Sync-only**: Chỉ đồng bộ data, không tạo blocks

### 3. Go Sub (Client Applications)
- **Chức năng**: Submit transactions tới mạng
- **Kết nối**: RPC HTTP hoặc Unix Domain Socket
- **Ví dụ**: Wallets, dApps, smart contract interactions

## Quá trình tạo block

### Phase 1: Submit Transactions
```
Go Sub ──RPC──► Rust Metanode ──► Consensus Pool
```
1. Go Sub tạo transaction (transfer, contract call, etc.)
2. Submit qua RPC HTTP (port 9100-9104) hoặc Unix socket
3. Transaction vào consensus pool của validator nodes

### Phase 2: Consensus & Block Creation
```
Consensus Pool ──► DAG Consensus ──► Block Creation ──► Commit
```
1. **Leader election**: Chọn leader cho round
2. **Block proposal**: Leader tạo block chứa transactions
3. **Voting**: Nodes vote cho block
4. **Finalization**: Quorum certificates
5. **Commit**: Block được commit và broadcast

### Phase 3: Execution
```
Committed Block ──Unix Socket──► Go Master ──► State Update
```
1. Commit processor gửi block tới Go Master
2. Go Master execute transactions theo thứ tự
3. Update state và persist data
4. Return confirmation

## Node Roles & Dynamic Switching

### Validator Nodes
- **Điều kiện**: Node nằm trong committee (được bầu làm validator)
- **Chức năng**:
  - Tham gia consensus đầy đủ
  - Tạo và vote blocks
  - Gửi committed blocks tới Go Master
  - Chạy RPC/UDS servers để nhận transactions
- **Config**: `initial_node_mode = "Validator"` (tùy chọn)

### Sync-only Nodes
- **Điều kiện**: Node không nằm trong committee
- **Chức năng**:
  - Đồng bộ data từ Go Master
  - Không tạo blocks hoặc tham gia voting
  - Không chạy RPC/UDS servers
- **Config**: `initial_node_mode = "SyncOnly"` (tùy chọn)

### Epoch Transitions
Mỗi epoch kết thúc, committee có thể thay đổi:
1. **EndOfEpoch transaction** được tạo
2. **Committee mới** được fetch từ Go Master
3. **Node roles cập nhật**:
   - Node mới vào committee → Chuyển `SyncOnly` → `Validator`
   - Node bị loại khỏi committee → Chuyển `Validator` → `SyncOnly`
4. **Authority recreation**: Validator mới tạo consensus authority

## Configuration

### Node Configuration (node_*.toml)
```toml
[network]
node_id = 0
network_address = "127.0.0.1:9000"
metrics_port = 9100

[consensus]
initial_node_mode = "Validator"  # "Validator" hoặc "SyncOnly"
db_path = "config/storage/node_0/consensus_db"

[executor]
send_socket_path = "/tmp/executor0.sock"
receive_socket_path = "/tmp/rust-go.sock_1"
executor_commit_enabled = true
executor_read_enabled = true

[epoch_transition]
epoch_transition_optimization = "balanced"
enable_gradual_shutdown = true
```

### Committee Configuration
- Committee được quản lý bởi Go Master
- Tự động fetch qua Unix socket mỗi lần startup
- Bao gồm: authorities, stakes, network info

## Installation & Setup

### Prerequisites
- Rust 1.70+
- Go 1.19+
- Unix domain sockets enabled
- Ports 9000-9004, 9100-9104 available

### Build
```bash
# Build Rust metanodes
cd metanode
cargo build --release

# Build Go components (nếu có)
cd ../go-components
go build
```

### Run System
```bash
# Start Go Master
./go-master

# Start Go Sub (optional)
./go-sub

# Start Rust nodes
./scripts/run_full_system.sh
```

## Monitoring & Debugging

### Logs
- **Node logs**: `logs/latest/node_*.log`
- **Consensus logs**: Authority startup, block creation, commits
- **Executor logs**: Transaction execution, state updates

### Key Metrics
- Block production rate
- Transaction throughput
- Epoch transition time
- Network latency

### Common Issues
1. **Port conflicts**: Đảm bảo ports không bị sử dụng
2. **Socket permissions**: Unix sockets cần quyền read/write
3. **Committee sync**: Nodes cần sync committee từ Go Master
4. **Epoch transitions**: Có thể mất vài giây để chuyển đổi

## Architecture Benefits

### Hybrid Go/Rust Design
- **Go**: Mature ecosystem, rich tooling, proven execution
- **Rust**: High performance, memory safety, modern consensus

### Dynamic Node Roles
- **Scalability**: Thêm/bớt nodes mà không restart toàn hệ thống
- **Cost efficiency**: Sync-only nodes tiết kiệm resources
- **Fault tolerance**: Nodes có thể recover roles tự động

### Sequential Consistency
- **Global execution index**: Đảm bảo transaction ordering
- **Block buffering**: Handle out-of-order commits
- **Fork prevention**: Duplicate index detection

## Development

### Adding New Features
1. **Consensus changes**: Modify `meta-consensus/` crate
2. **Execution logic**: Update Go Master components
3. **Client APIs**: Extend RPC/UDS interfaces

### Testing
```bash
# Unit tests
cargo test

# Integration tests
./scripts/run_integration_tests.sh

# Load testing
./scripts/load_test.sh
```

---

*Tài liệu này mô tả hệ thống Mysticeti blockchain hiện tại. Các thông tin có thể thay đổi theo development progress.*