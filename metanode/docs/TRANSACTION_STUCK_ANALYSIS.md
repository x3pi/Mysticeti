# Phân tích: Tại sao giao dịch bị đứng

## Giao dịch: `74a65d1969fc3048e03a3e6282b5ffea1001947f750208fb7f326064c5de960f`

## Luồng mong đợi

```
1. Go Sub Node (TxsProcessor2)
   ↓
   ✅ Transaction added to pending pool
   ✅ Successfully sent batch [1/1]: 1 transactions to Rust MetaNode
   ↓
2. Rust Node 0 RPC Server (127.0.0.1:10100)
   ↓
   ❌ KHÔNG THẤY LOG "Received transaction"
   ↓
3. Rust Consensus
   ↓
   ❌ Executing commit: 0 transactions (tất cả commits đều 0)
   ↓
4. Go Master Executor
   ↓
   ❌ Chỉ nhận empty blocks
```

## Vấn đề phát hiện

### 1. Go Sub Node ✅
- Transaction được thêm vào pending pool
- Batch được gửi thành công: `Successfully sent batch [1/1]: 1 transactions`

### 2. Rust RPC Server ❌
- **KHÔNG THẤY** log `📥 [TX FLOW] Received length-prefixed transaction data via RPC`
- **KHÔNG THẤY** log `📥 [TX FLOW] Received transaction data via UDS`
- **KHÔNG THẤY** log `📤 [TX FLOW] Preparing to submit`

### 3. Rust Consensus ❌
- Tất cả commits đều có `0 transactions`
- Log: `Executing commit #X: N blocks, 0 transactions`

## Nguyên nhân có thể

### 1. Connection không được thiết lập
- Go client gửi nhưng Rust RPC server không nhận được
- Có thể do:
  - Port 10100 không được bind
  - Connection bị drop trước khi Rust đọc được
  - Network issue

### 2. Protocol mismatch
- Go client gửi length-prefixed binary
- Rust RPC server có thể không parse đúng
- Timeout khi đọc length prefix hoặc data

### 3. Connection pool issue
- Connection pool exhausted
- Connections bị drop
- Retry logic không hoạt động

### 4. Rust RPC Server không start
- RPC server không được khởi động
- Port conflict
- Binding error

## Debugging Steps

### Step 1: Kiểm tra Rust RPC Server
```bash
# Kiểm tra RPC server có start không
grep "RPC server started" Mysticeti/metanode/logs/*/node_0.log

# Kiểm tra port có được bind không
netstat -tuln | grep 10100
# hoặc
ss -tuln | grep 10100
```

### Step 2: Kiểm tra Go Client Connection
```bash
# Kiểm tra metrics
grep "TX CLIENT.*Metrics" mtn-simple-2025/cmd/simple_chain/sample/simple/data-write/logs/*/App.log

# Kiểm tra pool_exhausted
grep "pool_exhausted" mtn-simple-2025/cmd/simple_chain/sample/simple/data-write/logs/*/App.log

# Kiểm tra failed sends
grep "Failed to send" mtn-simple-2025/cmd/simple_chain/sample/simple/data-write/logs/*/App.log
```

### Step 3: Kiểm tra Timeout Errors
```bash
# Kiểm tra timeout trong Rust logs
grep "Timeout reading" Mysticeti/metanode/logs/*/node_0.log

# Kiểm tra connection errors
grep "Failed to read\|Failed to process" Mysticeti/metanode/logs/*/node_0.log
```

### Step 4: Test Connection Manually
```bash
# Test gửi transaction thủ công đến Rust RPC server
echo -n -e "\x00\x00\x00\x01\x01" | nc 127.0.0.1 10100

# Hoặc dùng curl để test HTTP endpoint
curl -X POST http://127.0.0.1:10100/submit -d "test"
```

## Giải pháp đề xuất

### 1. Thêm logging chi tiết hơn
- Log khi connection được accept
- Log khi đọc length prefix
- Log khi đọc transaction data
- Log khi submit vào consensus

### 2. Kiểm tra connection health
- Monitor active connections
- Track connection failures
- Alert khi pool exhausted

### 3. Tăng timeout
- Tăng timeout cho length prefix (5s → 10s)
- Tăng timeout cho transaction data (10s → 30s)

### 4. Kiểm tra protocol
- Verify length-prefixed format
- Verify protobuf encoding
- Test với sample transaction

## Next Steps

1. ✅ Kiểm tra Rust RPC server có start không
2. ✅ Kiểm tra port 10100 có được bind không
3. ✅ Kiểm tra Go client metrics
4. ✅ Kiểm tra timeout errors
5. ✅ Test connection manually
6. ✅ Thêm logging chi tiết hơn

