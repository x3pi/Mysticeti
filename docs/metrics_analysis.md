# Phân Tích Metrics - Consensus Node

## 📊 Tổng Quan Hệ Thống

### Trạng Thái Node
- **Last Commit Index**: 1169
- **Highest Accepted Round**: 1174
- **Protocol Version**: 105
- **Authority Index**: 0 (node-0)
- **Network Type**: Tonic

### Block Statistics
- **Total Accepted Blocks**: 735 (183 own + 552 others)
- **Total Proposed Blocks**: 183 (182 normal + 1 forced)
- **Last Committed Leader Round**: 1171
- **Last Decided Leader Round**: 1171

---

## 🔄 Network Performance

### Inbound Requests
- **FetchBlocks**: 2 requests, 0 inflight
- **GetLatestRounds**: 6 requests, 0 inflight  
- **SubscribeBlocks**: 3 requests, **3 inflight** ⚠️

### Outbound Requests
- **FetchBlocks**: 2 requests, 0 inflight
- **GetLatestRounds**: 6 requests, 0 inflight
- **SubscribeBlocks**: 3 requests, **3 inflight** ⚠️

### Request Latency
- **FetchBlocks**: 
  - Inbound: 2 requests, avg ~0.38ms
  - Outbound: 2 requests, avg ~1.45ms
- **GetLatestRounds**:
  - Inbound: 6 requests, avg ~0.68ms
  - Outbound: 6 requests, avg ~2.2ms
- **SubscribeBlocks**: 
  - Inbound: 3 requests, 0 completed (đang chạy)
  - Outbound: 3 requests, 0 completed (đang chạy)

**⚠️ Lưu ý**: SubscribeBlocks có 3 requests đang inflight, có thể là stream đang hoạt động bình thường.

---

## ⏱️ Performance Metrics

### Block Proposal Interval
- **Average**: ~1.12 giây (204.45s / 183 blocks)
- **Distribution**: Hầu hết trong khoảng 0.3-0.5 giây
- **Total Proposals**: 183 blocks

### Block Commit Latency
- **Total Commits**: 733
- **Average Latency**: ~3.18 giây (2327.6s / 733)
- **Distribution**:
  - 76 commits trong 0.6-0.7s
  - 541 commits trong 0.8-0.9s
  - Hầu hết commits < 1 giây
  - Một số commits mất đến 3-4 giây

### Quorum Receive Latency
- **Total Quorums**: 684
- **Average**: ~159.6 giây (109138.3s / 684) ⚠️
- **Distribution**: 
  - 179 quorums < 0.005s
  - 180 quorums < 0.25s
  - 183 quorums < 2.5s
  - **Có một số quorums rất chậm** (có thể do startup hoặc network issues)

### Block Processing Times
- **Core::add_blocks**: Avg ~0.12ms (554 operations)
- **Core::try_commit**: Avg ~0.065ms (734 operations)
- **Core::try_new_block**: Avg ~0.11ms (730 operations)
- **DagState::flush**: Avg ~0.041ms (1352 operations)

---

## 🎯 Consensus Health

### Leader Performance
- **Committed Leaders**:
  - node-0: 36 direct commits
  - node-2: 43 direct commits
  - node-3: 104 direct commits + 1 indirect skip
- **Leader Timeouts**: 179 (false timeouts - có thể là normal)

### Block Acceptance by Authority
- **Highest Accepted Round per Authority**:
  - node-0: 1173
  - node-1: 1173
  - node-2: 1173
  - node-3: 1174 (cao nhất)

### Missing Blocks
- **Total Missing Blocks**: 2 (đã được resolve)
- **Current Missing Blocks**: 0 ✅
- **Missing Ancestors**: 0 ✅
- **Suspended Blocks**: 0 ✅

### Block Suspensions
- **Total Suspensions**: 2 (từ node-1)
- **Suspension Time**: ~2.18ms average
- **Unsuspensions**: 2 (đã resolve)

---

## 📈 Throughput & Efficiency

### Blocks Per Commit
- **Average**: 4 blocks/commit (733 blocks / 183 commits)
- **Distribution**: Hầu hết 4-8 blocks per commit

### Transaction Processing
- **Certifier Accepted Transactions**: 181 (chỉ từ node-0)
- **Certifier Output Blocks**: 727 proposed blocks
- **Finalizer Output Commits**: 183 direct commits

### Block Size & Content
- **Average Block Size**: ~325 bytes (59563 / 183)
- **Average Transactions per Block**: 1 transaction
- **Average Ancestors per Block**: ~4 ancestors (730 / 183)

---

## 🔍 Network Connectivity

### Subscriptions
- **Subscribed To**: 3 peers (node-1, node-2, node-3)
- **Subscribed By**: 3 peers (node-1, node-2, node-3)
- **Subscribed Blocks Received**: 184 blocks từ mỗi peer
- **Verified Blocks**: 184 blocks từ mỗi peer ✅

### Connection Attempts
- **node-1**: 1 success, 1 failure
- **node-2**: 1 success, 2 failures
- **node-3**: 1 success, 3 failures

**⚠️ Lưu ý**: Có một số connection failures ban đầu, nhưng cuối cùng đều thành công.

---

## 🚨 Potential Issues

### 1. Quorum Receive Latency
- Average latency rất cao (~159s) nhưng distribution cho thấy hầu hết < 2.5s
- Có thể do một số quorums rất chậm trong quá khứ (startup phase)
- **Recommendation**: Monitor thêm để xem có pattern không

### 2. Block Commit Latency
- Một số commits mất 3-4 giây
- **Recommendation**: Kiểm tra network latency và disk I/O

### 3. SubscribeBlocks Inflight
- 3 requests đang inflight - có thể là normal (streaming connections)
- **Recommendation**: Monitor để đảm bảo không bị stuck

---

## ✅ System Health Summary

### Healthy Indicators
- ✅ No missing blocks currently
- ✅ No suspended blocks
- ✅ All peers connected and verified
- ✅ Consistent block proposals
- ✅ Normal commit rate (~4 blocks/commit)

### Areas to Monitor
- ⚠️ Quorum receive latency (một số outliers)
- ⚠️ Block commit latency (một số commits chậm)
- ⚠️ Connection failures ban đầu (đã resolve)

---

## 📝 Recommendations

1. **Monitor Quorum Latency**: Theo dõi `quorum_receive_latency` để phát hiện network issues
2. **Optimize Commit Latency**: Kiểm tra disk I/O và network bandwidth
3. **Track Connection Stability**: Monitor `subscriber_connection_attempts` để đảm bảo stable connections
4. **Block Proposal Rate**: Hiện tại ~1 block/giây, có thể tối ưu nếu cần throughput cao hơn

---

*Metrics được thu thập từ node-0 tại thời điểm commit index 1169*

