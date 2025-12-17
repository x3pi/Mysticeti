# Kiến trúc hệ thống MetaNode

## Tổng quan

MetaNode Consensus Engine là một wrapper đơn giản trên Sui Mysticeti consensus protocol, cho phép chạy nhiều consensus nodes và đạt được sự đồng thuận về thứ tự giao dịch.

## Kiến trúc tổng thể

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
│  │  - DAG state                                         │      │
│  │  - Committed blocks                                  │      │
│  │  - Commit history                                    │      │
│  └──────────────────────────────────────────────────────┘      │
└─────────────────────────────────────────────────────────────────┘
```

## Các thành phần chính

### 1. ConsensusNode (`src/node.rs`)

Wrapper chính cho Sui ConsensusAuthority, quản lý lifecycle của một consensus node.

**Chức năng:**
- Khởi tạo và cấu hình node
- Quản lý keypairs (protocol và network)
- Tạo và quản lý ConsensusAuthority
- Cung cấp TransactionClient cho RPC server
- Xử lý shutdown gracefully

**Luồng khởi động:**
1. Load configuration và committee
2. Load hoặc generate keypairs
3. Tạo storage directories
4. Khởi tạo ConsensusAuthority (mất ~40-50s do recovery)
5. Tạo TransactionClient
6. Spawn CommitProcessor để xử lý commits

### 2. RPC Server (`src/rpc.rs`)

HTTP RPC server đơn giản để nhận transactions từ clients.

**Chức năng:**
- Bind vào port và lắng nghe HTTP requests
- Nhận POST requests đến `/submit` endpoint
- Parse transaction data (text hoặc hex)
- Submit transactions vào TransactionClient
- Trả về response với transaction hash và block reference
- Retry logic với exponential backoff

**API:**
- `POST /submit` - Submit transaction
- `GET /ready` - Health check endpoint

### 3. Commit Processor (`src/commit_processor.rs`)

Xử lý commits theo thứ tự từ consensus layer.

**Chức năng:**
- Nhận CommittedSubDag từ consensus
- Đảm bảo commits được xử lý theo thứ tự (ordered execution)
- Xử lý out-of-order commits (lưu vào pending)
- Log chi tiết về commits và transactions
- Tính toán transaction hashes

**Cấu trúc Commit:**
- Một commit (CommittedSubDag) chứa nhiều blocks
- Mỗi block có thể chứa 0 hoặc nhiều transactions
- Blocks được tạo bởi các authorities khác nhau
- Leader block được chọn để commit

### 4. Configuration (`src/config.rs`)

Quản lý cấu hình cho nodes và committee.

**Chức năng:**
- Generate committee với multiple authorities
- Generate keypairs cho từng node
- Load/save configuration files (TOML)
- Load/save keypairs (Base64 encoded)
- Quản lý epoch timestamp

**Cấu trúc:**
- `committee.json` - Committee configuration chung
- `node_X.toml` - Config cho từng node
- `node_X_protocol_key.json` - Protocol keypair
- `node_X_network_key.json` - Network keypair
- `epoch_timestamp.txt` - Epoch start timestamp

### 5. Transaction Handling (`src/transaction.rs`)

Xử lý transactions và verification.

**Chức năng:**
- NoopTransactionVerifier - Không verify (cho testing)
- TransactionHandler - Wrapper cho TransactionClient (reserved)

## Luồng xử lý Transaction

```
Client
  │
  ├─► POST /submit (HTTP)
  │
  ▼
RPC Server
  │
  ├─► Parse request body
  ├─► Calculate transaction hash
  │
  ▼
TransactionClient
  │
  ├─► Submit to consensus
  │
  ▼
ConsensusAuthority
  │
  ├─► Add to transaction pool
  ├─► Include in block proposal
  ├─► Broadcast block
  ├─► Achieve consensus
  │
  ▼
Commit Observer
  │
  ├─► Finalize commit
  │
  ▼
Commit Processor
  │
  ├─► Process commits in order
  ├─► Extract transactions
  ├─► Log execution
  │
  ▼
Application Logic (TODO)
  │
  └─► Execute transactions
```

## Storage

### RocksDB Storage

Mỗi node lưu trữ:
- **DAG State**: Trạng thái của DAG (blocks, rounds, votes)
- **Committed Blocks**: Các blocks đã được commit
- **Commit History**: Lịch sử các commits
- **Leader Schedule**: Lịch trình leader election

**Location:** `config/storage/node_X/consensus_db/`

### Recovery Process

Khi node restart:
1. Load DAG state từ RocksDB
2. Recover committed state (có thể mất nhiều thời gian nếu có nhiều commits)
3. Recover block commit statuses
4. Recover commit observer state
5. Replay unsent commits (nếu có)

**Lưu ý:** Recovery có thể mất 40-50 giây nếu có hơn 1 triệu commits.

## Network Layer

### Protocol: Tonic (gRPC)

- **Port range**: 9000-9003 (cho 4 nodes, có thể mở rộng)
- **Transport**: TCP với TLS
- **Message types**: Blocks, votes, sync requests

### Network Components

1. **Network Manager**: Quản lý connections giữa các nodes
2. **Subscriber**: Subscribe để nhận blocks từ peers
3. **Synchronizer**: Đồng bộ blocks khi bị lag
4. **Round Prober**: Kiểm tra round progress của peers

## Security

### Keypairs

Mỗi node có 2 loại keypair:

1. **Protocol Keypair**: 
   - Dùng để ký blocks
   - Ed25519
   - Lưu trong `node_X_protocol_key.json`

2. **Network Keypair**:
   - Dùng cho TLS và network identity
   - Ed25519
   - Lưu trong `node_X_network_key.json`

### Committee

- Tất cả nodes phải dùng cùng `committee.json`
- Committee chứa public keys của tất cả authorities
- Quorum threshold = 2f+1 (với f là số nodes lỗi có thể chịu đựng)

## Metrics

Mỗi node expose metrics qua Prometheus:
- **Port**: 9100 + node_id
- **Endpoint**: `http://localhost:9100/metrics`

Metrics bao gồm:
- Consensus metrics (rounds, commits, latency)
- Network metrics (messages, connections)
- Storage metrics (DB operations)

## Performance Considerations

### Startup Time

- **Consensus Authority startup**: ~40-50 giây (do recovery)
- **RPC Server bind**: <100ms
- **Transaction client ready**: Sau khi consensus authority ready

### Throughput

- Phụ thuộc vào consensus parameters
- Default: ~100-200 commits/second
- Có thể tối ưu bằng cách điều chỉnh parameters

### Latency

- **Transaction submission**: <100ms (nếu consensus ready)
- **Commit finalization**: ~200-500ms (tùy network)
- **End-to-end**: ~300-600ms

## Mở rộng

### Thêm Node mới

1. Generate config cho node mới
2. Update committee.json với node mới
3. Restart tất cả nodes với committee mới
4. Node mới sẽ sync từ existing nodes

### Tối ưu Performance

1. Tăng `max_blocks_per_sync`
2. Tăng `commit_sync_parallel_fetches`
3. Tối ưu RocksDB parameters
4. Sử dụng SSD cho storage

