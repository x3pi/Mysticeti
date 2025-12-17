# Câu hỏi thường gặp (FAQ)

## Khởi động và Recovery

### Q: Khi khởi động, hệ thống load lại dữ liệu như thế nào?

**A:** Khi node khởi động, hệ thống sẽ thực hiện recovery process để khôi phục trạng thái từ database:

1. **Load DAG State từ RocksDB**
   - Đọc committed state từ storage
   - Recover DAG structure (blocks, rounds, votes)
   - Recover threshold clock state

2. **Recover Block Commit Statuses**
   - Load commit statuses từ commit index hiện tại
   - Recover backwards đến GC round
   - Xác định blocks nào đã được commit

3. **Recover Commit Observer**
   - Load commit observer state trong range [1..=last_commit_index]
   - Recover unsent commits (nếu có)
   - Replay commits nếu cần

4. **Sync Missing Blocks**
   - Nếu có blocks missing, sync từ peers
   - Verify và add vào DAG
   - Catch up với current round

**Lưu ý:** Quá trình này có thể mất 40-60 giây nếu có nhiều commits (>1M commits).

### Q: Có cần nạp toàn bộ database không?

**A:** Không, hệ thống không nạp toàn bộ database vào memory. Thay vào đó:

- **DAG State**: Chỉ load committed state và recent rounds vào memory
- **Cached Rounds**: Chỉ cache 500 rounds gần nhất (có thể tùy chỉnh)
- **Old Data**: Dữ liệu cũ được GC và không load vào memory
- **On-demand Loading**: Blocks được load từ database khi cần

**Storage Structure:**
```
consensus_db/
├── CURRENT              # Current database state
├── MANIFEST-*           # Database manifest
├── *.sst                # SST files (sorted string tables)
└── LOG                  # Write-ahead log
```

**Memory Usage:**
- DAG state: ~100-500MB (tùy số rounds cached)
- Recent blocks: Loaded on-demand
- Old blocks: Không load vào memory

### Q: Tại sao khởi động mất nhiều thời gian (40-60 giây)?

**A:** Thời gian khởi động phụ thuộc vào số lượng commits cần recover:

**Các bước tốn thời gian:**
1. **Recovering committed state** (~1-5 giây)
   - Đọc từ RocksDB
   - Parse và reconstruct DAG state

2. **Recovering block commit statuses** (~1-5 giây)
   - Load commit statuses từ commit index
   - Recover backwards đến GC round

3. **Recovering commit observer** (~30-50 giây nếu có nhiều commits)
   - Recover trong range [1..=last_commit_index]
   - Với 1M+ commits, mất ~30-50 giây
   - Recover 250 unsent commits

4. **Replaying commits** (~5-10 giây)
   - Execute/replay các commits cũ
   - Log chi tiết về commits

**Tối ưu:**
- Tăng GC round để xóa dữ liệu cũ
- Sử dụng snapshot thay vì full recovery
- Sử dụng SSD để tăng tốc I/O

### Q: Recovery process hoạt động như thế nào?

**A:** Recovery process gồm các bước sau:

```
1. Load DAG State
   ├─► Read committed state from RocksDB
   ├─► Recover threshold clock
   └─► Reconstruct DAG structure

2. Recover Block Statuses
   ├─► Load commit statuses from last commit index
   ├─► Recover backwards to GC round
   └─► Mark blocks as committed/uncommitted

3. Recover Commit Observer
   ├─► Load commit observer state
   ├─► Recover unsent commits
   └─► Replay commits if needed

4. Sync Missing Blocks
   ├─► Detect missing blocks
   ├─► Request from peers
   └─► Verify and add to DAG

5. Catch Up
   ├─► Sync to current round
   └─► Ready to accept new transactions
```

**Log Example:**
```
INFO: Recovering committed state from C1106100
INFO: DagState was initialized with commit index 1106358
INFO: Recovering block commit statuses from commit index 1106358
INFO: Recovering commit observer in the range [1..=1106358]
INFO: Recovering 250 unsent commits
INFO: Consensus authority started, took 42.6s
```

### Q: Có thể skip recovery không?

**A:** Không thể skip recovery hoàn toàn, nhưng có thể tối ưu:

**Không thể skip:**
- DAG state recovery (cần để biết trạng thái hiện tại)
- Commit observer recovery (cần để biết commits đã xử lý)

**Có thể tối ưu:**
- **Tăng GC round**: Xóa dữ liệu cũ, giảm số commits cần recover
- **Snapshot**: Lưu snapshot định kỳ, recover từ snapshot thay vì từ đầu
- **Lazy loading**: Chỉ load dữ liệu cần thiết, load thêm khi cần

**Tăng GC round:**
```rust
// Trong code, có thể tăng GC round để xóa dữ liệu cũ
// Giảm số commits cần recover
```

### Q: Storage size tăng như thế nào?

**A:** Storage size tăng theo thời gian và số lượng commits:

**Storage Growth:**
- **Per commit**: ~1-10KB (tùy số blocks và transactions)
- **1K commits**: ~1-10MB
- **100K commits**: ~100MB-1GB
- **1M commits**: ~1-10GB

**GC (Garbage Collection):**
- RocksDB tự động GC dữ liệu cũ
- GC round được tính dựa trên commit index
- Dữ liệu cũ hơn GC round sẽ bị xóa

**Tối ưu Storage:**
- Tăng GC round để xóa dữ liệu cũ sớm hơn
- Compact database định kỳ
- Sử dụng compression

### Q: Có thể reset database không?

**A:** Có, có thể reset database bằng cách xóa storage directory:

**Cách 1: Xóa storage directory**
```bash
# Stop node
./stop_nodes.sh

# Xóa storage
rm -rf config/storage/node_0/consensus_db/*

# Start lại (sẽ tạo database mới)
./run_nodes.sh
```

**Cách 2: Xóa toàn bộ storage**
```bash
# Stop all nodes
./stop_nodes.sh

# Xóa toàn bộ storage
rm -rf config/storage/

# Start lại
./run_nodes.sh
```

**Lưu ý:**
- Xóa storage sẽ mất toàn bộ lịch sử
- Node sẽ sync lại từ peers
- Cần đảm bảo có ít nhất một node còn storage để sync

### Q: Tại sao có nhiều commits nhưng ít transactions?

**A:** Đây là hành vi bình thường:

**Lý do:**
1. **Empty blocks**: Authorities tạo blocks ngay cả khi không có transactions
2. **Consensus mechanism**: Cần commits để maintain consensus, không cần transactions
3. **Round progression**: Mỗi round có commits, không nhất thiết có transactions

**Ví dụ từ log:**
```
🔷 Executing commit #4500: 4 blocks, 0 transactions
```

**Giải thích:**
- Commit #4500 có 4 blocks (từ 4 authorities)
- Mỗi block có 0 transactions (empty blocks)
- Đây là bình thường khi không có transactions được submit

### Q: Có thể tăng tốc độ recovery không?

**A:** Có một số cách:

**1. Tăng GC Round**
- Giảm số commits cần recover
- Xóa dữ liệu cũ sớm hơn

**2. Sử dụng SSD**
- Tăng tốc I/O operations
- Giảm thời gian đọc từ database

**3. Tăng Batch Size**
- Tăng `commit_sync_batch_size`
- Tăng `max_blocks_per_sync`

**4. Parallel Fetching**
- Tăng `commit_sync_parallel_fetches`
- Fetch nhiều blocks song song

**5. Snapshot**
- Lưu snapshot định kỳ
- Recover từ snapshot thay vì từ đầu

### Q: Làm sao biết recovery đã hoàn thành?

**A:** Kiểm tra log để xem recovery đã hoàn thành:

**Log indicators:**
```
INFO: Recovering committed state from C1106100
INFO: Recovering finished, reached commit leader round 1106345
INFO: Recovering commit observer in the range [1..=1106358]
INFO: Recovering 250 unsent commits
INFO: Consensus authority started, took 42.6s  ← Recovery hoàn thành
INFO: Consensus node 0 initialized successfully
INFO: RPC server available at http://127.0.0.1:10100  ← Sẵn sàng nhận requests
```

**Check RPC server:**
```bash
# Kiểm tra RPC server đã sẵn sàng
curl http://127.0.0.1:10100/ready

# Nếu trả về {"ready":true} thì đã sẵn sàng
```

### Q: Recovery có ảnh hưởng đến performance không?

**A:** Recovery chỉ ảnh hưởng khi khởi động:

**During Recovery:**
- Node không accept transactions
- RPC server chưa sẵn sàng
- Network connections chưa active

**After Recovery:**
- Node hoạt động bình thường
- Performance không bị ảnh hưởng
- Recovery chỉ xảy ra một lần khi khởi động

**Lưu ý:**
- Recovery là one-time cost khi khởi động
- Sau khi recovery xong, performance như bình thường
- Không có ongoing overhead từ recovery

## Performance và Tuning

### Q: Làm sao để tăng throughput?

**A:** Có thể tăng throughput bằng cách:

**1. Giảm Delays**
```toml
speed_multiplier = 1.0  # Normal speed
# Hoặc giảm min_round_delay
```

**2. Tăng Batch Sizes**
```rust
parameters.max_blocks_per_sync = 64;
parameters.commit_sync_batch_size = 200;
```

**3. Tăng Parallel Fetching**
```rust
parameters.commit_sync_parallel_fetches = 16;
```

**4. Tối ưu Network**
- Sử dụng low-latency network
- Tăng bandwidth
- Giảm network congestion

### Q: Làm sao để giảm latency?

**A:** Có thể giảm latency bằng cách:

**1. Giảm Timeouts**
```rust
parameters.leader_timeout = Duration::from_millis(100);
parameters.min_round_delay = Duration::from_millis(30);
```

**2. Tối ưu Network**
- Sử dụng local network
- Giảm network latency
- Tối ưu routing

**3. Sử dụng SSD**
- Giảm storage I/O latency
- Tăng tốc database operations

## Network và Connectivity

### Q: Nodes không kết nối được với nhau?

**A:** Kiểm tra các điểm sau:

**1. Network Addresses**
```bash
# Kiểm tra network addresses trong config
cat config/node_0.toml | grep network_address
```

**2. Firewall**
```bash
# Kiểm tra firewall
sudo ufw status

# Allow ports
sudo ufw allow 9000:9003/tcp
```

**3. Connectivity**
```bash
# Test connectivity
telnet 127.0.0.1 9000
# hoặc
nc -zv 127.0.0.1 9000
```

**4. Committee Match**
- Đảm bảo tất cả nodes dùng cùng `committee.json`
- Network addresses trong committee phải match với config

### Q: Có thể chạy nodes trên nhiều máy không?

**A:** Có, nhưng cần:

**1. Update Network Addresses**
- Cập nhật `network_address` trong config với IP thực tế
- Cập nhật `committee.json` với IP addresses

**2. Firewall Rules**
- Allow consensus ports (9000-9003)
- Allow metrics ports (9100-9103)
- Allow RPC ports (10100-10103)

**3. Network Requirements**
- Low latency (<10ms recommended)
- Stable connection
- Sufficient bandwidth

## Storage và Persistence

### Q: Database size tăng nhanh, làm sao giảm?

**A:** Có thể giảm database size bằng cách:

**1. Tăng GC Round**
- Xóa dữ liệu cũ sớm hơn
- Giảm storage usage

**2. Compact Database**
```bash
# Stop node
./stop_nodes.sh

# Compact (RocksDB tự động compact, nhưng có thể force)
# Restart node
./run_nodes.sh
```

**3. Cleanup Old Data**
- Xóa logs cũ
- Archive old storage nếu cần

### Q: Có thể backup và restore database không?

**A:** Có, có thể backup và restore:

**Backup:**
```bash
# Stop nodes
./stop_nodes.sh

# Backup storage
tar -czf backup-$(date +%Y%m%d).tar.gz config/storage/

# Start nodes
./run_nodes.sh
```

**Restore:**
```bash
# Stop nodes
./stop_nodes.sh

# Restore storage
tar -xzf backup-20231217.tar.gz

# Start nodes
./run_nodes.sh
```

## Troubleshooting

### Q: Node không khởi động được?

**A:** Xem [TROUBLESHOOTING.md](./TROUBLESHOOTING.md) để biết chi tiết.

**Common issues:**
1. Port đã bị chiếm
2. Config file không hợp lệ
3. Key files không tồn tại
4. Committee mismatch

### Q: Transactions không được commit?

**A:** Kiểm tra:

**1. Consensus Ready**
```bash
grep "Consensus authority started" logs/node_0.log
```

**2. RPC Server Ready**
```bash
curl http://127.0.0.1:10100/ready
```

**3. Network Connectivity**
- Nodes có kết nối được với nhau không
- Firewall có block không

**4. Transaction Pool**
- Pool có đầy không
- Transactions có valid không

## Configuration

### Q: Làm sao thay đổi speed_multiplier?

**A:** Cập nhật trong config file:

```toml
# Chậm 20 lần
speed_multiplier = 0.05

# Chậm 40 lần
speed_multiplier = 0.025

# Tốc độ bình thường
speed_multiplier = 1.0
```

Sau đó restart nodes:
```bash
./stop_nodes.sh
./run_nodes.sh
```

### Q: Có thể override specific delays không?

**A:** Có, có thể override trong config:

```toml
speed_multiplier = 0.05
leader_timeout_ms = 5000  # Override thành 5 giây
min_round_delay_ms = 2000  # Override thành 2 giây
```

## Best Practices

### Q: Best practices cho production?

**A:** Xem [DEPLOYMENT.md](./DEPLOYMENT.md) để biết chi tiết.

**Tóm tắt:**
1. Sử dụng SSD cho storage
2. Monitor logs và metrics
3. Backup định kỳ
4. Sử dụng systemd services
5. Set up alerts
6. Tối ưu network
7. Tune consensus parameters

### Q: Có thể scale lên nhiều nodes hơn không?

**A:** Có, nhưng cần:

**1. Update Committee**
- Generate committee mới với số nodes mới
- Update tất cả nodes với committee mới

**2. Restart All Nodes**
- Stop tất cả nodes
- Update configs
- Start lại với committee mới

**3. Network Considerations**
- Đảm bảo network có thể handle nhiều nodes
- Tăng bandwidth nếu cần

## References

- [ARCHITECTURE.md](./ARCHITECTURE.md) - Kiến trúc hệ thống
- [CONSENSUS.md](./CONSENSUS.md) - Cơ chế consensus
- [DEPLOYMENT.md](./DEPLOYMENT.md) - Triển khai
- [TROUBLESHOOTING.md](./TROUBLESHOOTING.md) - Xử lý sự cố

