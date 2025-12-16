# Hướng dẫn Triển khai MetaNode Consensus Engine

Hướng dẫn chi tiết về cách triển khai và chạy nhiều consensus nodes.

## 📋 Mục lục

1. [Chuẩn bị](#chuẩn-bị)
2. [Tạo Configuration](#tạo-configuration)
3. [Triển khai Nodes](#triển-khai-nodes)
4. [Kiểm tra và Monitoring](#kiểm-tra-và-monitoring)
5. [Troubleshooting](#troubleshooting)
6. [Triển khai Production](#triển-khai-production)

## 🛠️ Chuẩn bị

### Yêu cầu hệ thống

- **Rust**: 1.70+ (khuyến nghị 1.75+)
- **OS**: Linux, macOS, hoặc Windows với WSL2
- **Network**: Ports 9000-9015 (cho 4 nodes) và 9100-9115 (metrics)
- **Disk**: Tối thiểu 1GB cho storage (tùy thuộc vào số lượng transactions)

### Build Project

```bash
# Di chuyển vào thư mục metanode
cd /home/abc/chain-new/Mysticeti/metanode

# Build release binary (khuyến nghị)
cargo build --release --bin metanode

# Hoặc build debug (nhanh hơn nhưng chậm hơn khi chạy)
cargo build --bin metanode
```

Binary sẽ được tạo tại: `target/release/metanode` hoặc `target/debug/metanode`

## 📝 Tạo Configuration

### Bước 1: Tạo Configuration cho Multiple Nodes

Tạo configuration cho 4 nodes (có thể thay đổi số lượng):

```bash
# Sử dụng binary đã build
./target/release/metanode generate --nodes 4 --output config

# Hoặc dùng cargo run
cargo run --release --bin metanode -- generate --nodes 4 --output config
```

### Bước 2: Kiểm tra Files đã tạo

Sau khi chạy lệnh trên, bạn sẽ có cấu trúc thư mục như sau:

```
config/
├── committee.json                    # Committee configuration (dùng chung cho tất cả nodes)
├── node_0.toml                       # Config cho node 0
├── node_1.toml                       # Config cho node 1
├── node_2.toml                       # Config cho node 2
├── node_3.toml                       # Config cho node 3
├── node_0_protocol_key.json          # Protocol keypair cho node 0
├── node_0_network_key.json           # Network keypair cho node 0
├── node_1_protocol_key.json          # Protocol keypair cho node 1
├── node_1_network_key.json           # Network keypair cho node 1
├── node_2_protocol_key.json          # Protocol keypair cho node 2
├── node_2_network_key.json           # Network keypair cho node 2
├── node_3_protocol_key.json          # Protocol keypair cho node 3
├── node_3_network_key.json           # Network keypair cho node 3
└── storage/
    ├── node_0/                       # Storage directory cho node 0
    ├── node_1/                       # Storage directory cho node 1
    ├── node_2/                       # Storage directory cho node 2
    └── node_3/                       # Storage directory cho node 3
```

### Bước 3: Xem Configuration Example

File `node_0.toml` sẽ có nội dung tương tự:

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

## 🚀 Triển khai Nodes

### Cách 1: Chạy Manual (Development/Testing)

Mở nhiều terminal và chạy từng node:

**Terminal 1 - Node 0:**
```bash
cd /home/abc/chain-new/Mysticeti/metanode
./target/release/metanode start --config config/node_0.toml
```

**Terminal 2 - Node 1:**
```bash
cd /home/abc/chain-new/Mysticeti/metanode
./target/release/metanode start --config config/node_1.toml
```

**Terminal 3 - Node 2:**
```bash
cd /home/abc/chain-new/Mysticeti/metanode
./target/release/metanode start --config config/node_2.toml
```

**Terminal 4 - Node 3:**
```bash
cd /home/abc/chain-new/Mysticeti/metanode
./target/release/metanode start --config config/node_3.toml
```

### Cách 2: Sử dụng Script tự động (Khuyến nghị)

Tạo script `run_nodes.sh`:

```bash
#!/bin/bash

# Script để chạy tất cả nodes trong tmux sessions

set -e

NODES=4
BINARY="./target/release/metanode"
CONFIG_DIR="config"

# Kiểm tra binary
if [ ! -f "$BINARY" ]; then
    echo "❌ Binary not found: $BINARY"
    echo "Please build first: cargo build --release --bin metanode"
    exit 1
fi

# Kill existing sessions
echo "🧹 Cleaning up existing sessions..."
for i in $(seq 0 $((NODES-1))); do
    tmux kill-session -t "metanode-$i" 2>/dev/null || true
done

# Start nodes
echo "🚀 Starting $NODES nodes..."
for i in $(seq 0 $((NODES-1))); do
    config_file="$CONFIG_DIR/node_$i.toml"
    if [ ! -f "$config_file" ]; then
        echo "❌ Config file not found: $config_file"
        exit 1
    fi
    
    echo "Starting node $i..."
    tmux new-session -d -s "metanode-$i" \
        "$BINARY start --config $config_file"
    
    sleep 1
done

echo "✅ All nodes started!"
echo ""
echo "To view logs:"
echo "  tmux attach -t metanode-0  # View node 0"
echo "  tmux attach -t metanode-1  # View node 1"
echo "  tmux attach -t metanode-2  # View node 2"
echo "  tmux attach -t metanode-3  # View node 3"
echo ""
echo "To stop all nodes:"
echo "  ./stop_nodes.sh"
```

Tạo script `stop_nodes.sh`:

```bash
#!/bin/bash

# Script để dừng tất cả nodes

NODES=4

echo "🛑 Stopping all nodes..."

for i in $(seq 0 $((NODES-1))); do
    tmux kill-session -t "metanode-$i" 2>/dev/null && echo "Stopped node $i" || echo "Node $i not running"
done

echo "✅ All nodes stopped"
```

Cấp quyền thực thi:

```bash
chmod +x run_nodes.sh stop_nodes.sh
```

Chạy:

```bash
./run_nodes.sh
```

### Cách 3: Sử dụng systemd (Production)

Tạo file systemd service cho mỗi node:

`/etc/systemd/system/metanode-0.service`:

```ini
[Unit]
Description=MetaNode Consensus Node 0
After=network.target

[Service]
Type=simple
User=metanode
WorkingDirectory=/opt/metanode
ExecStart=/opt/metanode/target/release/metanode start --config /opt/metanode/config/node_0.toml
Restart=always
RestartSec=10
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
```

Tạo tương tự cho các nodes khác (metanode-1, metanode-2, metanode-3).

Enable và start:

```bash
sudo systemctl enable metanode-0
sudo systemctl start metanode-0
sudo systemctl status metanode-0
```

## 📊 Kiểm tra và Monitoring

### 1. Kiểm tra Nodes đang chạy

```bash
# Kiểm tra processes
ps aux | grep metanode

# Kiểm tra ports
netstat -tuln | grep -E "(9000|9001|9002|9003|9100|9101|9102|9103)"

# Hoặc dùng ss
ss -tuln | grep -E "(9000|9001|9002|9003|9100|9101|9102|9103)"
```

### 2. Xem Logs

**Nếu dùng tmux:**
```bash
# Xem log node 0
tmux attach -t metanode-0

# Xem log node 1
tmux attach -t metanode-1

# Xem tất cả sessions
tmux list-sessions
```

**Nếu dùng systemd:**
```bash
# Xem logs node 0
sudo journalctl -u metanode-0 -f

# Xem logs của tất cả nodes
sudo journalctl -u metanode-* -f
```

### 3. Metrics (Prometheus)

Mỗi node expose metrics tại:

- Node 0: `http://localhost:9100/metrics`
- Node 1: `http://localhost:9101/metrics`
- Node 2: `http://localhost:9102/metrics`
- Node 3: `http://localhost:9103/metrics`

Kiểm tra metrics:

```bash
# Xem metrics node 0
curl http://localhost:9100/metrics

# Hoặc dùng browser
# http://localhost:9100/metrics
```

### 4. Kiểm tra Network Connectivity

```bash
# Test connection giữa các nodes
# Node 0 listening on port 9000
nc -zv 127.0.0.1 9000

# Node 1 listening on port 9001
nc -zv 127.0.0.1 9001
```

### 5. Kiểm tra Consensus hoạt động

Xem logs để kiểm tra:
- Nodes có kết nối với nhau không
- Blocks có được propose và commit không
- Có lỗi gì không

```bash
# Xem log với filter
tmux attach -t metanode-0 | grep -E "(connected|block|commit|error)"
```

## 🔧 Troubleshooting

### Node không start

**Lỗi: "Failed to bind address"**
```bash
# Kiểm tra port đã bị chiếm chưa
lsof -i :9000
# Hoặc
ss -tuln | grep 9000

# Kill process nếu cần
kill -9 <PID>
```

**Lỗi: "Committee path not specified"**
- Đảm bảo `committee_path` trong config file đúng
- Đảm bảo file `committee.json` tồn tại

**Lỗi: "Failed to load keypair"**
- Kiểm tra key files tồn tại
- Regenerate keys nếu cần: `./target/release/metanode generate --nodes 4 --output config`

### Nodes không kết nối được với nhau

**Kiểm tra network addresses:**
```bash
# Xem config của từng node
cat config/node_0.toml | grep network_address
cat config/node_1.toml | grep network_address
```

**Kiểm tra firewall:**
```bash
# Linux
sudo ufw status
sudo iptables -L

# Nếu cần, mở ports
sudo ufw allow 9000:9015/tcp
sudo ufw allow 9100:9115/tcp
```

**Kiểm tra committee configuration:**
- Tất cả nodes phải dùng cùng `committee.json`
- Network addresses trong committee phải match với config files

### Node crash hoặc restart liên tục

**Xem logs chi tiết:**
```bash
# Enable debug logging
RUST_LOG=debug ./target/release/metanode start --config config/node_0.toml
```

**Kiểm tra storage:**
```bash
# Xem storage directory
ls -lh config/storage/node_0/

# Kiểm tra disk space
df -h config/storage/
```

**Kiểm tra memory:**
```bash
# Xem memory usage
ps aux | grep metanode | awk '{print $4, $11}'
```

### Performance Issues

**Tăng log level để giảm overhead:**
```bash
# Thay đổi trong code hoặc dùng env var
RUST_LOG=warn ./target/release/metanode start --config config/node_0.toml
```

**Kiểm tra network latency:**
```bash
# Test latency giữa các nodes
ping 127.0.0.1
```

## 🏭 Triển khai Production

### 1. Security Best Practices

- **Không commit keys vào git**: Thêm vào `.gitignore`:
  ```
  config/*_key.json
  config/committee.json
  ```

- **Sử dụng strong keypairs**: Đảm bảo keys được generate an toàn

- **Network security**: Sử dụng TLS và firewall

- **Access control**: Giới hạn quyền truy cập vào nodes

### 2. High Availability

- **Multiple nodes**: Chạy ít nhất 4 nodes (3f+1 cho BFT)
- **Monitoring**: Setup monitoring và alerting
- **Backup**: Backup keys và storage định kỳ
- **Health checks**: Implement health check endpoints

### 3. Scaling

- **Horizontal scaling**: Thêm nodes mới vào committee
- **Vertical scaling**: Tăng resources cho nodes hiện tại
- **Load balancing**: Nếu có client connections

### 4. Monitoring Stack

Setup Prometheus + Grafana:

```yaml
# prometheus.yml
scrape_configs:
  - job_name: 'metanode'
    static_configs:
      - targets:
        - 'localhost:9100'
        - 'localhost:9101'
        - 'localhost:9102'
        - 'localhost:9103'
```

### 5. Logging

- **Centralized logging**: Sử dụng ELK stack hoặc Loki
- **Log rotation**: Setup log rotation để tránh đầy disk
- **Structured logging**: Sử dụng JSON format cho logs

## 📚 Tài liệu Tham khảo

- [Sui Documentation](https://docs.sui.io/)
- [Mysticeti Consensus](https://arxiv.org/pdf/2310.14821)
- [Rust Best Practices](https://rust-lang.github.io/api-guidelines/)

## 🆘 Hỗ trợ

Nếu gặp vấn đề:
1. Kiểm tra logs chi tiết
2. Xem troubleshooting section
3. Kiểm tra Sui documentation
4. Tạo issue trên repository

---

**Lưu ý**: Đây là hướng dẫn cho development/testing. Để triển khai production, cần thêm nhiều bước security và monitoring.



```bash

    # 1. Build project
    cd /home/abc/chain-new/Mysticeti/metanode
    cargo build --release --bin metanode

    # 2. Tạo configuration cho 4 nodes
    ./target/release/metanode generate --nodes 4 --output config

    # 3. Chạy tất cả nodes
    ./run_nodes.sh

    # 4. Xem logs (chọn một trong các cách)
    tmux attach -t metanode-0    # Xem node 0 trong tmux
    tail -f logs/node_0.log      # Xem log file

    # 5. Dừng tất cả nodes
    ./stop_nodes.sh

```

