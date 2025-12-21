# MetaNode Consensus Engine

Hệ thống consensus đa node dựa trên **Sui Mysticeti Consensus Protocol**, cho phép nhiều node giao tiếp và đạt được sự đồng thuận về thứ tự giao dịch trong một mạng blockchain phân tán.

## 📋 Mục lục

- [Tổng quan](#-tổng-quan)
- [Tính năng chính](#-tính-năng-chính)
- [Kiến trúc hệ thống](#-kiến-trúc-hệ-thống)
- [Cài đặt](#-cài-đặt)
- [Quick Start](#-quick-start)
- [Cấu hình](#-cấu-hình)
- [Sử dụng](#-sử-dụng)
- [Epoch Management](#-epoch-management)
- [RPC API](#-rpc-api)
- [Monitoring & Metrics](#-monitoring--metrics)
- [Troubleshooting](#-troubleshooting)
- [Tài liệu tham khảo](#-tài-liệu-tham-khảo)

---

## 🎯 Tổng quan

**MetaNode Consensus Engine** là một wrapper production-ready trên Sui Mysticeti consensus protocol, cung cấp:

- ✅ **Multi-node Consensus**: Chạy nhiều consensus nodes với cấu hình dễ dàng
- ✅ **DAG-based Consensus**: Sử dụng Directed Acyclic Graph để đạt consensus
- ✅ **Byzantine Fault Tolerance**: Chịu được f faulty nodes trong 3f+1 nodes
- ✅ **Epoch Management**: Hỗ trợ epoch transitions với fork-safety
- ✅ **Clock Synchronization**: NTP sync và clock drift monitoring
- ✅ **RPC Interface**: HTTP API để submit transactions
- ✅ **Metrics & Monitoring**: Prometheus metrics cho monitoring
- ✅ **Recovery**: Tự động recovery khi restart

## ✨ Tính năng chính

### Consensus Engine
- **Mysticeti Protocol**: DAG-based consensus với leader election
- **High Throughput**: Xử lý hàng trăm commits/second
- **Low Latency**: End-to-end transaction finalization ~300-600ms
- **Ordered Execution**: Đảm bảo commits được xử lý theo thứ tự

### Epoch Management
- **Time-based Epochs**: Tự động transition sau một khoảng thời gian
- **Fork-safe Transitions**: Commit index barrier đảm bảo tất cả nodes transition cùng lúc
- **Quorum-based Voting**: 2f+1 votes cần thiết cho epoch change
- **Per-epoch Storage**: Tách biệt consensus DB theo epoch

### Clock Synchronization
- **NTP Sync**: Đồng bộ với NTP servers
- **Drift Monitoring**: Theo dõi clock drift và cảnh báo
- **Health Gates**: Ngăn epoch proposals khi clock không healthy

### Network & Security
- **TLS Encryption**: gRPC với TLS cho network communication
- **Key Management**: Protocol và network keypairs riêng biệt
- **Committee-based**: Quorum threshold = 2f+1

---

## 🏗️ Kiến trúc hệ thống

```
┌─────────────────────────────────────────────────────────────────┐
│                    MetaNode Consensus Engine                    │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐        │
│  │   Node 0     │  │   Node 1     │  │   Node N     │        │
│  │              │  │              │  │              │        │
│  │ ┌──────────┐ │  │ ┌──────────┐ │  │ ┌──────────┐ │        │
│  │ │   RPC    │ │  │ │   RPC    │ │  │ │   RPC    │ │        │
│  │ │  Server  │ │  │ │  Server  │ │  │ │  Server  │ │        │
│  │ │ (HTTP)   │ │  │ │ (HTTP)   │ │  │ │ (HTTP)   │ │        │
│  │ └────┬─────┘ │  │ └────┬─────┘ │  │ └────┬─────┘ │        │
│  │      │       │  │      │       │  │      │       │        │
│  │ ┌────▼─────┐ │  │ ┌────▼─────┐ │  │ ┌────▼─────┐ │        │
│  │ │Transaction│ │  │ │Transaction│ │  │ │Transaction│ │        │
│  │ │  Client   │ │  │ │  Client   │ │  │ │  Client   │ │        │
│  │ └────┬─────┘ │  │ └────┬─────┘ │  │ └────┬─────┘ │        │
│  │      │       │  │      │       │  │      │       │        │
│  │ ┌────▼─────┐ │  │ ┌────▼─────┐ │  │ ┌────▼─────┐ │        │
│  │ │Consensus │ │  │ │Consensus │ │  │ │Consensus │ │        │
│  │ │Authority │ │  │ │Authority │ │  │ │Authority │ │        │
│  │ └────┬─────┘ │  │ └────┬─────┘ │  │ └────┬─────┘ │        │
│  │      │       │  │      │       │  │      │       │        │
│  │ ┌────▼─────┐ │  │ ┌────▼─────┐ │  │ ┌────▼─────┐ │        │
│  │ │  Commit  │ │  │ │  Commit  │ │  │ │  Commit  │ │        │
│  │ │Processor │ │  │ │Processor │ │  │ │Processor │ │        │
│  │ └──────────┘ │  │ └──────────┘ │  │ └──────────┘ │        │
│  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘        │
│         │                 │                 │                 │
│         └─────────────────┴─────────────────┘                 │
│                    Network Layer (Tonic/gRPC)                  │
│                                                                 │
│         ┌──────────────────────────────────────┐               │
│         │    Sui Mysticeti Consensus Core      │               │
│         │  - DAG-based consensus               │               │
│         │  - Transaction ordering              │               │
│         │  - Byzantine fault tolerance         │               │
│         │  - Leader election                   │               │
│         └──────────────────────────────────────┘               │
│                                                                 │
│  ┌──────────────────────────────────────────────────────┐      │
│  │              Storage Layer (RocksDB)                 │      │
│  │  - Per-epoch consensus DB                           │      │
│  │  - DAG state                                         │      │
│  │  - Committed blocks                                  │      │
│  │  - Commit history                                    │      │
│  └──────────────────────────────────────────────────────┘      │
└─────────────────────────────────────────────────────────────────┘
```

### Các thành phần chính

1. **ConsensusNode** (`src/node.rs`): Wrapper chính quản lý lifecycle của node
2. **RPC Server** (`src/rpc.rs`): HTTP server để nhận transactions
3. **Commit Processor** (`src/commit_processor.rs`): Xử lý commits theo thứ tự
4. **Epoch Change Manager** (`src/epoch_change.rs`): Quản lý epoch transitions
5. **Clock Sync Manager** (`src/clock_sync.rs`): Đồng bộ clock với NTP
6. **Configuration** (`src/config.rs`): Quản lý cấu hình và keypairs

Xem chi tiết trong [docs/ARCHITECTURE.md](./docs/ARCHITECTURE.md).

---

## 🚀 Cài đặt

### Yêu cầu hệ thống

- **Rust**: 1.70+ (khuyến nghị 1.75+)
- **OS**: Linux, macOS, hoặc Windows với WSL2
- **Network**: 
  - Ports 9000-9015 (consensus communication)
  - Ports 9100-9115 (Prometheus metrics)
  - Ports 10000-10015 (RPC server)
- **Storage**: Ít nhất 1GB cho mỗi node (tùy thuộc vào số lượng commits)

### Build từ source

```bash
# Di chuyển vào thư mục metanode
cd /home/abc/chain-new/Mysticeti/metanode

# Build release binary (khuyến nghị cho production)
cargo build --release --bin metanode

# Hoặc build debug (nhanh hơn cho development)
cargo build --bin metanode
```

**Binary sẽ được tạo tại:**
- Release: `target/release/metanode`
- Debug: `target/debug/metanode`

**Lưu ý:**
- ✅ Project đã độc lập, không cần Sui workspace
- ✅ Tất cả dependencies đã được copy vào `../crates/`
- ✅ Build sẽ tự động download các git dependencies (fastcrypto, anemo)

---

## ⚡ Quick Start

### 1. Build project

```bash
cargo build --release --bin metanode
```

### 2. Tạo configuration cho 4 nodes

```bash
./target/release/metanode generate --nodes 4 --output config
```

Lệnh này sẽ tạo:
- `config/committee_node_*.json` - Committee configuration cho từng node
- `config/node_*.toml` - Config files cho từng node
- `config/node_*_protocol_key.json` - Protocol keypairs
- `config/node_*_network_key.json` - Network keypairs
- `config/storage/node_*` - Storage directories

### 3. Chạy tất cả nodes

```bash
# Chạy tất cả nodes trong tmux sessions
./run_nodes.sh

# Dừng tất cả nodes
./stop_nodes.sh
```

### 4. Xem logs

```bash
# Xem log node 0 trong tmux
tmux attach -t metanode-0

# Hoặc xem log file (latest run)
tail -f logs/latest/node_0.log

# Xem epoch-related logs
tail -f logs/latest/node_0.epoch.log
```

### 5. Submit transaction

```bash
# Sử dụng client (nếu có)
cd ../client
./target/release/metanode-client submit \
    --endpoint http://127.0.0.1:10000 \
    --data "Hello, Blockchain!"

# Hoặc dùng curl
curl -X POST http://127.0.0.1:10000/submit \
    -H "Content-Type: application/json" \
    -d '{"data": "Hello, Blockchain!"}'
```

---

## ⚙️ Cấu hình

### Cấu trúc Configuration File

File `node_X.toml` có cấu trúc:

```toml
# Node identification
node_id = 0
network_address = "127.0.0.1:9000"

# Keypairs
protocol_key_path = "config/node_0_protocol_key.json"
network_key_path = "config/node_0_network_key.json"

# Committee
committee_path = "config/committee_node_0.json"
storage_path = "config/storage/node_0"

# Metrics
enable_metrics = true
metrics_port = 9100

# Performance tuning
speed_multiplier = 1.0  # 1.0 = normal, 0.05 = 20x slower
leader_timeout_ms = 200  # Optional: override speed_multiplier
min_round_delay_ms = 50  # Optional: override speed_multiplier

# Epoch management
time_based_epoch_change = true
epoch_duration_seconds = 600  # 10 minutes (None = disabled)
max_clock_drift_seconds = 5

# Clock synchronization
enable_ntp_sync = true
ntp_servers = ["pool.ntp.org", "time.google.com"]
ntp_sync_interval_seconds = 300  # 5 minutes
```

### Các tham số quan trọng

#### Performance
- `speed_multiplier`: Điều chỉnh tốc độ consensus (1.0 = bình thường, <1.0 = chậm hơn)
- `leader_timeout_ms`: Timeout cho leader election
- `min_round_delay_ms`: Delay tối thiểu giữa các rounds

#### Epoch Management
- `time_based_epoch_change`: Bật/tắt time-based epoch transitions
- `epoch_duration_seconds`: Thời gian mỗi epoch (None = vô thời hạn)
- `max_clock_drift_seconds`: Clock drift tối đa cho phép

#### Clock Sync
- `enable_ntp_sync`: Bật/tắt NTP synchronization
- `ntp_servers`: Danh sách NTP servers
- `ntp_sync_interval_seconds`: Khoảng thời gian sync với NTP

Xem chi tiết trong [docs/CONFIGURATION.md](./docs/CONFIGURATION.md).

---

## 📖 Sử dụng

### Chạy Nodes

#### Cách 1: Sử dụng script (Khuyến nghị)

```bash
# Chạy tất cả nodes
./run_nodes.sh
# hoặc
./scripts/node/run_nodes.sh

# Dừng tất cả nodes
./stop_nodes.sh
# hoặc
./scripts/node/stop_nodes.sh
```

**Lưu ý:** Các script thường dùng có symlinks ở root để backward compatibility. Xem [scripts/README.md](./scripts/README.md) để biết cấu trúc đầy đủ.

Script `run_nodes.sh` sẽ:
- Tạo per-run log directory (`logs/run-YYYYMMDDTHHMMSSZ/`)
- Reset epoch timestamp (nếu `RESET_EPOCH_TIMESTAMP_MS=1`)
- Start tất cả nodes trong tmux sessions
- Tạo epoch-only logs để dễ grep

#### Cách 2: Chạy manual

```bash
# Terminal 1 - Node 0
./target/release/metanode start --config config/node_0.toml

# Terminal 2 - Node 1
./target/release/metanode start --config config/node_1.toml

# Terminal 3 - Node 2
./target/release/metanode start --config config/node_2.toml

# Terminal 4 - Node 3
./target/release/metanode start --config config/node_3.toml
```

### Xem Logs

#### Xem log real-time

```bash
# Xem log của node 0 (latest run)
tail -f logs/latest/node_0.log

# Xem epoch-related logs
tail -f logs/latest/node_0.epoch.log

# Xem tất cả nodes
tail -f logs/latest/node_*.log
```

#### Tìm kiếm trong logs

```bash
# Tìm commits
grep "Executing commit" logs/latest/node_0.log

# Tìm transactions
grep "Transaction submitted" logs/latest/node_0.log

# Tìm epoch transitions
grep "EPOCH TRANSITION" logs/latest/node_0.epoch.log

# Đếm số commits
grep -c "Executing commit" logs/latest/node_0.log
```

#### Xem log trong tmux

```bash
# Attach vào tmux session của node 0
tmux attach -t metanode-0

# List tất cả sessions
tmux list-sessions

# Detach: Ctrl+B, sau đó D
```

Xem thêm trong [docs/TROUBLESHOOTING.md](./docs/TROUBLESHOOTING.md).

---

## 🔄 Epoch Management

### Tổng quan

Hệ thống hỗ trợ **epoch transitions** với các tính năng:

- **Time-based Epochs**: Tự động transition sau `epoch_duration_seconds`
- **Fork-safe**: Commit index barrier đảm bảo tất cả nodes transition cùng lúc
- **Quorum Voting**: Cần 2f+1 votes để approve epoch change
- **In-process Restart**: Authority restart trong cùng process (không cần restart process)

### Cấu hình Epoch

```toml
# Bật time-based epoch change
time_based_epoch_change = true
epoch_duration_seconds = 600  # 10 minutes

# Clock synchronization (quan trọng cho epoch transitions)
enable_ntp_sync = true
max_clock_drift_seconds = 5
```

### Quy trình Epoch Transition

1. **Proposal**: Node nào đó propose epoch change khi thời gian đã hết
2. **Voting**: Các nodes vote cho proposal (auto-vote nếu hợp lệ)
   - **CRITICAL**: Votes tiếp tục được broadcast ngay cả sau khi đạt quorum để đảm bảo tất cả nodes đều thấy quorum
3. **Quorum**: Khi đạt 2f+1 votes, proposal được approve
4. **Commit Index Barrier**: Đợi commit index vượt qua barrier (proposal_commit_index + 10)
5. **Fork-Safety Validations**: 
   - Verify quorum đạt
   - Verify đạt commit index barrier
   - Verify proposal hash consistency
   - Verify timestamp consistency
   - Sử dụng barrier làm `last_commit_index` (deterministic)
6. **Transition**: Tất cả nodes transition cùng lúc với cùng `last_commit_index` và `global_exec_index` (fork-safe)
7. **Restart**: Authority restart với epoch mới và consensus DB mới

### Monitoring Epoch

```bash
# Xem epoch status của tất cả nodes
./check_epoch_status.sh
# hoặc
./scripts/analysis/check_epoch_status.sh

# Verify fork-safety sau transition
./verify_epoch_transition.sh
# hoặc
./scripts/analysis/verify_epoch_transition.sh

# Analyze vote propagation
./scripts/analysis/analyze_vote_propagation.sh

# Phân tích epoch transition chi tiết
./scripts/analysis/analyze_epoch_transition.sh

# Phân tích tại sao hệ thống bị stuck
./scripts/analysis/analyze_stuck_system.sh

# Xem epoch status trong logs
tail -f logs/latest/node_0.log | grep -E "epoch|EPOCH"

# Tìm epoch proposals
grep "EPOCH CHANGE PROPOSAL" logs/latest/node_0.log

# Tìm epoch transitions và fork-safety values
grep "EPOCH TRANSITION\|Deterministic Values\|FORK-SAFETY" logs/latest/node_0.log
```

Xem chi tiết trong:
- [docs/EPOCH.md](./docs/EPOCH.md) - Epoch và cách triển khai
- [docs/EPOCH_PRODUCTION.md](./docs/EPOCH_PRODUCTION.md) - Best practices cho production
- [docs/FORK_SAFETY.md](./docs/FORK_SAFETY.md) - Fork-safety mechanisms và verification
- [docs/QUORUM_LOGIC.md](./docs/QUORUM_LOGIC.md) - Logic quorum cho epoch transition

---

## 🌐 RPC API

### Endpoints

#### `POST /submit`

Submit một transaction vào consensus.

**Request:**
```json
{
  "data": "Hello, Blockchain!"
}
```

**Response:**
```json
{
  "success": true,
  "transaction_hash": "a1b2c3d4...",
  "message": "Transaction submitted successfully"
}
```

**Example với curl:**
```bash
curl -X POST http://127.0.0.1:10000/submit \
    -H "Content-Type: application/json" \
    -d '{"data": "Hello, Blockchain!"}'
```

#### `GET /ready`

Health check endpoint.

**Response:**
```json
{
  "ready": true
}
```

### Ports

- **Node 0**: `http://127.0.0.1:10000`
- **Node 1**: `http://127.0.0.1:10001`
- **Node 2**: `http://127.0.0.1:10002`
- **Node 3**: `http://127.0.0.1:10003`

RPC port = metrics_port + 1000

Xem chi tiết trong [docs/RPC_API.md](./docs/RPC_API.md).

---

## 📊 Monitoring & Metrics

### Prometheus Metrics

Mỗi node expose metrics qua Prometheus:

- **Port**: 9100 + node_id
- **Endpoint**: `http://localhost:9100/metrics`

**Example:**
```bash
# Node 0 metrics
curl http://localhost:9100/metrics

# Node 1 metrics
curl http://localhost:9101/metrics
```

### Metrics Categories

- **Consensus Metrics**: Rounds, commits, latency
- **Network Metrics**: Messages, connections, bandwidth
- **Storage Metrics**: DB operations, cache hits/misses
- **Epoch Metrics**: Current epoch, epoch duration, transitions

### Logging

Logs được lưu trong `logs/run-YYYYMMDDTHHMMSSZ/`:

- `node_X.log`: Full logs
- `node_X.epoch.log`: Epoch-related logs only

Xem chi tiết trong [docs/DEPLOYMENT.md](./docs/DEPLOYMENT.md).

---

## 🐛 Troubleshooting

### Node không kết nối được

1. **Kiểm tra network addresses** trong config files
2. **Đảm bảo ports không bị chiếm**:
   ```bash
   netstat -tuln | grep -E '900[0-9]|910[0-9]|100[0-9][0-9]'
   ```
3. **Kiểm tra firewall settings**
4. **Xem logs** để tìm lỗi kết nối:
   ```bash
   tail -f logs/latest/node_0.log | grep -i error
   ```

### Lỗi khi load keys

1. **Đảm bảo key files tồn tại**:
   ```bash
   ls -la config/node_*_protocol_key.json
   ls -la config/node_*_network_key.json
   ```
2. **Kiểm tra format** của key files (BCS encoded)
3. **Regenerate keys** nếu cần:
   ```bash
   ./target/release/metanode generate --nodes 4 --output config
   ```

### Committee mismatch

1. **Tất cả nodes phải dùng cùng committee.json** (hoặc per-node committee files với cùng nội dung)
2. **Node IDs phải match** với committee
3. **Regenerate committee** nếu cần:
   ```bash
   ./target/release/metanode generate --nodes 4 --output config
   ```

### Epoch transition không xảy ra

1. **Kiểm tra time_based_epoch_change** trong config
2. **Kiểm tra epoch_duration_seconds** (None = disabled)
3. **Kiểm tra clock sync**:
   ```bash
   # Xem clock sync status trong logs
   grep -i "clock\|ntp" logs/latest/node_0.log
   ```
4. **Kiểm tra quorum**:
   ```bash
   grep "EPOCH CHANGE PROPOSAL\|quorum" logs/latest/node_0.epoch.log
   ```

### Recovery mất nhiều thời gian

Recovery có thể mất 40-50 giây nếu có hơn 1 triệu commits. Đây là bình thường.

Xem chi tiết trong [docs/TROUBLESHOOTING.md](./docs/TROUBLESHOOTING.md) và [docs/FAQ.md](./docs/FAQ.md).

---

## 📚 Tài liệu tham khảo

### Tài liệu MetaNode

Xem thêm tài liệu chi tiết trong thư mục [docs/](./docs/):

#### Tài liệu kỹ thuật
- [docs/README.md](./docs/README.md) - Mục lục và tổng quan tài liệu
- [docs/ARCHITECTURE.md](./docs/ARCHITECTURE.md) - Kiến trúc hệ thống và các thành phần
- [docs/CONSENSUS.md](./docs/CONSENSUS.md) - Cơ chế consensus và DAG
- [docs/TRANSACTIONS.md](./docs/TRANSACTIONS.md) - Xử lý transactions và commit processing
- [docs/RPC_API.md](./docs/RPC_API.md) - RPC API documentation
- [docs/COMMITTEE.md](./docs/COMMITTEE.md) - Giải thích về committee.json và cấu hình authorities
- [docs/RECOVERY.md](./docs/RECOVERY.md) - Recovery process và commit replay khi khởi động
- [docs/EPOCH.md](./docs/EPOCH.md) - Epoch và cách triển khai epoch transition
- [docs/EPOCH_PRODUCTION.md](./docs/EPOCH_PRODUCTION.md) - Best practices cho epoch transition trong production

#### Hướng dẫn sử dụng
- [docs/CONFIGURATION.md](./docs/CONFIGURATION.md) - Cấu hình hệ thống
- [docs/DEPLOYMENT.md](./docs/DEPLOYMENT.md) - Triển khai và vận hành
- [docs/DEPLOYMENT_CHECKLIST.md](./docs/DEPLOYMENT_CHECKLIST.md) - Checklist deploy
- [docs/TROUBLESHOOTING.md](./docs/TROUBLESHOOTING.md) - Xử lý sự cố và debugging
- [docs/FAQ.md](./docs/FAQ.md) - Câu hỏi thường gặp

#### Fork-Safety và Quorum
- [docs/FORK_SAFETY.md](./docs/FORK_SAFETY.md) - Fork-safety mechanisms và verification
- [docs/QUORUM_LOGIC.md](./docs/QUORUM_LOGIC.md) - Logic quorum cho epoch transition

#### Scripts và Tools
- [scripts/README.md](./scripts/README.md) - Hướng dẫn sử dụng các script tiện ích
- [docs/analysis/](./docs/analysis/) - Analysis reports và debugging tools

### Tài liệu Tham khảo

- [Sui Documentation](https://docs.sui.io/)
- [Mysticeti Consensus Paper](https://arxiv.org/pdf/2310.14821)
- [Sui GitHub Repository](https://github.com/MystenLabs/sui)

---

## 📝 License

Apache 2.0 - Giống như Sui

---

## 🤝 Đóng góp

Đây là một project demo/example. Để đóng góp vào Sui consensus, vui lòng tham gia [Sui repository chính](https://github.com/MystenLabs/sui).

---

## ⚠️ Lưu ý

Đây là một implementation đơn giản dựa trên Sui consensus. Để sử dụng trong production, vui lòng:

1. Tham khảo [docs/EPOCH_PRODUCTION.md](./docs/EPOCH_PRODUCTION.md) cho best practices
2. Đảm bảo clock synchronization được bật (`enable_ntp_sync = true`)
3. Monitor metrics và logs thường xuyên
4. Test kỹ epoch transitions trước khi deploy
5. Backup storage directories trước khi thay đổi cấu hình

---

## 🔧 Development Workflow

### Rebuild và restart

```bash
# 1. Rebuild
cd metanode
cargo build --release

# 2. Restart nodes
./stop_nodes.sh
./run_nodes.sh

# 3. Xem logs
tail -f logs/latest/node_0.log | grep 'Executing commit'

# 4. Submit transaction (trong terminal khác)
cd ../client
./target/release/metanode-client submit \
    --endpoint http://127.0.0.1:10000 \
    --data "Hello, Blockchain!"
```

---

**MetaNode Consensus Engine** - Production-ready consensus engine dựa trên Sui Mysticeti Protocol.
