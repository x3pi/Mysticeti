# MetaNode Consensus Engine

Consensus Engine đa node dựa trên Sui Mysticeti Consensus Protocol, cho phép nhiều node giao tiếp và đồng thuận giao dịch với nhau.

## 📋 Tổng quan

MetaNode Consensus Engine là một wrapper đơn giản trên Sui Mysticeti consensus, cho phép bạn:

- ✅ Chạy nhiều consensus nodes
- ✅ Giao tiếp giữa các nodes qua network
- ✅ Đồng thuận giao dịch sử dụng Mysticeti protocol
- ✅ Cấu hình dễ dàng cho multiple nodes

## 🏗️ Kiến trúc

```
┌─────────────────────────────────────────────────────────┐
│              MetaNode Consensus Engine                   │
├─────────────────────────────────────────────────────────┤
│                                                          │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐ │
│  │  Node 0  │  │  Node 1  │  │  Node 2  │  │  Node 3  │ │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘  └────┬─────┘ │
│       │             │             │             │        │
│       └─────────────┴─────────────┴─────────────┘        │
│                    Network Layer                          │
│       ┌──────────────────────────────────────┐          │
│       │    Sui Mysticeti Consensus Core       │          │
│       │  - DAG-based consensus                │          │
│       │  - Transaction ordering                │          │
│       │  - Byzantine fault tolerance          │          │
│       └──────────────────────────────────────┘          │
└─────────────────────────────────────────────────────────┘
```

## 🚀 Cài đặt

### Yêu cầu

- Rust 1.70+ 
- Sui repository đã được clone vào `../sui/`
- Sui dependencies đã được build (khuyến nghị build từ Sui workspace)

### Build

**Cách 1: Build Sui workspace trước, sau đó build metanode (Khuyến nghị)**

```bash
# Bước 1: Build Sui workspace để đảm bảo tất cả dependencies đã sẵn sàng
cd /home/abc/chain-new/Mysticeti/sui
cargo build --workspace

# Bước 2: Build metanode
cargo build --manifest-path ../metanode/Cargo.toml --bin metanode --release
```

**Cách 2: Build trực tiếp (có thể gặp lỗi với axum-server)**

```bash
cd metanode
cargo build --release
```

**Lưu ý:** 
- Nếu gặp lỗi với `axum-server`, đây là vấn đề tương thích version trong Sui dependency chain
- Xem file `BUILD_ISSUE.md` để biết chi tiết và các cách giải quyết
- Code của `metanode-consensus` là đúng, vấn đề nằm ở dependency chain của Sui

## 📖 Sử dụng

### Quick Start

**1. Build project:**
```bash
cargo build --release --bin metanode
```

**2. Tạo configuration cho 4 nodes:**
```bash
./target/release/metanode generate --nodes 4 --output config
```

**3. Chạy tất cả nodes (sử dụng script):**
```bash
./run_nodes.sh
```

**4. Xem logs:**
```bash
# Xem log node 0
tmux attach -t metanode-0

# Hoặc xem log file
tail -f logs/node_0.log
```

**5. Dừng tất cả nodes:**
```bash
./stop_nodes.sh
```

### Chi tiết

#### 1. Tạo Configuration cho Multiple Nodes

Tạo configuration files cho 4 nodes:

```bash
# Sử dụng binary đã build
./target/release/metanode generate --nodes 4 --output config

# Hoặc dùng cargo run
cargo run --release --bin metanode -- generate --nodes 4 --output config
```

Lệnh này sẽ tạo:
- `config/committee.json` - Committee configuration chung
- `config/node_0.toml` đến `config/node_3.toml` - Config cho từng node
- `config/node_*_protocol_key.json` - Protocol keypairs
- `config/node_*_network_key.json` - Network keypairs
- `config/storage/node_*` - Storage directories

#### 2. Chạy Nodes

**Cách 1: Sử dụng script tự động (Khuyến nghị)**

```bash
# Chạy tất cả nodes trong tmux sessions
./run_nodes.sh

# Dừng tất cả nodes
./stop_nodes.sh
```

**Cách 2: Chạy manual (Development)**

Mở nhiều terminal và chạy từng node:

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

**Xem hướng dẫn chi tiết:** Xem file [DEPLOYMENT.md](../docs/metanode/DEPLOYMENT.md) để biết thêm về:
- Triển khai production
- Monitoring và metrics
- Troubleshooting
- Best practices

### 3. Cấu trúc Configuration File

File `node_X.toml` có cấu trúc:

```toml
node_id = 0
network_address = "127.0.0.1:9000"
protocol_key_path = "config/node_0_protocol_key.json"
network_key_path = "config/node_0_network_key.json"
committee_path = "config/committee.json"
storage_path = "config/storage/node_0"
enable_metrics = true
metrics_port = 9100
```

## 🔧 Cấu trúc Code

```
metanode/
├── Cargo.toml          # Dependencies và build config
├── src/
│   ├── main.rs         # Entry point và CLI
│   ├── config.rs       # Configuration management
│   ├── node.rs         # Consensus node wrapper
│   └── transaction.rs  # Transaction handling
└── Readme.md           # Tài liệu này
```

### Các Module

#### `config.rs`
- Quản lý configuration cho nodes
- Tạo committee và keypairs
- Load/save configuration files

#### `node.rs`
- Wrapper cho Sui ConsensusAuthority
- Khởi tạo và quản lý node lifecycle
- Xử lý shutdown

#### `transaction.rs`
- Transaction submission interface
- Wrapper cho TransactionClient

## 🌐 Network Configuration

Mặc định, các nodes giao tiếp qua:
- **Port range**: 9000-9003 (cho 4 nodes)
- **Protocol**: Tonic (gRPC-based)
- **Address**: 127.0.0.1 (có thể thay đổi)

Để chạy trên nhiều máy, cập nhật `network_address` trong config files.

## 📊 Metrics

Mỗi node có thể expose metrics qua Prometheus:
- **Port**: 9100 + node_id
- **Endpoint**: `http://localhost:9100/metrics`

## 🔐 Security

- Mỗi node có protocol keypair riêng để ký blocks
- Network keypair cho TLS và network identity
- Committee configuration được chia sẻ giữa tất cả nodes

## 🐛 Troubleshooting

### Node không kết nối được

1. Kiểm tra network addresses trong config
2. Đảm bảo ports không bị chiếm
3. Kiểm tra firewall settings

### Lỗi khi load keys

1. Đảm bảo key files tồn tại
2. Kiểm tra format của key files (BCS encoded)
3. Regenerate keys nếu cần: `cargo run --bin metanode -- generate`

### Committee mismatch

1. Tất cả nodes phải dùng cùng `committee.json`
2. Node IDs phải match với committee
3. Regenerate committee nếu cần

## 📋 Xem Logs

### Xem log real-time

**Xem tất cả logs của node 0:**
```bash
tail -f logs/node_0.log
```

**Xem chỉ commits được execute:**
```bash
tail -f logs/node_0.log | grep 'Executing commit'
```

**Xem transactions được submit:**
```bash
tail -f logs/node_0.log | grep -E 'Transaction submitted|Transaction included'
```

**Xem cả commits và transactions:**
```bash
tail -f logs/node_0.log | grep -E 'Executing commit|Transaction submitted|Transaction included'
```

### Xem log của tất cả nodes

**Xem log của tất cả nodes cùng lúc:**
```bash
tail -f logs/node_*.log
```

**Xem log của node cụ thể:**
```bash
tail -f logs/node_1.log  # Node 1
tail -f logs/node_2.log  # Node 2
tail -f logs/node_3.log  # Node 3
```

### Tìm kiếm trong logs

**Tìm commit theo index:**
```bash
grep "commit #110470" logs/node_0.log
```

**Tìm transaction theo hash:**
```bash
grep "a1b2c3d4" logs/node_0.log
```

**Đếm số commits:**
```bash
grep -c "Executing commit" logs/node_0.log
```

**Xem commits có transactions:**
```bash
grep "Executing commit" logs/node_0.log | grep -v "transactions=0"
```

**Xem 100 dòng log cuối cùng:**
```bash
tail -n 100 logs/node_0.log
```

**Xem log từ thời điểm cụ thể:**
```bash
grep "2025-12-16T10:37" logs/node_0.log
```

### Xem log với màu sắc (nếu có cài ccze)

```bash
tail -f logs/node_0.log | ccze -A
```

### Xem log và lọc theo mức độ

**Chỉ xem INFO và WARN:**
```bash
tail -f logs/node_0.log | grep -E 'INFO|WARN'
```

**Chỉ xem ERROR:**
```bash
tail -f logs/node_0.log | grep ERROR
```

## 📚 Tài liệu

### Tài liệu MetaNode

Xem thêm tài liệu chi tiết trong thư mục [docs/](./docs/):

#### Tài liệu kỹ thuật
- [docs/README.md](./docs/README.md) - Mục lục và tổng quan tài liệu
- [docs/ARCHITECTURE.md](./docs/ARCHITECTURE.md) - Kiến trúc hệ thống và các thành phần
- [docs/CONSENSUS.md](./docs/CONSENSUS.md) - Cơ chế consensus và DAG
- [docs/TRANSACTIONS.md](./docs/TRANSACTIONS.md) - Xử lý transactions và commit processing
- [docs/RPC_API.md](./docs/RPC_API.md) - RPC API documentation
- [docs/COMMITTEE.md](./docs/COMMITTEE.md) - Giải thích về committee.json và cấu hình authorities

#### Hướng dẫn sử dụng
- [docs/CONFIGURATION.md](./docs/CONFIGURATION.md) - Cấu hình hệ thống
- [docs/DEPLOYMENT.md](./docs/DEPLOYMENT.md) - Triển khai và vận hành
- [docs/TROUBLESHOOTING.md](./docs/TROUBLESHOOTING.md) - Xử lý sự cố và debugging
- [docs/FAQ.md](./docs/FAQ.md) - Câu hỏi thường gặp về khởi động, recovery, và các vấn đề khác

### Tài liệu Tham khảo
- [Sui Documentation](https://docs.sui.io/)
- [Mysticeti Consensus Paper](https://arxiv.org/pdf/2310.14821)
- [Sui GitHub Repository](https://github.com/MystenLabs/sui)

## 📝 License

Apache 2.0 - Giống như Sui

## 🤝 Đóng góp

Đây là một project demo/example. Để đóng góp vào Sui consensus, vui lòng tham gia [Sui repository chính](https://github.com/MystenLabs/sui).

---

**Lưu ý**: Đây là một implementation đơn giản dựa trên Sui consensus. Để sử dụng trong production, vui lòng tham khảo Sui main repository và best practices.


```bash

   # 1. Rebuild
    cd metanode
    cargo build --release

    # 2. Restart nodes
    ./stop_nodes.sh
    ./run_nodes.sh

    # 3. Xem logs
    tail -f logs/node_0.log | grep 'Executing commit'

    # 4. Submit transaction (trong terminal khác)
    cd ../client
    ./target/release/metanode-client submit \
        --endpoint http://127.0.0.1:10100 \
        --data "Hello, Blockchain!"

```