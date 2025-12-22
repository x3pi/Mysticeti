# Implementation Plan: Channel-Based Architecture

## Tổng quan

Implement channel-based architecture với multiple workers để gửi transaction batches liên tục từ Go-sub sang Rust.

## Architecture

```
Go-Sub Node:
┌─────────────────────────────────────────┐
│ TxsProcessor2                          │
│ - Lấy transactions từ pool              │
│ - Batch thành groups (50 tx/batch)      │
└──────────────┬──────────────────────────┘
               │
               ▼
┌─────────────────────────────────────────┐
│ ChannelBasedSender                      │
│ - Buffered channel (1000 batches)      │
│ - Multiple workers (10-20 workers)     │
└──────────────┬──────────────────────────┘
               │
               ├──► Worker 1 (UDS conn) ──┐
               ├──► Worker 2 (UDS conn) ──┤
               ├──► Worker 3 (UDS conn) ──┼──► Rust UDS Server
               ├──► ...                    │
               └──► Worker N (UDS conn) ──┘
```

## Implementation Steps

### Phase 1: Channel-Based Sender (Go)

1. **Tạo `channel_sender.go`** ✅
   - `ChannelBasedSender`: Quản lý channel và workers
   - `ChannelWorker`: Worker với persistent connection
   - Buffered channel: 1000 batches
   - Multiple workers: 10-20 workers

2. **Update `block_processor.go`**
   - Thay `txsender.NewClient` bằng `txsender.NewChannelBasedSender`
   - Sử dụng UDS path: `/tmp/metanode-tx-0.sock`
   - Gửi batches vào channel thay vì gọi `SendTransaction` trực tiếp

3. **Testing**
   - Test với 1 worker
   - Test với nhiều workers
   - Test với channel đầy

### Phase 2: Optimize Rust UDS Server

1. **Update `tx_socket_server.rs`**
   - Tăng số lượng concurrent handlers
   - Loại bỏ response (fire-and-forget)
   - Optimize buffer sizes

2. **Testing**
   - Test với nhiều connections đồng thời
   - Test throughput

### Phase 3: Integration & Optimization

1. **Tune parameters**
   - Channel buffer size
   - Worker count
   - Batch size

2. **Monitoring**
   - Metrics: sent, failed, batches
   - Channel utilization
   - Worker utilization

## Code Changes

### Go: `block_processor.go`

```go
// Thay đổi từ:
bp.txClient, err = txsender.NewClient(rpcAddress, poolSize)

// Thành:
// Sử dụng UDS cho localhost
udsPath := fmt.Sprintf("/tmp/metanode-tx-0.sock")
bp.txSender, err = txsender.NewChannelBasedSender(udsPath, 10, 1000)
```

```go
// Thay đổi từ:
if err := bp.txClient.SendTransaction(bTransaction); err != nil {

// Thành:
if err := bp.txSender.SendBatch(bTransaction); err != nil {
```

### Rust: `tx_socket_server.rs`

```rust
// Tăng concurrent handlers
let semaphore = Arc::new(Semaphore::new(100)); // Tăng từ default

// Loại bỏ response (fire-and-forget)
// Không gửi response, chỉ xử lý và submit
```

## Expected Performance

- **Throughput**: 10x-100x improvement
- **Latency**: <0.1ms (không cần delay)
- **Scalability**: Unlimited (channel buffer)
- **Reliability**: Tự động reconnect khi connection lỗi

## Migration Path

1. **Step 1**: Implement `ChannelBasedSender` (✅ done)
2. **Step 2**: Update `block_processor.go` để sử dụng channel sender
3. **Step 3**: Test và tune parameters
4. **Step 4**: Optimize Rust UDS server
5. **Step 5**: Production deployment

