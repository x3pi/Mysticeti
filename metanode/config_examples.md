# Ví Dụ Cấu Hình Executor

Tài liệu này mô tả cách cấu hình executor để control việc trao đổi với Go engine.

## Cấu Hình Cơ Bản

### Node 0 (Master - Full Access)
```toml
# Node 0 có thể đọc committee state và commit blocks
executor_read_enabled = true
executor_commit_enabled = true
executor_send_socket_path = "/tmp/executor0.sock"
executor_receive_socket_path = "/tmp/rust-go.sock_1"
```

### Node 1,2,3 (Read-Only)
```toml
# Các node khác chỉ đọc committee state, không commit blocks
executor_read_enabled = true
executor_commit_enabled = false
executor_send_socket_path = "/tmp/executor1.sock"  # Vẫn cần define nhưng không sử dụng
executor_receive_socket_path = "/tmp/rust-go.sock_1"  # All nodes read from same socket
```

### Node Test (No Executor)
```toml
# Node chỉ chạy consensus, không tương tác với Go
executor_read_enabled = false
executor_commit_enabled = false
```

## Các Tình Huống Sử Dụng

### 1. Testing Single Node
```toml
[node_0]
executor_read_enabled = true
executor_commit_enabled = true
# Socket paths như trên
```

### 2. Multi-Node Production
```toml
[node_0]
executor_read_enabled = true
executor_commit_enabled = true

[node_1]
executor_read_enabled = true
executor_commit_enabled = false
executor_receive_socket_path = "/tmp/rust-go.sock_1"  # Same as node 0

[node_2]
executor_read_enabled = true
executor_commit_enabled = false
executor_receive_socket_path = "/tmp/rust-go.sock_1"  # Same as node 0
```

### 3. Light Nodes (Chỉ Consensus)
```toml
[node_3]
executor_read_enabled = false  # Không cần đọc committee từ Go
executor_commit_enabled = false
# Node này chỉ tham gia consensus, không tương tác với execution engine
```

## Lưu Ý Quan Trọng

1. **Ít nhất 1 node** phải có `executor_commit_enabled = true` để commit transactions
2. **Tất cả nodes** nên có `executor_read_enabled = true` để đọc committee state (trừ light nodes)
3. **Socket paths** phải unique cho mỗi node
4. **Go engine** cần chạy trước khi start Rust nodes

## Troubleshooting

### Node không thể đọc committee
```
Cause: executor_read_enabled = false
Fix: Set executor_read_enabled = true
```

### Transactions không được commit
```
Cause: Không có node nào có executor_commit_enabled = true
Fix: Đảm bảo ít nhất 1 node có executor_commit_enabled = true
```

### Socket connection failed
```
Cause: Go engine chưa start hoặc socket paths sai
Fix: Kiểm tra Go engine đang chạy và socket paths đúng
```
