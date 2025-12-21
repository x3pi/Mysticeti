# Scripts Directory

Thư mục chứa các script tiện ích cho MetaNode Consensus Engine.

## Cấu trúc

```
scripts/
├── node/          # Node management scripts
│   ├── run_nodes.sh          # Chạy tất cả nodes
│   ├── stop_nodes.sh         # Dừng tất cả nodes
│   ├── kill_node.sh          # Kill một node cụ thể
│   ├── start_node.sh         # Start một node cụ thể
│   └── restart_node.sh       # Restart một node cụ thể
│
└── analysis/      # Analysis và debugging scripts
    ├── check_epoch_status.sh         # Kiểm tra epoch status của tất cả nodes
    ├── verify_epoch_transition.sh     # Verify fork-safety sau epoch transition
    ├── analyze_epoch_transition.sh   # Phân tích chi tiết epoch transition
    ├── analyze_stuck_system.sh       # Phân tích tại sao hệ thống bị stuck
    ├── analyze_vote_propagation.sh   # Phân tích vote propagation
    └── analyze_bad_nodes.sh          # Phân tích bad nodes
```

## Sử dụng

### Node Management

```bash
# Chạy tất cả nodes
./scripts/node/run_nodes.sh
# hoặc (với symlink)
./run_nodes.sh

# Dừng tất cả nodes
./scripts/node/stop_nodes.sh
# hoặc
./stop_nodes.sh

# Kill một node cụ thể
./scripts/node/kill_node.sh 0
# hoặc
./kill_node.sh 0

# Start một node cụ thể
./scripts/node/start_node.sh 0
# hoặc
./start_node.sh 0

# Restart một node cụ thể
./scripts/node/restart_node.sh 0
# hoặc
./restart_node.sh 0
```

### Analysis & Debugging

```bash
# Kiểm tra epoch status
./scripts/analysis/check_epoch_status.sh
# hoặc
./check_epoch_status.sh

# Verify fork-safety sau epoch transition
./scripts/analysis/verify_epoch_transition.sh
# hoặc
./verify_epoch_transition.sh

# Phân tích epoch transition
./scripts/analysis/analyze_epoch_transition.sh

# Phân tích tại sao hệ thống bị stuck
./scripts/analysis/analyze_stuck_system.sh

# Phân tích vote propagation
./scripts/analysis/analyze_vote_propagation.sh

# Phân tích bad nodes
./scripts/analysis/analyze_bad_nodes.sh
```

## Symlinks

Để giữ backward compatibility, các script thường dùng có symlinks ở root:
- `run_nodes.sh` → `scripts/node/run_nodes.sh`
- `stop_nodes.sh` → `scripts/node/stop_nodes.sh`
- `kill_node.sh` → `scripts/node/kill_node.sh`
- `start_node.sh` → `scripts/node/start_node.sh`
- `restart_node.sh` → `scripts/node/restart_node.sh`
- `check_epoch_status.sh` → `scripts/analysis/check_epoch_status.sh`
- `verify_epoch_transition.sh` → `scripts/analysis/verify_epoch_transition.sh`

## Lưu ý

- Tất cả scripts đều chạy từ root directory của project
- Scripts sử dụng relative paths, đảm bảo chạy từ đúng directory
- Xem chi tiết từng script trong comments của script đó

