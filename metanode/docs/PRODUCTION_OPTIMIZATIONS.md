# Production Optimizations for High-Throughput Transaction Processing

## Vấn đề đã được fix

### Vấn đề: "Chạy một lúc giao dịch đứng"
- **Triệu chứng**: Sau khi gửi một số lượng transaction nhất định (khoảng 26-30 transactions), hệ thống bị đứng, transactions không được xử lý
- **Nguyên nhân**: 
  1. Connection pool bị exhausted khi có quá nhiều transactions đồng thời
  2. Connection không ổn định, bị đóng sớm trước khi Rust server đọc xong
  3. Không có rate limiting, gửi quá nhiều batch cùng lúc
  4. Retry logic không đủ mạnh

## Giải pháp đã triển khai

### 1. Go Client (`pkg/txsender/client.go`)

#### Connection Pool Management
- **Graceful degradation**: Không fail nếu một số connection thất bại, chỉ log warning và tiếp tục
- **Background monitor**: Check mỗi 2 giây (thay vì 5 giây) để maintain pool nhanh hơn
- **Buffer sizes**: Tăng từ 256KB lên 512KB cho read/write buffers
- **Timeout tối ưu**: 
  - Pool timeout: 2 giây (từ 5 giây) - nhanh hơn cho localhost
  - Dial timeout: 3 giây (từ 5 giây)
  - Write timeout: 5 giây (từ 10 giây) cho localhost

#### Metrics & Monitoring
```go
- totalSent: Tổng số transaction đã gửi
- totalFailed: Tổng số transaction thất bại
- poolExhausted: Số lần pool bị exhausted
- connCreated: Số connection được tạo mới
- activeConns: Số connection đang active
```
- Log metrics mỗi 30 giây để monitor health

#### Retry Logic
- **Tăng retries**: Từ 3 lên 5 retries
- **Exponential backoff**: 10ms, 20ms, 40ms, 80ms, 160ms
- **Retry cho nhiều loại lỗi**: `broken pipe`, `connection reset`, `i/o timeout`

### 2. Go Block Processor (`block_processor.go`)

#### Rate Limiting
- **Rate limiter**: 50ms giữa các batch (tối đa 20 batches/giây)
- **Ticker interval**: Giảm từ 10ms xuống 5ms để xử lý nhanh hơn
- **Tránh quá tải**: Đảm bảo không gửi quá nhiều batch cùng lúc

#### Batch Processing
- **Batch size**: 20 transactions per batch (từ 10) - cân bằng giữa throughput và connection pool
- **Concurrent sends**: 100 goroutines (từ 50) với semaphore để giới hạn
- **Async sending**: Gửi batch trong goroutine riêng để không block main loop

#### Retry Logic
- **Tăng retries**: Từ 3 lên 5 retries
- **Exponential backoff**: 50ms, 100ms, 200ms, 400ms, 800ms
- **Non-blocking**: Retry trong goroutine riêng, không block batch processing

#### Logging Optimization
- Chỉ log batch đầu tiên, cuối cùng, hoặc mỗi 10 batch
- Giảm log spam khi có nhiều transactions

### 3. Rust RPC Server (`rpc.rs`)

#### Concurrent Connections
- **Tăng limit**: Từ 200 lên 500 concurrent connections
- **TCP_NODELAY**: Set trên mỗi accepted connection
- **Timeout ngắn hơn**: 
  - Length prefix: 5 giây (từ 30 giây)
  - Transaction data: 10 giây (từ 30 giây)

## Kết quả mong đợi

### 1. Ổn định hơn
- ✅ Graceful degradation khi connection thất bại
- ✅ Auto-recovery với background monitor
- ✅ Metrics để monitor health
- ✅ Connection health tracking

### 2. Thông lượng cao hơn
- ✅ Buffer sizes lớn hơn (512KB)
- ✅ Rate limiting để tránh quá tải
- ✅ Async sending với semaphore
- ✅ Batch size tối ưu (20 transactions)

### 3. Reliability
- ✅ Retry logic tốt hơn (5 retries với exponential backoff)
- ✅ Connection health tracking
- ✅ Error handling tốt hơn
- ✅ Non-blocking operations

### 4. Production-ready
- ✅ Metrics và monitoring
- ✅ Graceful degradation
- ✅ Rate limiting và backpressure
- ✅ Optimized cho localhost

## Cấu hình tối ưu cho Production

### Go Client
- **Pool size**: 100 connections
- **Buffer sizes**: 512KB read/write
- **Timeouts**: 2s pool, 3s dial, 5s write
- **Retries**: 5 với exponential backoff

### Go Block Processor
- **Ticker interval**: 5ms
- **Rate limiter**: 50ms (20 batches/giây)
- **Batch size**: 20 transactions
- **Concurrent sends**: 100 goroutines
- **Retries**: 5 với exponential backoff

### Rust RPC Server
- **Concurrent connections**: 500
- **TCP_NODELAY**: Enabled
- **Timeouts**: 5s length prefix, 10s transaction data

## Monitoring

### Metrics được log mỗi 30 giây:
```
📊 [TX CLIENT] Metrics: sent=X, failed=Y, pool_exhausted=Z, conn_created=W, active_conns=V, pool_size=U
```

### Các chỉ số quan trọng:
- **pool_exhausted**: Nếu tăng liên tục, cần tăng pool size hoặc rate limiter
- **failed**: Nếu tăng, có thể có vấn đề với network hoặc Rust server
- **active_conns**: Nên gần bằng pool_size, nếu thấp hơn nhiều thì có connection bị hỏng

## Troubleshooting

### Nếu vẫn có transactions bị đứng:
1. Kiểm tra metrics: `pool_exhausted` có tăng không?
2. Kiểm tra Rust logs: Có timeout errors không?
3. Tăng pool size nếu cần: Thay đổi `poolSize := 100` thành giá trị lớn hơn
4. Tăng rate limiter: Thay đổi `50 * time.Millisecond` thành giá trị lớn hơn (ví dụ: 100ms)

### Nếu connection pool bị exhausted:
1. Tăng pool size trong `block_processor.go`: `poolSize := 100` → `poolSize := 200`
2. Tăng rate limiter: `50 * time.Millisecond` → `100 * time.Millisecond`
3. Giảm batch size: `maxTransactionsPerBatch = 20` → `maxTransactionsPerBatch = 10`

## Best Practices

1. **Monitor metrics**: Theo dõi `pool_exhausted` và `failed` để phát hiện vấn đề sớm
2. **Tune parameters**: Điều chỉnh pool size, rate limiter, batch size dựa trên workload
3. **Log analysis**: Phân tích logs để tìm patterns trong errors
4. **Load testing**: Test với workload thực tế để tìm optimal parameters

