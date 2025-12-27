# Committee Configuration

## Tổng quan

File `committee.json` định nghĩa các authorities (nodes) tham gia vào consensus. **Tất cả nodes đều lấy committee từ Go state qua Unix Domain Socket**, không phụ thuộc vào `executor_enabled`.

## Cơ chế Load Committee

### Tất cả Nodes Lấy từ Go State

**CRITICAL:** Tất cả nodes (0, 1, 2, 3) đều lấy committee từ Go state qua Unix Domain Socket khi khởi động:

1. **Startup (Block 0 - Genesis):**
   - Tất cả nodes tạo `ExecutorClient` để kết nối đến Go Master
   - Gọi `GetValidatorsAtBlockRequest(block_number=0)` qua Unix Domain Socket
   - Go Master trả về `ValidatorInfoList` với validators từ genesis state
   - Rust build committee từ validators này

2. **Epoch Transition:**
   - Tất cả nodes lấy committee từ Go state tại `last_global_exec_index` của epoch trước
   - Đảm bảo tất cả nodes có cùng committee từ cùng block number

3. **Unix Domain Socket Paths:**
   - Node 0: `/tmp/rust-go.sock_2` (Go Master)
   - Node 1-3: `/tmp/rust-go.sock_1` (Go Master)

### Không Còn Load từ File

**LƯU Ý:** Các file `committee_node_*.json` không còn được sử dụng để load committee:
- Script `run_full_system.sh` xóa tất cả `committee_node_*.json` files sau khi sync vào `genesis.json`
- Files này chỉ được tạo lại sau epoch transition để lưu `epoch_timestamp_ms` và `last_global_exec_index`
- Tất cả nodes đều lấy committee từ Go state, không đọc từ file

## Cấu trúc File (Sau Epoch Transition)

Sau epoch transition, `committee_node_X.json` được tạo lại với cấu trúc:

```json
{
  "epoch": 1,
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
  ],
  "epoch_timestamp_ms": 1766799608153,
  "last_global_exec_index": 899
}
```

**Lưu ý:** File này chỉ dùng để lưu metadata (epoch_timestamp_ms, last_global_exec_index), không dùng để load committee.

## Các Trường Chính

### 1. `epoch` (u64)

**Mô tả:** Số epoch hiện tại của consensus network.

**Giá trị:** 
- `0` = Epoch đầu tiên (genesis)
- Tăng dần khi có epoch change

**Vai trò:**
- Đánh dấu version của committee
- Khi thay đổi committee (thêm/xóa nodes), epoch sẽ tăng
- Tất cả nodes phải dùng cùng epoch

### 2. `total_stake` (u64)

**Mô tả:** Tổng stake của tất cả authorities trong committee.

**Tính toán:**
```
total_stake = sum(stake của tất cả authorities)
```

**Ví dụ:**
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

**Ví dụ:**
- 4 nodes → f = 1 (có thể chịu 1 node lỗi)
- `quorum_threshold = 2*1 + 1 = 3`

**Ý nghĩa:**
- Cần ít nhất 3 nodes đồng ý để commit một block
- Đảm bảo safety: không thể commit 2 blocks khác nhau cùng lúc

### 4. `validity_threshold` (u64)

**Mô tả:** Ngưỡng stake tối thiểu để một block được coi là valid.

**Công thức:**
```
validity_threshold = f + 1
```

**Ví dụ:**
- 4 nodes → f = 1
- `validity_threshold = 1 + 1 = 2`

**Ý nghĩa:**
- Cần ít nhất 2 nodes vote để block được coi là valid
- Block valid có thể được include trong DAG
- Nhưng cần quorum để commit

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

### `hostname` (String)

**Mô tả:** Tên định danh của authority.

**Ví dụ:**
```json
"hostname": "node-0"
```

**Vai trò:**
- Dùng cho logging và identification
- Được lấy từ Go state (`validator.name`)

### `authority_key` (String, Base64)

**Mô tả:** Public key của authority keypair (BLS).

**Format:** Base64 encoded BLS public key

**Vai trò:**
- Dùng để verify authority identity
- Được lấy từ Go state (`validator.authority_key`)

### `protocol_key` (String, Base64)

**Mô tả:** Public key của protocol keypair - dùng để ký blocks.

**Format:** Base64 encoded Ed25519 public key

**Vai trò:**
- **Quan trọng nhất**: Dùng để ký blocks trong consensus
- Verify block signatures
- Mỗi node có protocol keypair riêng (private key lưu trong `node_X_protocol_key.json`)
- Được lấy từ Go state (`validator.protocol_key`)

### `network_key` (String, Base64)

**Mô tả:** Public key của network keypair - dùng cho TLS và network identity.

**Format:** Base64 encoded Ed25519 public key

**Vai trò:**
- Dùng cho TLS connections
- Network identity và authentication
- Mỗi node có network keypair riêng (private key lưu trong `node_X_network_key.json`)
- Được lấy từ Go state (`validator.network_key`)

### `epoch_timestamp_ms` (u64)

**Mô tả:** Timestamp khi epoch bắt đầu (milliseconds).

**Vai trò:**
- Được lưu sau epoch transition
- Dùng để tính toán thời gian epoch
- Tất cả nodes phải có cùng timestamp

### `last_global_exec_index` (u64)

**Mô tả:** Global execution index cuối cùng của epoch trước.

**Vai trò:**
- Được lưu sau epoch transition
- Dùng để tính `global_exec_index` cho epoch mới
- Tất cả nodes phải có cùng giá trị

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

**Committee (từ Go state):**
- Chứa **public keys** của tất cả authorities
- Được **chia sẻ** giữa tất cả nodes
- Định nghĩa **network topology**
- **Tất cả nodes lấy từ Go state**, không từ file

**Node_X.toml:**
- Chứa **private keys** của node cụ thể
- **Riêng tư** cho từng node
- Định nghĩa **local configuration**
- `executor_enabled`: Chỉ ảnh hưởng đến việc gửi commits đến Go, không ảnh hưởng đến việc lấy committee

### Matching Requirements

**Quan trọng:** Các trường phải match:

1. **Node ID** trong `node_X.toml` phải match với **index** trong `authorities[]`
   - `node_0.toml` → `authorities[0]`
   - `node_1.toml` → `authorities[1]`
   - ...

2. **Network Address** phải match:
   - `node_0.toml`: `network_address = "127.0.0.1:9000"`
   - Go state: `authorities[0].address = "/ip4/127.0.0.1/tcp/9000"`

3. **Public Keys** phải match:
   - `authorities[0].protocol_key` phải match với public key từ `node_0_protocol_key.json`
   - `authorities[0].network_key` phải match với public key từ `node_0_network_key.json`

## Sử dụng trong Consensus

### Khi Node Khởi động

1. Tạo `ExecutorClient` để kết nối đến Go Master qua Unix Domain Socket
2. Gọi `GetValidatorsAtBlockRequest(block_number=0)` để lấy validators từ Go state
3. Build committee từ `ValidatorInfoList` trả về
4. Verify node's own index và keys match
5. Load public keys của tất cả peers từ committee

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
   - Thêm validator mới vào Go state (genesis.json)
   - Go Master sẽ trả về validator mới trong `ValidatorInfoList`
   - Rust nodes sẽ tự động nhận committee mới từ Go

2. **Xóa node:**
   - Xóa validator khỏi Go state
   - Go Master sẽ không trả về validator này
   - Rust nodes sẽ tự động nhận committee mới từ Go

3. **Thay đổi stake:**
   - Update stake trong Go state
   - Go Master sẽ trả về stake mới
   - Rust nodes sẽ tự động nhận committee mới từ Go

### Quy trình thay đổi

**Lưu ý:** Tất cả nodes phải restart với committee mới từ Go state.

1. **Update Go state:**
   - Update `genesis.json` với validators mới
   - Hoặc update stake state trong Go Master

2. **Restart Go Master:**
   - Go Master sẽ load validators mới từ genesis.json hoặc stake state

3. **Restart Rust nodes:**
   - Tất cả Rust nodes sẽ lấy committee mới từ Go state khi khởi động

## Best Practices

### 1. Đảm bảo Go Master Chạy Trước

**CRITICAL:** Go Master phải chạy và init genesis block trước khi Rust nodes khởi động:

```bash
# 1. Start Go Master
cd mtn-simple-2025/cmd/simple_chain
go run . -config=config-master.json

# 2. Đợi Go Master init genesis (kiểm tra log)
# 3. Start Rust nodes
cd Mysticeti/metanode
./scripts/node/run_nodes.sh
```

### 2. Verify Committee Consistency

```bash
# Kiểm tra tất cả nodes nhận cùng committee từ Go
for i in 0 1 2 3; do
    echo "Node $i:"
    grep "Successfully loaded committee from Go state" logs/latest/node_${i}.log
done
```

### 3. Validate Keys

- Đảm bảo public keys trong Go state match với private keys trong Rust
- Verify signatures khi test

### 4. Network Configuration

- Đảm bảo tất cả addresses có thể reach được
- Test connectivity trước khi start nodes
- Đảm bảo Unix Domain Socket paths đúng

## Security Considerations

### 1. Public Keys

- Public keys trong Go state là **public**
- Có thể chia sẻ công khai
- Không cần bảo mật

### 2. Private Keys

- **KHÔNG** bao giờ commit private keys vào git
- Private keys phải được bảo vệ
- Sử dụng secrets management trong production

### 3. Committee Integrity

- Verify Go state không bị modify
- Sử dụng checksums nếu cần
- Monitor changes trong production

## Troubleshooting

### Committee Mismatch

**Triệu chứng:**
```
Error: Committee validation failed
Error: Node ID 5 is out of range for committee size 4
```

**Giải pháp:**
- Đảm bảo Go Master đã init genesis với validators đúng
- Kiểm tra Go Master trả về đúng số lượng validators
- Kiểm tra node IDs match với indices

### Go Master Không Trả Về Validators

**Triệu chứng:**
```
Failed to load committee from Go state after 10 retries: No validators found in Go state at block 0
```

**Giải pháp:**
- Đảm bảo Go Master đã chạy và init genesis block
- Kiểm tra `genesis.json` có validators không
- Kiểm tra Go Master log để xem có lỗi init genesis không
- Đảm bảo `sync_committee_to_genesis.py` đã tạo `delegator_stakes` đúng

### Key Mismatch

**Triệu chứng:**
```
Error: Signature verification failed
```

**Giải pháp:**
- Verify public keys trong Go state match với private keys trong Rust
- Regenerate keys nếu cần

### Network Issues

**Triệu chứng:**
```
Error: Failed to connect to Go request socket
```

**Giải pháp:**
- Kiểm tra Go Master đã chạy chưa
- Kiểm tra Unix Domain Socket paths đúng
- Kiểm tra permissions của socket files

## References

- [CONFIGURATION.md](./CONFIGURATION.md) - Cấu hình chi tiết
- [CONSENSUS.md](./CONSENSUS.md) - Cơ chế consensus
- [ARCHITECTURE.md](./ARCHITECTURE.md) - Kiến trúc hệ thống
- [TRANSACTION_FLOW.md](./TRANSACTION_FLOW.md) - Luồng transaction
