# Cấu hình hệ thống

## Tổng quan

MetaNode sử dụng file-based configuration với format TOML. Mỗi node có một file config riêng, và tất cả nodes chia sẻ một committee configuration chung.

## Cấu trúc Configuration

### Node Configuration (`node_X.toml`)

File cấu hình cho từng node:

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

#### Các trường

- `node_id`: `usize` - ID của node (0-based index)
- `network_address`: `string` - Địa chỉ network (host:port)
- `protocol_key_path`: `string?` - Đường dẫn đến protocol keypair file
- `network_key_path`: `string?` - Đường dẫn đến network keypair file
- `committee_path`: `string?` - Đường dẫn đến committee.json
- `storage_path`: `string` - Đường dẫn đến storage directory
- `enable_metrics`: `bool` - Bật/tắt metrics
- `metrics_port`: `u16` - Port cho metrics server

### Committee Configuration (`committee.json`)

File cấu hình chung cho tất cả nodes:

```json
{
  "epoch": 0,
  "total_stake": 4,
  "quorum_threshold": 3,
  "validity_threshold": 2,
  "authorities": [
    {
      "stake": 1,
      "address": "/ip4/127.0.0.1/tcp/9000",
      "hostname": "node-0",
      "authority_key": "...",
      "protocol_key": "...",
      "network_key": "..."
    },
    ...
  ]
}
```

#### Các trường

- `epoch`: `u64` - Epoch number
- `total_stake`: `u64` - Tổng stake của tất cả authorities
- `quorum_threshold`: `u64` - Ngưỡng quorum (2f+1)
- `validity_threshold`: `u64` - Ngưỡng validity (f+1)
- `authorities`: `array` - Danh sách authorities

## Generate Configuration

### Sử dụng CLI

```bash
# Generate config cho 4 nodes
./target/release/metanode generate --nodes 4 --output config

# Hoặc với cargo run
cargo run --release --bin metanode -- generate --nodes 4 --output config
```

### Output Files

Lệnh generate sẽ tạo:

```
config/
├── committee.json              # Committee configuration
├── epoch_timestamp.txt         # Epoch start timestamp
├── node_0.toml                 # Node 0 config
├── node_0_protocol_key.json    # Node 0 protocol keypair
├── node_0_network_key.json    # Node 0 network keypair
├── node_1.toml                 # Node 1 config
├── node_1_protocol_key.json    # Node 1 protocol keypair
├── node_1_network_key.json    # Node 1 network keypair
├── ...
└── storage/
    ├── node_0/                 # Node 0 storage
    ├── node_1/                 # Node 1 storage
    └── ...
```

## Keypairs

### Protocol Keypair

Dùng để ký blocks trong consensus:
- **Algorithm**: Ed25519
- **Format**: Base64 encoded (private + public)
- **Size**: 64 bytes (32 private + 32 public)

### Network Keypair

Dùng cho TLS và network identity:
- **Algorithm**: Ed25519
- **Format**: Base64 encoded (private + public)
- **Size**: 64 bytes (32 private + 32 public)

### Keypair Format

```json
"<base64_encoded_private_key><base64_encoded_public_key>"
```

Ví dụ:
```
dGVzdF9wcml2YXRlX2tleV8zMl9ieXRlc19sb25nX2hlcmU...
```

## Network Configuration

### Default Ports

Với 4 nodes:
- **Node 0**: 9000 (network), 9100 (metrics), 10100 (RPC)
- **Node 1**: 9001 (network), 9101 (metrics), 10101 (RPC)
- **Node 2**: 9002 (network), 9102 (metrics), 10102 (RPC)
- **Node 3**: 9003 (network), 9103 (metrics), 10103 (RPC)

### Multi-Machine Setup

Để chạy trên nhiều máy, cập nhật `network_address`:

**Machine 1 (Node 0):**
```toml
network_address = "192.168.1.10:9000"
```

**Machine 2 (Node 1):**
```toml
network_address = "192.168.1.11:9001"
```

**Machine 3 (Node 2):**
```toml
network_address = "192.168.1.12:9002"
```

**Machine 4 (Node 3):**
```toml
network_address = "192.168.1.13:9003"
```

Và cập nhật `committee.json` với địa chỉ đúng:
```json
{
  "authorities": [
    {
      "address": "/ip4/192.168.1.10/tcp/9000",
      ...
    },
    {
      "address": "/ip4/192.168.1.11/tcp/9001",
      ...
    },
    ...
  ]
}
```

## Storage Configuration

### Storage Path

Mỗi node có storage riêng:
```
config/storage/node_X/
└── consensus_db/          # RocksDB database
    ├── CURRENT
    ├── MANIFEST-*
    ├── *.sst              # SST files
    └── ...
```

### Storage Options

Có thể tùy chỉnh storage path trong config:
```toml
storage_path = "/data/metanode/node_0"
```

**Lưu ý:**
- Sử dụng SSD cho production
- Đảm bảo đủ disk space (ít nhất 10GB cho testing)
- Backup storage định kỳ

## Epoch Configuration

### Epoch Timestamp

Tất cả nodes phải dùng cùng epoch start timestamp:
- File: `config/epoch_timestamp.txt`
- Format: Unix timestamp (milliseconds)
- Generated tự động khi generate config

### Epoch Management

Hiện tại chỉ support epoch 0. Để thay đổi epoch:
1. Update `committee.json` với epoch mới
2. Update `epoch_timestamp.txt`
3. Restart tất cả nodes

## Metrics Configuration

### Enable/Disable Metrics

```toml
enable_metrics = true
metrics_port = 9100
```

### Metrics Endpoint

Khi enabled, metrics có sẵn tại:
```
http://localhost:9100/metrics
```

### Prometheus Integration

Có thể scrape metrics bằng Prometheus:
```yaml
scrape_configs:
  - job_name: 'metanode'
    static_configs:
      - targets: ['localhost:9100', 'localhost:9101', ...]
```

## Environment Variables

### Logging

Có thể điều chỉnh log level bằng environment variable:
```bash
RUST_LOG=metanode=info,consensus_core=debug ./target/release/metanode start
```

### Log Levels

- `error`: Chỉ errors
- `warn`: Warnings và errors
- `info`: Info, warnings, errors (default)
- `debug`: Debug, info, warnings, errors
- `trace`: Tất cả logs

## Advanced Configuration

### Consensus Parameters

Có thể tùy chỉnh consensus parameters trong code (`src/node.rs`):

```rust
let mut parameters = consensus_config::Parameters::default();
parameters.leader_timeout = Duration::from_millis(200);
parameters.min_round_delay = Duration::from_millis(50);
parameters.max_blocks_per_sync = 32;
// ... more parameters
```

Xem [CONSENSUS.md](./CONSENSUS.md) để biết thêm về các parameters.

## Configuration Validation

### Validation Rules

1. **Node ID**: Phải trong range [0, committee_size)
2. **Network Address**: Phải là valid host:port
3. **Keypairs**: Phải tồn tại và valid format
4. **Committee**: Phải match với node_id
5. **Storage Path**: Phải có quyền write

### Common Errors

1. **Node ID out of range**
   ```
   Error: Node ID 5 is out of range for committee size 4
   ```
   → Kiểm tra node_id trong config

2. **Key file not found**
   ```
   Error: Failed to read key file
   ```
   → Đảm bảo key files tồn tại

3. **Committee mismatch**
   ```
   Error: Committee validation failed
   ```
   → Tất cả nodes phải dùng cùng committee.json

## Backup và Restore

### Backup Configuration

```bash
# Backup toàn bộ config
tar -czf metanode-config-backup.tar.gz config/

# Backup chỉ keys
tar -czf metanode-keys-backup.tar.gz config/*_key.json
```

### Restore Configuration

```bash
# Restore toàn bộ config
tar -xzf metanode-config-backup.tar.gz

# Restore chỉ keys
tar -xzf metanode-keys-backup.tar.gz -C config/
```

**Lưu ý:** Không share keys giữa các environments!

## Security Best Practices

1. **Protect Key Files**
   - Set permissions: `chmod 600 config/*_key.json`
   - Không commit keys vào git
   - Sử dụng secrets management trong production

2. **Network Security**
   - Sử dụng firewall để restrict access
   - Enable TLS trong production
   - Sử dụng VPN cho multi-machine setup

3. **Storage Security**
   - Encrypt storage nếu chứa sensitive data
   - Backup định kỳ
   - Rotate keys định kỳ

