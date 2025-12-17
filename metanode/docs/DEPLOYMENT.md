# Triển khai và Vận hành

## Tổng quan

Hướng dẫn chi tiết về cách triển khai và vận hành MetaNode Consensus Engine trong môi trường development và production.

## Yêu cầu hệ thống

### Development

- **OS**: Linux, macOS, hoặc WSL2
- **CPU**: 2+ cores
- **RAM**: 4GB+
- **Disk**: 10GB+ free space
- **Network**: Local network với latency <10ms

### Production

- **OS**: Linux (Ubuntu 20.04+ recommended)
- **CPU**: 4+ cores
- **RAM**: 8GB+
- **Disk**: 50GB+ SSD (recommended)
- **Network**: Low latency network (<5ms between nodes)

## Cài đặt

### Build từ source

```bash
# 1. Clone repository
git clone <repository-url>
cd metanode

# 2. Build project
cargo build --release --bin metanode

# 3. Verify build
./target/release/metanode --help
```

### Dependencies

- Rust 1.70+
- Sui dependencies (đã được include trong workspace)

## Development Setup

### 1. Generate Configuration

```bash
# Generate config cho 4 nodes
./target/release/metanode generate --nodes 4 --output config
```

### 2. Start Nodes

**Cách 1: Sử dụng script (Khuyến nghị)**

```bash
# Start tất cả nodes trong tmux sessions
./run_nodes.sh

# Stop tất cả nodes
./stop_nodes.sh
```

**Cách 2: Manual**

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

### 3. Verify Nodes

```bash
# Check logs
tail -f logs/node_0.log

# Check RPC server
curl http://127.0.0.1:10100/ready

# Check metrics
curl http://127.0.0.1:9100/metrics
```

## Production Deployment

### Single Machine Deployment

**1. Tạo systemd service:**

```bash
sudo nano /etc/systemd/system/metanode@.service
```

```ini
[Unit]
Description=MetaNode Consensus Node %i
After=network.target

[Service]
Type=simple
User=metanode
WorkingDirectory=/opt/metanode
ExecStart=/opt/metanode/target/release/metanode start --config /opt/metanode/config/node_%i.toml
Restart=always
RestartSec=10
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
```

**2. Enable và start services:**

```bash
sudo systemctl daemon-reload
sudo systemctl enable metanode@0
sudo systemctl enable metanode@1
sudo systemctl enable metanode@2
sudo systemctl enable metanode@3

sudo systemctl start metanode@0
sudo systemctl start metanode@1
sudo systemctl start metanode@2
sudo systemctl start metanode@3
```

**3. Check status:**

```bash
sudo systemctl status metanode@0
sudo journalctl -u metanode@0 -f
```

### Multi-Machine Deployment

**1. Setup trên mỗi machine:**

```bash
# Machine 1 (Node 0)
scp -r metanode/ user@machine1:/opt/
ssh user@machine1
cd /opt/metanode
./target/release/metanode start --config config/node_0.toml
```

**2. Update network addresses:**

Cập nhật `network_address` trong config files và `committee.json` với IP addresses thực tế.

**3. Firewall rules:**

```bash
# Allow consensus ports
sudo ufw allow 9000:9003/tcp

# Allow metrics ports
sudo ufw allow 9100:9103/tcp

# Allow RPC ports
sudo ufw allow 10100:10103/tcp
```

## Monitoring

### Logs

**View logs:**

```bash
# Real-time logs
tail -f logs/node_0.log

# Search logs
grep "ERROR" logs/node_0.log

# Count commits
grep -c "Executing commit" logs/node_0.log
```

**Log rotation:**

```bash
# Install logrotate config
sudo nano /etc/logrotate.d/metanode
```

```
/opt/metanode/logs/*.log {
    daily
    rotate 7
    compress
    delaycompress
    missingok
    notifempty
    create 0640 metanode metanode
}
```

### Metrics

**Prometheus setup:**

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

**Grafana dashboard:**

Import metrics vào Grafana để visualize:
- Commit rate
- Transaction throughput
- Network latency
- Storage usage

### Health Checks

**RPC health check:**

```bash
#!/bin/bash
# health_check.sh

for port in 10100 10101 10102 10103; do
    if curl -f http://127.0.0.1:$port/ready > /dev/null 2>&1; then
        echo "Node on port $port is healthy"
    else
        echo "Node on port $port is DOWN"
        exit 1
    fi
done
```

## Backup và Recovery

### Backup

**1. Backup configuration:**

```bash
tar -czf metanode-config-$(date +%Y%m%d).tar.gz config/
```

**2. Backup storage:**

```bash
# Stop nodes first
./stop_nodes.sh

# Backup storage
tar -czf metanode-storage-$(date +%Y%m%d).tar.gz config/storage/

# Start nodes
./run_nodes.sh
```

**3. Automated backup:**

```bash
#!/bin/bash
# backup.sh

BACKUP_DIR="/backup/metanode"
DATE=$(date +%Y%m%d_%H%M%S)

mkdir -p $BACKUP_DIR

# Backup config
tar -czf $BACKUP_DIR/config-$DATE.tar.gz config/

# Backup storage (if nodes stopped)
if ! pgrep -f "metanode start" > /dev/null; then
    tar -czf $BACKUP_DIR/storage-$DATE.tar.gz config/storage/
fi
```

### Recovery

**1. Restore configuration:**

```bash
tar -xzf metanode-config-20231217.tar.gz
```

**2. Restore storage:**

```bash
# Stop nodes
./stop_nodes.sh

# Restore storage
tar -xzf metanode-storage-20231217.tar.gz

# Start nodes
./run_nodes.sh
```

## Maintenance

### Node Restart

**Graceful shutdown:**

```bash
# Send SIGTERM
kill -TERM <pid>

# Hoặc sử dụng systemd
sudo systemctl stop metanode@0
```

**Force restart:**

```bash
# Kill process
kill -9 <pid>

# Restart
./target/release/metanode start --config config/node_0.toml
```

### Update Nodes

**1. Build new version:**

```bash
git pull
cargo build --release --bin metanode
```

**2. Rolling update:**

```bash
# Stop node 0
sudo systemctl stop metanode@0

# Update binary
cp target/release/metanode /opt/metanode/target/release/

# Start node 0
sudo systemctl start metanode@0

# Wait for sync, then repeat for other nodes
```

### Storage Cleanup

**GC old data:**

RocksDB tự động GC, nhưng có thể manual cleanup:

```bash
# Stop node
sudo systemctl stop metanode@0

# Compact database (optional)
# RocksDB sẽ tự compact, nhưng có thể force

# Start node
sudo systemctl start metanode@0
```

## Scaling

### Thêm Node mới

**1. Generate config cho node mới:**

```bash
# Generate config cho node 4
./target/release/metanode generate --nodes 5 --output config_new
```

**2. Update committee:**

Merge node mới vào existing committee (cần update code hoặc manual edit).

**3. Restart tất cả nodes:**

Tất cả nodes cần restart với committee mới.

### Horizontal Scaling

Hiện tại không support dynamic node addition. Cần:
1. Stop tất cả nodes
2. Update committee với nodes mới
3. Restart tất cả nodes

## Security

### Key Management

**1. Protect keys:**

```bash
chmod 600 config/*_key.json
```

**2. Rotate keys:**

Generate new keys và update committee (cần restart tất cả nodes).

### Network Security

**1. Firewall:**

```bash
# Only allow from trusted IPs
sudo ufw allow from 192.168.1.0/24 to any port 9000:9003
```

**2. TLS:**

Enable TLS trong production (cần update code).

### Access Control

**1. RPC access:**

Restrict RPC ports to internal network only.

**2. Metrics:**

Restrict metrics ports hoặc use authentication.

## Performance Tuning

### Consensus Parameters

Tune parameters trong `src/node.rs`:

```rust
parameters.leader_timeout = Duration::from_millis(200);
parameters.min_round_delay = Duration::from_millis(50);
parameters.max_blocks_per_sync = 64;  // Increase for better throughput
```

### System Tuning

**1. Increase file descriptors:**

```bash
ulimit -n 65536
```

**2. TCP tuning:**

```bash
# /etc/sysctl.conf
net.core.somaxconn = 1024
net.ipv4.tcp_max_syn_backlog = 2048
```

**3. I/O scheduler:**

```bash
# Use deadline scheduler for SSD
echo deadline > /sys/block/sda/queue/scheduler
```

## Troubleshooting

Xem [TROUBLESHOOTING.md](./TROUBLESHOOTING.md) để biết chi tiết về:
- Common issues
- Debugging techniques
- Performance problems
- Network issues

## Best Practices

1. **Always backup** trước khi update
2. **Monitor logs** thường xuyên
3. **Use SSD** cho storage
4. **Keep nodes in sync** (restart cùng lúc nếu cần)
5. **Test updates** trong development trước
6. **Document changes** trong production
7. **Set up alerts** cho critical issues

