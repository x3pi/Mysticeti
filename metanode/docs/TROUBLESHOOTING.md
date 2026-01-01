# Xử lý sự cố

## Tổng quan

Hướng dẫn xử lý các vấn đề thường gặp khi vận hành MetaNode Consensus Engine.

## Vấn đề khởi động

### Node không khởi động được

**Triệu chứng:**
```
Error: Failed to bind RPC server to 127.0.0.1:10100: Address already in use
```

**Nguyên nhân:**
- Port đã bị chiếm bởi process khác
- Node cũ chưa được stop

**Giải pháp:**
```bash
# Tìm process đang dùng port
lsof -i :10100
# hoặc
netstat -tlnp | grep 10100

# Kill process
kill -9 <pid>

# Hoặc stop nodes cũ
./scripts/node/stop_nodes.sh
# hoặc (với symlink)
./stop_nodes.sh
```

### Node khởi động chậm

**Triệu chứng:**
- Node mất 40-60 giây để khởi động
- Log hiển thị "Recovering committed state"

**Nguyên nhân:**
- Node đang recover từ storage
- Có nhiều commits cần recover (>1M commits)

**Giải pháp:**
- Đây là hành vi bình thường
- Có thể giảm recovery time bằng cách:
  - Tăng GC round để xóa dữ liệu cũ

### Committee mismatch

**Triệu chứng:**
```
Error: Committee validation failed
Error: Node ID 5 is out of range for committee size 4
```

**Nguyên nhân:**
- Node config không match với committee
- Committee.json không đồng bộ giữa các nodes

**Giải pháp:**
```bash
# Kiểm tra committee.json giống nhau trên tất cả nodes
diff config/committee.json config/committee.json.backup

# Regenerate config nếu cần
./target/release/metanode generate --nodes 4 --output config
```

## Vấn đề network

### Nodes không kết nối được

**Triệu chứng:**
- Nodes không sync blocks
- Log hiển thị connection errors

**Nguyên nhân:**
- Network addresses sai
- Firewall blocking
- Nodes chưa khởi động

**Giải pháp:**
```bash
# Kiểm tra network addresses
cat config/node_0.toml | grep network_address

# Kiểm tra firewall
sudo ufw status

# Test connectivity
telnet 127.0.0.1 9000
# hoặc
nc -zv 127.0.0.1 9000
```

### High network latency

**Triệu chứng:**
- Commits chậm
- Blocks sync chậm

**Nguyên nhân:**
- Network latency cao
- Bandwidth thấp
- Network congestion

**Giải pháp:**
- Sử dụng local network (<10ms latency)
- Tăng bandwidth
- Tối ưu network configuration

## Vấn đề RPC

### RPC server không respond

**Triệu chứng:**
```
Error: Failed to send request: error sending request for url
```

**Nguyên nhân:**
- RPC server chưa bind xong
- Consensus authority chưa sẵn sàng
- Port bị chiếm

**Giải pháp:**
```bash
# Kiểm tra RPC server
curl http://127.0.0.1:10100/ready

# Kiểm tra logs
tail -f logs/node_0.log | grep "RPC server"

# Đợi consensus authority ready (có thể mất 40-60s)
# RPC server sẽ tự retry khi submit transaction
```

### Transaction submission fails

**Triệu chứng:**
```
Error: Transaction submission failed after retries
```

**Nguyên nhân:**
- Consensus authority chưa sẵn sàng
- Transaction pool đầy
- Network issues

**Giải pháp:**
```bash
# Kiểm tra consensus ready
grep "Consensus authority started" logs/node_0.log

# Đợi consensus ready, sau đó retry
# RPC server có retry logic tự động
```

## Vấn đề storage

### Disk space đầy

**Triệu chứng:**
```
Error: No space left on device
```

**Nguyên nhân:**
- Storage directory đầy
- RocksDB files quá lớn

**Giải pháp:**
```bash
# Kiểm tra disk space
df -h

# Kiểm tra storage size
du -sh config/storage/node_0/

# Cleanup old data (cần stop node)
# RocksDB sẽ tự GC, nhưng có thể manual cleanup
```

### Storage corruption

**Triệu chứng:**
```
Error: Database corruption detected
```

**Nguyên nhân:**
- Disk failure
- Unexpected shutdown
- Storage corruption

**Giải pháp:**
```bash
# Stop node
./scripts/node/stop_nodes.sh
# hoặc (với symlink)
./stop_nodes.sh

# Backup corrupted storage
mv config/storage/node_0 config/storage/node_0.corrupted

# Restore from backup
tar -xzf metanode-storage-backup.tar.gz

# Start node
./scripts/node/run_nodes.sh
# hoặc (với symlink)
./run_nodes.sh

# Node sẽ sync từ peers
```

## Vấn đề performance

### Low throughput

**Triệu chứng:**
- Commits chậm (<50 commits/s)
- Transactions chậm

**Nguyên nhân:**
- Consensus parameters chưa tối ưu
- Network latency cao
- Storage I/O chậm

**Giải pháp:**
```rust
// Tune parameters trong src/node.rs
parameters.min_round_delay = Duration::from_millis(30);  // Giảm delay
parameters.max_blocks_per_sync = 64;  // Tăng batch size
```

### High latency

**Triệu chứng:**
- End-to-end latency >1s
- Commits mất >500ms

**Nguyên nhân:**
- Network latency
- Consensus timeout cao
- Storage I/O chậm

**Giải pháp:**
- Giảm `leader_timeout`
- Giảm `min_round_delay`
- Sử dụng SSD cho storage

## Vấn đề consensus

### Nodes không đồng thuận

**Triệu chứng:**
- Nodes commit khác nhau
- Log hiển thị conflicts

**Nguyên nhân:**
- Committee mismatch
- Clock skew
- Network partitions

**Giải pháp:**
```bash
# Kiểm tra committee
diff config/committee.json config/committee.json.backup

# Kiểm tra clock sync
date
# Tất cả nodes phải có clock đồng bộ

# Kiểm tra network
ping <other-node-ip>
```

### Out-of-order commits

**Triệu chứng:**
```
WARN: Received out-of-order commit: index=5, expected=3
```

**Nguyên nhân:**
- Network latency
- Node lag
- Normal behavior (sẽ được xử lý)

**Giải pháp:**
- Đây là hành vi bình thường
- CommitProcessor sẽ xử lý out-of-order commits
- Commits sẽ được execute theo thứ tự

## Vấn đề Epoch Transition

### Epoch không chuyển đổi được

**Triệu chứng:**
- Một số nodes transition, một số không
- Log hiển thị "quorum not reached"
- Nodes ở các epoch khác nhau

**Nguyên nhân:**
1. **Quorum không đủ**: Với 3 nodes online, cần 100% nodes phải vote (3/3)
2. **Votes không propagate**: Votes không được broadcast đúng cách
3. **Proposal hash mismatch**: Proposal hash không match giữa các nodes
4. **Node offline**: Một node offline không vote

**Giải pháp:**
```bash
# 1. Kiểm tra quorum status
./check_epoch_status.sh

# 2. Kiểm tra vote propagation
./scripts/analysis/analyze_vote_propagation.sh

# 3. Kiểm tra nodes online
ps aux | grep metanode

# 4. Nếu cần, restart node offline
./scripts/node/start_node.sh <node_id>
# hoặc (với symlink)
./start_node.sh <node_id>

# 5. Verify fork-safety sau transition
./scripts/analysis/verify_epoch_transition.sh
# hoặc (với symlink)
./verify_epoch_transition.sh
```

### Fork sau epoch transition

**Triệu chứng:**
- Nodes ở cùng epoch nhưng có `last_global_exec_index` khác nhau
- Nodes không thể validate blocks từ nhau
- Log hiển thị "Ancestor block not found"

**Nguyên nhân:**
- Nodes transition với `last_commit_index` khác nhau
- Nodes có `epoch_timestamp_ms` khác nhau
- Committee.json không đồng bộ

**Giải pháp:**
```bash
# 1. Verify fork-safety
./verify_epoch_transition.sh

# 2. Kiểm tra deterministic values trong logs
grep "Deterministic Values.*ALL NODES MUST MATCH" logs/latest/node_*.log

# 3. Nếu phát hiện fork, sync committee.json từ node đúng
cp config/committee_node_0.json config/committee_node_1.json
cp config/committee_node_0.json config/committee_node_2.json
cp config/committee_node_0.json config/committee_node_3.json

# 4. Restart các nodes
./scripts/node/restart_node.sh 1
# hoặc (với symlink)
./restart_node.sh 1
./scripts/node/restart_node.sh 2
./scripts/node/restart_node.sh 3
```

### Votes không được propagate

**Triệu chứng:**
- Node thấy quorum nhưng các nodes khác không thấy
- Log hiển thị "quorum not reached" mặc dù đã có đủ votes

**Nguyên nhân:**
- Votes không được include trong blocks
- Votes không được broadcast đúng cách
- Node đạt quorum và dừng broadcast votes (bug đã fix)

**Giải pháp:**
- Đảm bảo code đã được update với fix vote propagation
- Kiểm tra logs để xem votes có được broadcast không
- Sử dụng `analyze_vote_propagation.sh` để debug

## Debugging

### Enable debug logs

```bash
# Set log level
export RUST_LOG=metanode=debug,consensus_core=debug

# Start node
./target/release/metanode start --config config/node_0.toml
```

### Analyze logs

```bash
# Xem commits
grep "Executing commit" logs/node_0.log | tail -20

# Xem transactions
grep "Transaction submitted" logs/node_0.log

# Xem errors
grep "ERROR" logs/node_0.log

# Xem warnings
grep "WARN" logs/node_0.log

# Count commits
grep -c "Executing commit" logs/node_0.log
```

### Network debugging

```bash
# Test connectivity
for port in 9000 9001 9002 9003; do
    echo "Testing port $port"
    nc -zv 127.0.0.1 $port
done

# Monitor network traffic
sudo tcpdump -i lo port 9000

# Check connections
netstat -an | grep 9000
```

### Performance profiling

```bash
# CPU profiling
perf record -g ./target/release/metanode start --config config/node_0.toml
perf report

# Memory profiling
valgrind --tool=massif ./target/release/metanode start --config config/node_0.toml
```

## Common Issues Checklist

- [ ] Nodes không khởi động → Kiểm tra ports, config
- [ ] Nodes không kết nối → Kiểm tra network, firewall
- [ ] RPC không respond → Đợi consensus ready
- [ ] Transactions fail → Kiểm tra consensus state
- [ ] Low performance → Tune parameters
- [ ] Storage issues → Kiểm tra disk space, corruption
- [ ] Consensus issues → Kiểm tra committee, clock sync

## Getting Help

1. **Check logs**: Luôn kiểm tra logs trước
2. **Reproduce**: Tái tạo issue trong development
3. **Document**: Ghi lại steps để reproduce
4. **Search**: Tìm trong documentation và issues
5. **Ask**: Hỏi trong community hoặc create issue

## Useful Commands

```bash
# Check node status
ps aux | grep metanode

# Check ports
netstat -tlnp | grep -E '9000|9100|10100'

# Check logs
tail -f logs/node_0.log

# Check metrics
curl http://127.0.0.1:9100/metrics

# Check RPC
curl http://127.0.0.1:10100/ready

# Stop all nodes
./scripts/node/stop_nodes.sh
# hoặc (với symlink)
./stop_nodes.sh

# Start all nodes
./scripts/node/run_nodes.sh
# hoặc (với symlink)
./run_nodes.sh

# Check disk space
df -h

# Check storage size
du -sh config/storage/*
```

