# Giao Tiếp Giữa Rust Consensus và Go Execution Engine

Tài liệu này mô tả các luồng giao tiếp giữa Rust Consensus Engine (Mysticeti Metanode) và Go Execution Engine (Simple Chain) thông qua Unix Domain Sockets.

## Tổng Quan Kiến Trúc

Hệ thống gồm 4 thành phần chính:

1. **Rust Consensus Engine** (Mysticeti): Xử lý consensus, tạo và commit blocks
2. **Go Master Node**: Node chính của execution engine, quản lý state và xử lý transactions
3. **Go Sub Nodes**: Các node phụ (node_1, node_2, node_3), chỉ đọc state từ Master
4. **Unix Domain Sockets**: Kênh giao tiếp giữa Rust và Go components

## Luồng Giao Tiếp

### 1. Luồng Gửi Blocks (Rust → Go)

**Mục đích**: Rust gửi committed sub-DAGs (blocks) đến Go để execute và cập nhật state.

**Socket Paths**:
- **Rust Send Socket**: `/tmp/executor{N}.sock` (N = node_id)
  - Node 0: `/tmp/executor0.sock`
  - Node 1: `/tmp/executor1.sock`
  - Node 2: `/tmp/executor2.sock`
  - Node 3: `/tmp/executor3.sock`

- **Go Receive Socket**: `/tmp/rust-go.sock_{N+1}`
  - Node 0 kết nối đến: `/tmp/rust-go.sock_1`
  - Node 1 kết nối đến: `/tmp/rust-go.sock_2`
  - Node 2 kết nối đến: `/tmp/rust-go.sock_3`
  - Node 3 kết nối đến: `/tmp/rust-go.sock_4`

**Quy tắc**: Chỉ **Node 0** mới được phép gửi blocks đến Go Master. Các node khác chỉ đọc state.

### 2. Luồng Đọc State (Go → Rust)

**Mục đích**: Rust đọc committee state, active validators và thông tin epoch từ Go.

**Socket Paths**:
- **Rust Receive Socket**: `/tmp/rust-go.sock_{N+1}` (đồng thời dùng để gửi và nhận)
- **Go Send Socket**: `/tmp/rust-go.sock_{N+1}`

## Cấu Hình Socket Paths

### Trong Rust (metanode/src/config.rs)

```rust
pub struct NodeConfig {
    // ... other fields ...
    pub executor_send_socket_path: String,    // Path gửi blocks đến Go
    pub executor_receive_socket_path: String, // Path nhận state từ Go
}
```

### Ví dụ cấu hình cho Node 0:

```toml
[node_0]
# ... other config ...
executor_send_socket_path = "/tmp/executor0.sock"
executor_receive_socket_path = "/tmp/rust-go.sock_1"
```

### Trong Go (pkg/config/config.go)

```go
type SimpleChainConfig struct {
    // ... other fields ...
    RustGoSendSocketPath    string `json:"rust_go_send_socket_path"`
    RustGoReceiveSocketPath string `json:"rust_go_receive_socket_path"`
}
```

### Ví dụ cấu hình Go Master:

```json
{
  "rust_go_send_socket_path": "/tmp/rust-go.sock_1",
  "rust_go_receive_socket_path": "/tmp/executor0.sock"
}
```

## Quy Tắc Giao Tiếp

### 1. Quyền Truy Cập State (Configurable)

- **Nodes with `executor_commit_enabled = true`**: Read + Write
  - Gửi committed blocks đến Go để execute transactions
  - Đọc committee state và validator info
  - Mặc định: chỉ node 0

- **Nodes with `executor_read_enabled = true`**: Read Only
  - Chỉ đọc committee state từ Go để đồng bộ
  - **KHÔNG** gửi blocks đến Go
  - Mặc định: tất cả nodes

### 2. Socket Management

- **Connection Pooling**: Mỗi ExecutorClient duy trì connection pool
- **Auto Reconnect**: Tự động reconnect khi socket bị đóng
- **Timeout Handling**: 10 giây timeout cho mỗi request

### 3. Backpressure Mechanism

- **Buffer Size**: Tối đa 500 blocks trong buffer
- **Blocking Behavior**: Rust dừng tạo blocks khi Go chậm xử lý
- **Sequential Processing**: Blocks được gửi theo thứ tự global_exec_index

## Flow Diagram

```
┌─────────────────┐     ┌──────────────────┐     ┌─────────────────┐
│   Rust Node 0   │     │   Go Master      │     │   Go Sub Nodes  │
│   (Consensus)   │     │   (Execution)    │     │   (Read Only)   │
└─────────────────┘     └──────────────────┘     └─────────────────┘
         │                           │                     │
         │ Send Committed Blocks     │                     │
         │ ─────────────────────────►│                     │
         │ executor0.sock            │                     │
         │                           │                     │
         │ Request State Info        │                     │
         │◄──────────────────────────│                     │
         │ rust-go.sock_1            │                     │
         │                           │                     │
         │                           │ Sync State          │
         │                           │────────────────────►│
         │                           │                     │
```

## Cấu Hình Mặc Định Theo Node

| Node ID | Rust Send Socket      | Rust Receive Socket   | Go Send Socket       | Go Receive Socket    | Read  | Commit |
|---------|----------------------|----------------------|---------------------|---------------------|-------|--------|
| 0       | /tmp/executor0.sock  | /tmp/rust-go.sock_1  | /tmp/rust-go.sock_1 | /tmp/executor0.sock | ✅    | ✅     |
| 1       | /tmp/executor1.sock  | /tmp/rust-go.sock_1  | /tmp/rust-go.sock_1 | /tmp/executor1.sock | ✅    | ❌     |
| 2       | /tmp/executor2.sock  | /tmp/rust-go.sock_1  | /tmp/rust-go.sock_1 | /tmp/executor2.sock | ✅    | ❌     |
| 3       | /tmp/executor3.sock  | /tmp/rust-go.sock_1  | /tmp/rust-go.sock_1 | /tmp/executor3.sock | ✅    | ❌     |

## Troubleshooting

### 1. Connection Errors

- **"Connection refused"**: Go chưa start hoặc socket path sai
- **"Permission denied"**: Socket file permissions sai
- **"No such file"**: Socket chưa được tạo

### 2. Buffer Overflow

- **Symptom**: Rust chậm lại, buffer đầy
- **Cause**: Go xử lý chậm hơn Rust tạo blocks
- **Solution**: Tăng resources Go hoặc giảm consensus rate

### 3. Socket Path Conflicts

- Đảm bảo mỗi node có unique socket paths
- Xóa socket files cũ trước khi restart: `rm /tmp/executor*.sock /tmp/rust-go.sock_*`

## Environment Variables

- `LOCALHOST_TESTING=1`: Bypass network health checks cho local development
- `SINGLE_NODE=1`: Force single-node mode, bỏ qua multi-node consensus requirements
