# Scripts Khởi Động Nodes Riêng Biệt

## Tổng quan

Bộ scripts này cho phép khởi động từng node riêng biệt thay vì khởi động toàn bộ hệ thống cùng lúc. Điều này hữu ích cho:

- **Debugging**: Khởi động từng node một để kiểm tra lỗi
- **Development**: Chỉ chạy những nodes cần thiết
- **Maintenance**: Khởi động lại individual nodes khi cần
- **Scalability testing**: Thêm/bớt nodes linh hoạt

## ⚠️  **Quan trọng: KHÔNG xóa dữ liệu cũ**

**Khi khởi động nodes riêng biệt, scripts sẽ KHÔNG xóa:**
- ✅ Dữ liệu sample/blocks hiện có
- ✅ Genesis.json và validator keys
- ✅ Node configurations
- ✅ Chỉ xóa logs và sockets của node đó

**Điều này có nghĩa:**
- Nếu hệ thống đã có dữ liệu, nó sẽ được giữ nguyên
- Nodes sẽ tiếp tục từ state hiện tại
- Không tạo genesis mới hay keys mới

## Scripts có sẵn

### Khởi động tất cả (theo thứ tự)
- **`run_all_individual.sh`**: Khởi động tất cả nodes theo thứ tự đúng (tương tự `run_full_system.sh` nhưng dùng individual scripts)

### Go Nodes
- **`run_go_master.sh`**: Khởi động Go Master Node (config-master.json)
- **`run_go_sub.sh`**: Khởi động Go Sub Node (config-sub-write.json)

### Rust Consensus Nodes
- **`run_node_0.sh`**: Node 0 - Validator với executor enabled
- **`run_node_1.sh`**: Node 1 - Validator (không executor)
- **`run_node_2.sh`**: Node 2 - Validator (không executor)
- **`run_node_3.sh`**: Node 3 - Validator (không executor)
- **`run_node_4.sh`**: Node 4 - Sync-Only Node

## Thứ tự khởi động (QUAN TRỌNG)

### Luôn tuân thủ thứ tự sau để tránh lỗi:

```
1. Go Master Node      ← Luôn đầu tiên
2. Go Sub Node         ← Sau Go Master (cần kết nối)
3. Rust Node 0         ← Sau Go nodes (có executor)
4. Rust Node 1-3       ← Validators (không executor)
5. Rust Node 4         ← Sync-Only Node
```

## Cách sử dụng

### Khởi động tất cả cùng lúc (khuyến nghị)

```bash
cd /home/abc/chain-n/Mysticeti/metanode/scripts

# Khởi động tất cả theo thứ tự đúng
./run_all_individual.sh
```

### Khởi động trong tmux (khuyên dùng)

```bash
cd /home/abc/chain-n/Mysticeti/metanode/scripts

# Khởi động tự động trong tmux session
./start_mysticeti_in_tmux.sh

# Hoặc khởi động thủ công:
tmux new-session -d -s mysticeti-startup -c /home/abc/chain-n/Mysticeti/metanode/scripts
tmux send-keys -t mysticeti-startup './run_all_individual.sh' C-m
tmux attach -t mysticeti-startup
```

### Khởi động từng node riêng biệt

### Khởi động từng node riêng biệt

```bash
cd /home/abc/chain-n/Mysticeti/metanode/scripts

# 1. Khởi động Go Master (luôn đầu tiên)
./run_go_master.sh

# 2. Khởi động Go Sub (sau Go Master)
./run_go_sub.sh

# 3. Khởi động Rust Nodes (sau Go nodes)
./run_node_0.sh  # Validator với executor
./run_node_1.sh  # Validator
./run_node_2.sh  # Validator
./run_node_3.sh  # Validator
./run_node_4.sh  # Sync-Only Node
```

### Chuẩn bị (một lần duy nhất)

```bash
# 1. Build Rust binary
cd /home/abc/chain-n/Mysticeti/metanode
cargo build --release --bin metanode

# 2. Tạo configs và genesis
cd /home/abc/chain-n/Mysticeti/metanode
./target/release/metanode generate --nodes 5 --output config

# 3. Setup genesis.json (nếu chưa có)
# Scripts sẽ tự động tạo nếu cần
```

### Kiểm tra trạng thái

```bash
# Xem tất cả tmux sessions
tmux list-sessions

# Xem logs của từng node
tmux attach -t go-master
tmux attach -t go-sub
tmux attach -t metanode-0
tmux attach -t metanode-1
# ... etc

# Kiểm tra ports
lsof -i :9000  # Node 0
lsof -i :9001  # Node 1
lsof -i :9002  # Node 2
lsof -i :9003  # Node 3
lsof -i :9004  # Node 4
```

### Dừng nodes

```bash
# Dừng từng node
tmux kill-session -t go-master
tmux kill-session -t go-sub
tmux kill-session -t metanode-0
tmux kill-session -t metanode-1
# ... etc

# Hoặc dùng script tổng thể
./stop_full_system.sh
```

## Thông tin chi tiết từng node

### Go Master Node
- **Script**: `run_go_master.sh`
- **Tmux**: `go-master` ✅
- **Config**: `config-master.json`
- **Chức năng**: Init genesis, quản lý validators, thực thi transactions
- **Log**: `/tmp/go-master.log`

### Go Sub Node
- **Script**: `run_go_sub.sh`
- **Tmux**: `go-sub` ✅
- **Config**: `config-sub-write.json`
- **Chức năng**: Nhận blocks từ Master, xử lý write operations
- **Log**: `/tmp/go-sub.log`

### Rust Node 0 (Validator + Executor)
- **Script**: `run_node_0.sh`
- **Tmux**: `metanode-0`
- **Config**: `config/node_0.toml`
- **Port**: 9000
- **Chức năng**: Consensus + thực thi blocks
- **Log**: `logs/metanode-0.log`

### Rust Node 1-3 (Validators)
- **Scripts**: `run_node_1.sh`, `run_node_2.sh`, `run_node_3.sh`
- **Tmux**: `metanode-1`, `metanode-2`, `metanode-3`
- **Configs**: `config/node_1.toml`, `config/node_2.toml`, `config/node_3.toml`
- **Ports**: 9001, 9002, 9003
- **Chức năng**: Chỉ consensus (không thực thi)
- **Logs**: `logs/metanode-1.log`, etc.

### Rust Node 4 (Sync-Only)
- **Script**: `run_node_4.sh`
- **Tmux**: `metanode-4`
- **Config**: `config/node_4.toml`
- **Port**: 9004
- **Chức năng**: Chỉ đồng bộ data, không tham gia validator ban đầu
- **Log**: `logs/metanode-4.log`

## Troubleshooting

### Node không khởi động được

```bash
# Kiểm tra log chi tiết
tail -50 /tmp/go-master.log
tail -50 logs/metanode-0.log

# Kiểm tra tmux session
tmux list-sessions
tmux attach -t <session-name>
```

### Port conflicts

```bash
# Kiểm tra port nào đang bị chiếm
lsof -i :9000

# Kill process dùng port
kill -9 <PID>
```

### Tmux session issues

```bash
# Kiểm tra tmux sessions
tmux list-sessions

# Kill tmux session cụ thể
tmux kill-session -t metanode-0
tmux kill-session -t go-master

# Kill tất cả tmux sessions
tmux kill-server

# Attach vào session đang chạy
tmux attach -t mysticeti-startup

# Detach từ session (không kill)
# Trong tmux session: Ctrl+B, D
```

### Go Master không init genesis

```bash
# Kiểm tra genesis.json có tồn tại không
ls -la /home/abc/chain-n/mtn-simple-2025/cmd/simple_chain/genesis.json

# Restart Go Master nếu cần
tmux kill-session -t go-master
./run_go_master.sh
```

### Rust nodes không kết nối được

```bash
# Đảm bảo Go Master và Go Sub đã chạy trước
tmux has-session -t go-master
tmux has-session -t go-sub

# Kiểm tra sockets
ls -la /tmp/rust-go.sock_*
```

## So sánh với script tổng thể

| Tính năng | `run_full_system.sh` | Individual Scripts |
|-----------|---------------------|-------------------|
| **Khởi động** | Tất cả cùng lúc | Từng node một |
| **Debugging** | Khó | Dễ |
| **Flexibility** | Thấp | Cao |
| **Speed** | Nhanh | Chậm hơn |
| **Error handling** | Trung bình | Tốt |
| **Data cleanup** | Xóa toàn bộ dữ liệu cũ | **Giữ nguyên dữ liệu cũ** |
| **Genesis creation** | Tạo genesis mới | Sử dụng genesis hiện có |
| **Key generation** | Tạo keys mới | Sử dụng keys hiện có |

## Scripts liên quan

- `run_full_system.sh`: Khởi động toàn bộ hệ thống (xóa dữ liệu cũ)
- `stop_full_system.sh`: Dừng toàn bộ hệ thống
- `run_all_individual.sh`: Khởi động tất cả bằng individual scripts (giữ dữ liệu cũ)
- `start_mysticeti_in_tmux.sh`: Khởi động trong tmux session (khuyên dùng)
- **Tất cả nodes chạy trong tmux**: `go-master`, `go-sub`, `metanode-0` đến `metanode-4`
- Individual scripts: `run_go_master.sh`, `run_go_sub.sh`, `run_node_*.sh`

## Tips

1. **Luôn khởi động Go Master trước tiên** (nếu chưa chạy)
2. **Đợi Go Master init genesis (10-15 giây) trước khi khởi động Go Sub** (chỉ khi khởi động lần đầu)
3. **Đợi Go Sub kết nối với Go Master trước khi khởi động Rust nodes** (chỉ khi khởi động lần đầu)
4. **Sử dụng tmux để monitor logs real-time**
5. **Check ports và sockets nếu có lỗi kết nối**
6. **Scripts riêng lẻ giữ nguyên dữ liệu cũ** - không cần lo lắng mất dữ liệu

## Khi nào cần xóa dữ liệu cũ

Nếu bạn muốn **reset hoàn toàn hệ thống** (tương tự `run_full_system.sh`), hãy:

```bash
# 1. Dừng tất cả nodes
bash scripts/stop_full_system.sh

# 2. Xóa dữ liệu cũ thủ công
rm -rf /home/abc/chain-n/mtn-simple-2025/cmd/simple_chain/sample

# 3. Xóa configs và logs
rm -rf /home/abc/chain-n/Mysticeti/metanode/config/
rm -rf /home/abc/chain-n/Mysticeti/metanode/logs/

# 4. Tạo lại configs
cd /home/abc/chain-n/Mysticeti/metanode
cargo build --release --bin metanode
./target/release/metanode generate --nodes 5 --output config

# 5. Khởi động lại
bash scripts/run_all_individual.sh
```

---

*Scripts này được tạo từ `run_full_system.sh` để hỗ trợ development và debugging linh hoạt hơn. Chúng **giữ nguyên dữ liệu cũ** để tránh mất state khi khởi động lại nodes.* 🚀