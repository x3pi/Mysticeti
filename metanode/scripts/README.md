# Metanode Scripts

Quản lý 5 node (0–4), mỗi node có: **Rust Metanode** + **Go Master** + **Go Sub**.

## Cấu trúc

```
scripts/
├── node/              # Quản lý node (start/stop/resume)
│   ├── run_all.sh     # Fresh start tất cả 5 node
│   ├── run_node.sh    # Fresh start 1 node
│   ├── stop_all.sh    # Dừng tất cả node
│   ├── stop_node.sh   # Dừng 1 node
│   ├── resume_all.sh  # Resume tất cả (giữ data)
│   └── resume_node.sh # Resume 1 node (giữ data)
├── logs/              # Xem log
│   ├── rust.sh        # Log Rust
│   ├── go-master.sh   # Log Go Master
│   ├── go-sub.sh      # Log Go Sub
│   ├── follow.sh      # Follow tất cả log 1 node
│   └── view.sh        # Tổng quan log tất cả node
└── analysis/          # Phân tích & debug
```

---

## 🚀 Quản lý Node

### Fresh Start (xóa data, giữ keys)

```bash
# Tất cả node
./node/run_all.sh

# 1 node cụ thể
./node/run_node.sh 0     # Node 0
./node/run_node.sh 4     # Node 4
```

`run_all.sh` sẽ tự động:
1. Stop tất cả process
2. Xóa data Go + Rust (giữ keys/config)
3. Sync committee → genesis
4. Start Go Masters → chờ socket → Go Subs → Rust Nodes

`run_node.sh` dùng khi chỉ muốn reset 1 node (các node khác vẫn chạy).

### Stop

```bash
# Tất cả
./node/stop_all.sh

# 1 node
./node/stop_node.sh 2
```

Quy trình stop: **SIGTERM** (flush LevelDB) → chờ 5s → kill tmux → xóa socket.

### Resume (giữ data)

```bash
# Tất cả
./node/resume_all.sh

# 1 node
./node/resume_node.sh 1
```

Resume khác run ở chỗ **không xóa data**, dùng khi restart sau khi dừng tạm.

---

## 📋 Xem Log

Tất cả log nằm trong `logs/node_N/`:

```
logs/node_0/
├── rust.log                        # Rust metanode
├── go-master-stdout.log            # Go Master stdout
├── go-sub-stdout.log               # Go Sub stdout
├── go-master/epoch_0/App.log       # Go Master epoch log
└── go-sub/epoch_0/App.log          # Go Sub epoch log
```

### Xem nhanh

```bash
./logs/rust.sh 0             # 50 dòng cuối Rust node 0
./logs/rust.sh 0 200         # 200 dòng cuối
./logs/rust.sh 0 -f          # Follow real-time

./logs/go-master.sh 1        # Go Master node 1
./logs/go-master.sh 1 -f     # Follow

./logs/go-sub.sh 2           # Go Sub node 2
```

### Follow tất cả log 1 node

```bash
./logs/follow.sh 0           # Follow Rust + Go Master + Go Sub node 0
```

### Tổng quan

```bash
./logs/view.sh               # Liệt kê tất cả log files + size
./logs/view.sh 0             # Tail tất cả log node 0
./logs/view.sh 0 rust        # Chỉ Rust
./logs/view.sh 0 go 200      # Chỉ Go, 200 dòng
```

---

## ⚙️ Cấu hình

### Rust (TOML)

```
config/node_N.toml           # N = 0..4
```

Các field quan trọng:
| Field | Mô tả |
|-------|--------|
| `network_address` | `127.0.0.1:900N` |
| `executor_commit_enabled` | `true` (bắt buộc) |
| `executor_send_socket_path` | `/tmp/executorN.sock` |
| `executor_receive_socket_path` | `/tmp/rust-go-nodeN-master.sock` |

### Go (JSON)

```
cmd/simple_chain/config-master-nodeN.json   # Go Master
cmd/simple_chain/config-sub-nodeN.json      # Go Sub
```

### Tmux Sessions

| Node | Go Master | Go Sub | Rust |
|------|-----------|--------|------|
| 0 | `go-master-0` | `go-sub-0` | `metanode-0` |
| 1 | `go-master-1` | `go-sub-1` | `metanode-1` |
| 2 | `go-master-2` | `go-sub-2` | `metanode-2` |
| 3 | `go-master-3` | `go-sub-3` | `metanode-3` |
| 4 | `go-master-4` | `go-sub-4` | `metanode-4` |

```bash
tmux ls                       # Xem tất cả session
tmux attach -t metanode-0     # Attach vào Rust node 0
```

---

## 🔍 Lệnh hữu ích

```bash
# Kiểm tra node nào đang chạy
tmux ls

# Xem consensus đang ở block nào (từ Rust log)
./logs/rust.sh 0 5 | grep "commit_index"

# Xem epoch hiện tại
./logs/rust.sh 0 5 | grep "epoch"

# Kiểm tra Go đang xử lý block nào
./logs/go-master.sh 0 5 | grep "block"
```
