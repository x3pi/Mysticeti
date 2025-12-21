# Committee Configuration

## Tổng quan

File `committee.json` là file cấu hình chung cho tất cả nodes trong consensus network. Nó định nghĩa các authorities (nodes) tham gia vào consensus, các public keys của họ, và các thresholds cần thiết để đạt được quorum.

## Cấu trúc File

```json
{
  "committee": {
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
      }
    ]
  },
  "epoch_timestamp_ms": 1766032959787
}
```

**Lưu ý:** Hệ thống hiện tại dùng định dạng “extended” (wrapper) để lưu `epoch_timestamp_ms` chung cho tất cả nodes.

## Các Trường Chính

### 1. `epoch` (u64)

**Mô tả:** Số epoch hiện tại của consensus network.

**Giá trị:** 
- `0` = Epoch đầu tiên
- Tăng dần khi có epoch change

**Vai trò:**
- Đánh dấu version của committee
- Khi thay đổi committee (thêm/xóa nodes), epoch sẽ tăng
- Tất cả nodes phải dùng cùng epoch

**Ví dụ:**
```json
"epoch": 0
```

### 2. `total_stake` (u64)

**Mô tả:** Tổng stake của tất cả authorities trong committee.

**Tính toán:**
```
total_stake = sum(stake của tất cả authorities)
```

**Trong ví dụ:**
- 4 authorities, mỗi authority có stake = 1
- `total_stake = 1 + 1 + 1 + 1 = 4`

**Vai trò:**
- Dùng để tính quorum và validity thresholds
- Stake cao hơn = authority có ảnh hưởng lớn hơn trong consensus

### 3. `quorum_threshold` (u64)

**Mô tả:** Ngưỡng stake tối thiểu cần thiết để đạt quorum (đồng thuận).

**Công thức:**
```
quorum_threshold = 2f + 1
```
Với `f` = số nodes lỗi có thể chịu đựng được.

**Trong ví dụ:**
- 4 nodes → f = 1 (có thể chịu 1 node lỗi)
- `quorum_threshold = 2*1 + 1 = 3`

**Ý nghĩa:**
- Cần ít nhất 3 nodes đồng ý để commit một block
- Đảm bảo safety: không thể commit 2 blocks khác nhau cùng lúc

**Ví dụ:**
```
Node 0: stake=1, vote=YES
Node 1: stake=1, vote=YES  
Node 2: stake=1, vote=YES
Node 3: stake=1, vote=NO

Total votes: 3/4 = đạt quorum (3 >= quorum_threshold)
→ Block được commit
```

### 4. `validity_threshold` (u64)

**Mô tả:** Ngưỡng stake tối thiểu để một block được coi là valid.

**Công thức:**
```
validity_threshold = f + 1
```

**Trong ví dụ:**
- 4 nodes → f = 1
- `validity_threshold = 1 + 1 = 2`

**Ý nghĩa:**
- Cần ít nhất 2 nodes vote để block được coi là valid
- Block valid có thể được include trong DAG
- Nhưng cần quorum để commit

**Ví dụ:**
```
Node 0: stake=1, vote=YES
Node 1: stake=1, vote=YES
Node 2: stake=1, vote=NO
Node 3: stake=1, vote=NO

Total votes: 2/4 = đạt validity (2 >= validity_threshold)
→ Block valid, nhưng chưa đủ quorum để commit
```

### 5. `authorities` (Array)

**Mô tả:** Danh sách các authorities (nodes) trong committee.

**Số lượng:**
- Tối thiểu: 4 nodes (để chịu được 1 node lỗi)
- Khuyến nghị: 4-16 nodes
- Tối đa: Phụ thuộc vào network capacity

## Cấu trúc Authority

Mỗi authority có các trường sau:

### `stake` (u64)

**Mô tả:** Trọng số (stake) của authority trong consensus.

**Giá trị:**
- Thường là 1 cho mỗi authority (equal stake)
- Có thể khác nhau nếu muốn weighted voting

**Vai trò:**
- Authority có stake cao hơn có ảnh hưởng lớn hơn
- Dùng để tính quorum và validity

**Ví dụ:**
```json
"stake": 1  // Equal stake
```

### `address` (Multiaddr)

**Mô tả:** Địa chỉ network của authority.

**Format:** Multiaddr format
```
/ip4/<IP>/tcp/<PORT>
```

**Ví dụ:**
```json
"address": "/ip4/127.0.0.1/tcp/9000"
```

**Vai trò:**
- Nodes sử dụng để kết nối với nhau
- Phải match với `network_address` trong node config

**Lưu ý:**
- Tất cả nodes phải có thể reach được địa chỉ này
- Firewall phải allow ports này

### `hostname` (String)

**Mô tả:** Tên định danh của authority.

**Ví dụ:**
```json
"hostname": "node-0"
```

**Vai trò:**
- Dùng cho logging và identification
- Không ảnh hưởng đến consensus logic

### `authority_key` (String, Base64)

**Mô tả:** Public key của authority keypair.

**Format:** Base64 encoded Ed25519 public key

**Vai trò:**
- Dùng để verify authority identity
- Không dùng để ký blocks (dùng protocol_key)

**Ví dụ:**
```json
"authority_key": "qigf4InpYaAXB1JkBE6UuqjgSnT7rjggcR+co0iqhZKqsObC1+mSlI0ObOBxQ4q8F4jbtqXdwRF2lO62UAHvIsC2XLoAJ/FFtwijjwdUQydS8JiuRjzEuXu7F3tW4Akf"
```

### `protocol_key` (String, Base64)

**Mô tả:** Public key của protocol keypair - dùng để ký blocks.

**Format:** Base64 encoded Ed25519 public key

**Vai trò:**
- **Quan trọng nhất**: Dùng để ký blocks trong consensus
- Verify block signatures
- Mỗi node có protocol keypair riêng (private key lưu trong `node_X_protocol_key.json`)

**Ví dụ:**
```json
"protocol_key": "tZ9tp5ixpRpUWW6mSn361fX/02a15/RI3CLpg9tBnts="
```

**Lưu ý:**
- Private key tương ứng phải match với public key này
- Private key được lưu trong `config/node_X_protocol_key.json`

### `network_key` (String, Base64)

**Mô tả:** Public key của network keypair - dùng cho TLS và network identity.

**Format:** Base64 encoded Ed25519 public key

**Vai trò:**
- Dùng cho TLS connections
- Network identity và authentication
- Mỗi node có network keypair riêng (private key lưu trong `node_X_network_key.json`)

**Ví dụ:**
```json
"network_key": "Fefgf36/pkgrpc3Gn8ZBou3c7/CiFvyLeNxq3taS7BU="
```

## Byzantine Fault Tolerance

### Tính toán Thresholds

Với **n nodes** và **f faulty nodes** có thể chịu đựng:

```
n = 3f + 1
f = (n - 1) / 3
```

**Ví dụ với 4 nodes:**
- n = 4
- f = (4 - 1) / 3 = 1
- Có thể chịu được **1 node lỗi**

**Thresholds:**
- `quorum_threshold = 2f + 1 = 2*1 + 1 = 3`
- `validity_threshold = f + 1 = 1 + 1 = 2`

### Safety và Liveness

**Safety (An toàn):**
- Với quorum_threshold = 2f+1, không thể commit 2 blocks khác nhau
- Đảm bảo tất cả honest nodes commit cùng thứ tự

**Liveness (Sống động):**
- Với f faulty nodes, hệ thống vẫn tiếp tục commit
- Cần ít nhất 2f+1 nodes hoạt động

## Quan hệ với Node Config

### Committee vs Node Config

**Committee.json:**
- Chứa **public keys** của tất cả authorities
- Được **chia sẻ** giữa tất cả nodes
- Định nghĩa **network topology**

**Node_X.toml:**
- Chứa **private keys** của node cụ thể
- **Riêng tư** cho từng node
- Định nghĩa **local configuration**

### Matching Requirements

**Quan trọng:** Các trường phải match:

1. **Node ID** trong `node_X.toml` phải match với **index** trong `authorities[]`
   - `node_0.toml` → `authorities[0]`
   - `node_1.toml` → `authorities[1]`
   - ...

2. **Network Address** phải match:
   - `node_0.toml`: `network_address = "127.0.0.1:9000"`
   - `committee.json`: `authorities[0].address = "/ip4/127.0.0.1/tcp/9000"`

3. **Public Keys** phải match:
   - `authorities[0].protocol_key` phải match với public key từ `node_0_protocol_key.json`
   - `authorities[0].network_key` phải match với public key từ `node_0_network_key.json`

## Sử dụng trong Consensus

### Khi Node Khởi động

1. Load `committee.json`
2. Verify node's own index và keys match
3. Load public keys của tất cả peers
4. Sử dụng để verify blocks và votes từ peers

### Trong Consensus Process

1. **Block Verification:**
   - Verify block signature bằng `protocol_key` của author
   - Verify vote signatures bằng `protocol_key` của voters

2. **Quorum Calculation:**
   - Tính tổng stake của votes
   - So sánh với `quorum_threshold`
   - Commit nếu đạt quorum

3. **Network Connections:**
   - Kết nối đến peers dựa trên `address`
   - Sử dụng `network_key` cho TLS

## Thay đổi Committee

### Khi nào cần thay đổi?

1. **Thêm node mới:**
   - Thêm authority mới vào `authorities[]`
   - Tăng `total_stake`
   - Recalculate thresholds
   - Tăng `epoch`

2. **Xóa node:**
   - Xóa authority khỏi `authorities[]`
   - Giảm `total_stake`
   - Recalculate thresholds
   - Tăng `epoch`

3. **Thay đổi stake:**
   - Update `stake` của authority
   - Recalculate `total_stake` và thresholds
   - Tăng `epoch`

### Quy trình thay đổi

**Lưu ý:** Tất cả nodes phải restart với committee mới cùng lúc.

1. **Stop tất cả nodes:**
   ```bash
   ./scripts/node/stop_nodes.sh
   # hoặc (với symlink)
   ./stop_nodes.sh
   ```

2. **Update committee.json:**
   - Thêm/xóa authorities
   - Update thresholds
   - Tăng epoch

3. **Update node configs:**
   - Đảm bảo node IDs match với indices
   - Update network addresses nếu cần

4. **Restart tất cả nodes:**
   ```bash
   ./scripts/node/run_nodes.sh
   # hoặc (với symlink)
   ./run_nodes.sh
   ```

## Best Practices

### 1. Backup Committee

```bash
# Backup trước khi thay đổi
cp config/committee.json config/committee.json.backup
```

### 2. Verify Consistency

```bash
# Kiểm tra tất cả nodes dùng cùng committee
for i in 0 1 2 3; do
    echo "Node $i:"
    grep "committee_path" config/node_$i.toml
done
```

### 3. Validate Keys

- Đảm bảo public keys trong committee match với private keys
- Verify signatures khi test

### 4. Network Configuration

- Đảm bảo tất cả addresses có thể reach được
- Test connectivity trước khi start nodes

## Security Considerations

### 1. Public Keys

- Public keys trong committee.json là **public**
- Có thể chia sẻ công khai
- Không cần bảo mật

### 2. Private Keys

- **KHÔNG** bao giờ commit private keys vào git
- Private keys phải được bảo vệ
- Sử dụng secrets management trong production

### 3. Committee Integrity

- Verify committee.json không bị modify
- Sử dụng checksums nếu cần
- Monitor changes trong production

## Ví dụ Thực tế

### Committee với 4 nodes (hiện tại)

```json
{
  "epoch": 0,
  "total_stake": 4,
  "quorum_threshold": 3,    // Cần 3/4 nodes
  "validity_threshold": 2,  // Cần 2/4 nodes
  "authorities": [
    { "stake": 1, "address": "/ip4/127.0.0.1/tcp/9000", ... },  // Node 0
    { "stake": 1, "address": "/ip4/127.0.0.1/tcp/9001", ... },  // Node 1
    { "stake": 1, "address": "/ip4/127.0.0.1/tcp/9002", ... },  // Node 2
    { "stake": 1, "address": "/ip4/127.0.0.1/tcp/9003", ... }   // Node 3
  ]
}
```

**Fault Tolerance:**
- Có thể chịu được **1 node lỗi**
- Cần **3 nodes** để commit (quorum)
- Cần **2 nodes** để block valid

### Committee với 7 nodes (ví dụ)

```json
{
  "epoch": 0,
  "total_stake": 7,
  "quorum_threshold": 5,    // 2f+1 với f=2
  "validity_threshold": 3,  // f+1 với f=2
  "authorities": [
    // 7 authorities...
  ]
}
```

**Fault Tolerance:**
- Có thể chịu được **2 nodes lỗi**
- Cần **5 nodes** để commit (quorum)
- Cần **3 nodes** để block valid

## Troubleshooting

### Committee Mismatch

**Triệu chứng:**
```
Error: Committee validation failed
Error: Node ID 5 is out of range for committee size 4
```

**Giải pháp:**
- Đảm bảo tất cả nodes dùng cùng `committee.json`
- Kiểm tra node IDs match với indices
- Regenerate committee nếu cần

### Key Mismatch

**Triệu chứng:**
```
Error: Signature verification failed
```

**Giải pháp:**
- Verify public keys trong committee match với private keys
- Regenerate keys nếu cần

### Network Issues

**Triệu chứng:**
```
Error: Failed to connect to peer
```

**Giải pháp:**
- Kiểm tra addresses trong committee có đúng không
- Test connectivity đến các addresses
- Kiểm tra firewall

## References

- [CONFIGURATION.md](./CONFIGURATION.md) - Cấu hình chi tiết
- [CONSENSUS.md](./CONSENSUS.md) - Cơ chế consensus
- [ARCHITECTURE.md](./ARCHITECTURE.md) - Kiến trúc hệ thống

