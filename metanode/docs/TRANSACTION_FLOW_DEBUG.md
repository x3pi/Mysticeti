# Transaction Flow Debug Guide

## Luồng giao dịch đầy đủ

```
┌─────────────────────────────────────────────────────────────────┐
│                    GO SUB NODE                                  │
│  (config-sub-write.json, ServiceType: SUB-WRITE)               │
│                                                                 │
│  block_processor.go: TxsProcessor2()                          │
│    ↓                                                            │
│  1. ProcessTransactionsInPoolSub()                              │
│     → Lấy transactions từ transaction pool                     │
│     → Add vào pendingTxManager với status=StatusProcessing     │
│    ↓                                                            │
│  2. MarshalTransactions(txs)                                    │
│     → []byte (protobuf Transactions message)                   │
│     → Format: [length_prefix: 4 bytes][protobuf_data]          │
│    ↓                                                            │
│  3. txClient.SendTransaction(bTransaction)                     │
│     → pkg/txsender/client.go                                    │
│     → Gửi qua TCP (127.0.0.1:10100)                            │
│     → Protocol: length-prefixed binary                         │
│     → Connection pool: 100 connections                          │
│     → Rate limiting: 20 batches/giây                           │
└───────────────────────┬─────────────────────────────────────────┘
                        │
                        │ Transaction Data (length-prefixed protobuf)
                        │
                        ▼
┌─────────────────────────────────────────────────────────────────┐
│                  RUST CONSENSUS (MetaNode Node 0)              │
│                                                                 │
│  src/rpc.rs: RpcServer.start()                                 │
│    ↓                                                            │
│  1. TcpListener.bind("127.0.0.1:10100")                         │
│     → Accept connections                                        │
│     → Semaphore: 500 concurrent connections                     │
│    ↓                                                            │
│  2. Detect protocol:                                            │
│     → Read first 4 bytes (length prefix)                        │
│     → Timeout: 5s cho length prefix                             │
│     → Timeout: 10s cho transaction data                         │
│    ↓                                                            │
│  3. process_transaction_data()                                  │
│     → Decode protobuf (Transactions hoặc Transaction)          │
│     → Extract individual transactions                            │
│     → Calculate transaction hash                               │
│    ↓                                                            │
│  4. transaction_client.submit()                                 │
│     → Submit vào consensus authority                            │
│     → Transaction được thêm vào DAG                            │
│    ↓                                                            │
│  5. Consensus processing:                                      │
│     → Leader tạo blocks                                         │
│     → Blocks chứa transactions                                 │
│     → Commit blocks khi đủ quorum                               │
│    ↓                                                            │
│  6. CommitProcessor.process_commit()                            │
│     → Xử lý committed sub-DAG                                   │
│     → Extract transactions từ blocks                            │
│    ↓                                                            │
│  7. ExecutorClient.send_committed_subdag()                     │
│     → Chỉ Node 0 có executor_enabled=true                      │
│     → Convert to protobuf (CommittedEpochData)                  │
│     → Gửi qua UDS (/tmp/executor0.sock)                         │
└───────────────────────┬─────────────────────────────────────────┘
                        │
                        │ Committed Blocks (protobuf CommittedEpochData)
                        │
                        ▼
┌─────────────────────────────────────────────────────────────────┐
│                    GO MASTER NODE                               │
│  (config-master.json, ServiceType: MASTER)                      │
│                                                                 │
│  block_processor.go: runSocketExecutor()                        │
│    ↓                                                            │
│  1. Listen UDS (/tmp/executor0.sock)                           │
│     → Nhận CommittedEpochData protobuf                         │
│    ↓                                                            │
│  2. Extract transactions:                                      │
│     → Iterate qua tất cả blocks trong epochData.Blocks          │
│     → Unmarshal ms.Digest (raw transaction data)                │
│     → Try Transaction protobuf first                            │
│     → Fallback to Transactions protobuf                         │
│    ↓                                                            │
│  3. ProcessTransactions():                                     │
│     → Execute transactions                                      │
│     → Create Go block                                           │
│     → Update chain state                                        │
│    ↓                                                            │
│  4. Broadcast block to Go Sub Node                              │
│     → Via pub/sub (block_data_topic)                            │
│    ↓                                                            │
│  5. Go Sub Node: TxsProcessor()                                │
│     → Nhận blocks từ Go Master                                  │
│     → Extract receipts                                          │
│     → Broadcast receipts to clients                             │
└─────────────────────────────────────────────────────────────────┘
```

## Các điểm kiểm tra khi giao dịch bị đứng

### 1. Go Sub Node → Rust (Transaction Submission)

**Kiểm tra trong Go Sub logs:**
```bash
grep "TX FLOW.*Sending\|TX FLOW.*Successfully sent" go-sub.log
```

**Các log mong đợi:**
- `✅ [TX FLOW] Transaction added to pending pool`
- `📤 [TX FLOW] Sending batch [X/Y]: N transactions to Rust MetaNode`
- `✅ [TX FLOW] Successfully sent batch [X/Y]: N transactions`

**Nếu không thấy:**
- Kiểm tra `txClient` có được tạo không
- Kiểm tra connection pool có exhausted không: `pool_exhausted` trong metrics
- Kiểm tra có lỗi gửi không: `Failed to send batch`

**Kiểm tra trong Rust Node 0 logs:**
```bash
grep "TX FLOW.*Received\|RPC server" node_0.log
```

**Các log mong đợi:**
- `📥 [TX FLOW] Received length-prefixed transaction data via RPC`
- `📥 [TX FLOW] Received transaction data via UDS`
- `✅ [TX FLOW] Successfully submitted transaction`

**Nếu không thấy:**
- Kiểm tra RPC server có start không: `RPC server started on 127.0.0.1:10100`
- Kiểm tra có timeout errors không: `Timeout reading length prefix`
- Kiểm tra connection có bị reject không

### 2. Rust Consensus → Commit

**Kiểm tra trong Rust Node 0 logs:**
```bash
grep "Executing commit\|TX FLOW.*Sent committed" node_0.log
```

**Các log mong đợi:**
- `🔷 [Global Index: X] Executing commit #X: N blocks, M transactions`
- `📤 [TX FLOW] Sent committed sub-DAG to Go executor: total_tx=M`

**Nếu `total_tx=0`:**
- Transaction không được submit vào consensus
- Hoặc transaction bị reject bởi consensus
- Kiểm tra logs về transaction submission errors

### 3. Rust → Go Master (Block Execution)

**Kiểm tra trong Rust Node 0 logs:**
```bash
grep "TX FLOW.*Sent committed" node_0.log
```

**Các log mong đợi:**
- `📤 [TX FLOW] Sent committed sub-DAG to Go executor: commit_index=X, total_tx=Y`

**Kiểm tra trong Go Master logs:**
```bash
grep "TX FLOW.*Received\|TX FLOW.*Processing\|TX FLOW.*Extracting" go-master.log
```

**Các log mong đợi:**
- `📥 [TX FLOW] Received committed epoch data from Rust`
- `📦 [TX FLOW] Processing committed sub-DAG: N blocks`
- `📦 [TX FLOW] Extracting transactions from Rust block[X/Y]: transactions=Z`

**Nếu không thấy:**
- Kiểm tra Go Master có listen UDS không: `/tmp/executor0.sock`
- Kiểm tra có lỗi unmarshal không: `Failed to unmarshal transaction`

### 4. Go Master → Go Sub (Block Broadcast)

**Kiểm tra trong Go Master logs:**
```bash
grep "TX FLOW.*Go block.*created\|Broadcast" go-master.log
```

**Các log mong đợi:**
- `✅ [TX FLOW] Go block #X created successfully: tx_count=Y`
- Block được broadcast qua pub/sub

**Kiểm tra trong Go Sub logs:**
```bash
grep "TX FLOW.*Received block\|TX FLOW.*Broadcast receipt" go-sub.log
```

**Các log mong đợi:**
- Go Sub nhận blocks từ Go Master
- Receipts được broadcast đến clients

## Debugging Steps cho Transaction `74a65d1969fc3048e03a3e6282b5ffea1001947f750208fb7f326064c5de960f`

### Step 1: Kiểm tra Go Sub Node
```bash
# Tìm transaction trong Go Sub logs
grep "74a65d1969fc3048e03a3e6282b5ffea1001947f750208fb7f326064c5de960f" \
  mtn-simple-2025/cmd/simple_chain/sample/simple/data-write/logs/*/App.log

# Kiểm tra có được gửi không
grep -A 5 "74a65d1969fc3048e03a3e6282b5ffea1001947f750208fb7f326064c5de960f" \
  mtn-simple-2025/cmd/simple_chain/sample/simple/data-write/logs/*/App.log | \
  grep "Successfully sent\|Failed to send"
```

### Step 2: Kiểm tra Rust Node 0
```bash
# Tìm transaction trong Rust logs
grep "74a65d1969fc3048e03a3e6282b5ffea1001947f750208fb7f326064c5de960f" \
  Mysticeti/metanode/logs/*/node_0.log

# Kiểm tra RPC server có nhận được không
grep -A 5 "TX FLOW.*Received" \
  Mysticeti/metanode/logs/*/node_0.log | \
  grep -i "74a65d"
```

### Step 3: Kiểm tra Rust Commit
```bash
# Kiểm tra commit có chứa transaction không
grep "Executing commit" \
  Mysticeti/metanode/logs/*/node_0.log | \
  grep -v "0 transactions"
```

### Step 4: Kiểm tra Go Master
```bash
# Tìm transaction trong Go Master logs
grep "74a65d1969fc3048e03a3e6282b5ffea1001947f750208fb7f326064c5de960f" \
  mtn-simple-2025/cmd/simple_chain/sample/simple/data/logs/*/App.log
```

## Common Issues

### Issue 1: Transaction không được gửi từ Go Sub
**Triệu chứng:**
- Go Sub logs: `Successfully sent batch` nhưng không có transaction hash
- Hoặc: `Failed to send batch`

**Nguyên nhân:**
- Connection pool exhausted
- Network timeout
- Rust RPC server không sẵn sàng

**Giải pháp:**
- Kiểm tra metrics: `pool_exhausted` trong Go client logs
- Tăng connection pool size
- Kiểm tra Rust RPC server có start không

### Issue 2: Transaction không được nhận bởi Rust
**Triệu chứng:**
- Go Sub logs: `Successfully sent batch`
- Rust logs: Không có `Received transaction`

**Nguyên nhân:**
- Protocol mismatch (length-prefixed vs HTTP)
- Connection timeout
- Rust RPC server crash

**Giải pháp:**
- Kiểm tra Rust RPC server logs cho errors
- Kiểm tra timeout settings
- Verify protocol format

### Issue 3: Transaction không được commit
**Triệu chứng:**
- Rust logs: `Received transaction` nhưng `Executing commit: 0 transactions`

**Nguyên nhân:**
- Transaction không được submit vào consensus
- Transaction bị reject bởi consensus logic
- Consensus không tạo blocks với transactions

**Giải pháp:**
- Kiểm tra consensus submission logs
- Kiểm tra transaction validation
- Kiểm tra leader selection

### Issue 4: Transaction không được execute
**Triệu chứng:**
- Rust logs: `Sent committed sub-DAG: total_tx=0`
- Hoặc Go Master logs: Không có transaction

**Nguyên nhân:**
- Executor client không enabled
- UDS connection failed
- Protobuf unmarshal error

**Giải pháp:**
- Kiểm tra `executor_enabled=true` trong `node_0.toml`
- Kiểm tra UDS socket: `/tmp/executor0.sock`
- Kiểm tra protobuf format

## Metrics để Monitor

### Go Client Metrics (mỗi 30 giây):
```
📊 [TX CLIENT] Metrics: sent=X, failed=Y, pool_exhausted=Z, conn_created=W, active_conns=V, pool_size=U
```

**Các chỉ số quan trọng:**
- `pool_exhausted`: Nếu tăng liên tục → cần tăng pool size hoặc rate limiter
- `failed`: Nếu tăng → có vấn đề với network hoặc Rust server
- `active_conns`: Nên gần bằng `pool_size`

### Rust RPC Server:
- Concurrent connections: Max 500
- Timeout errors: Cần monitor

### Rust Commit Processor:
- `Executing commit: N blocks, M transactions`
- Nếu `M=0` liên tục → transactions không được submit

## Script để Trace Transaction

Sử dụng script `trace_transaction.sh`:
```bash
./scripts/trace_transaction.sh 74a65d1969fc3048e03a3e6282b5ffea1001947f750208fb7f326064c5de960f
```

Script sẽ tìm transaction trong:
1. Go Sub Node logs
2. Rust Node 0 logs (RPC reception)
3. Rust Node 0 logs (Commit)
4. Go Master Node logs (Execution)

