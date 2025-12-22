# Phân tích: Tại sao giao dịch bị đứng sau vài giao dịch

## Vấn đề phát hiện

### 1. **Rate Limiter Blocking Main Loop**

```go
// TxsProcessor2()
for {
    <-ticker.C  // Mỗi 5ms
    
    // ... process transactions ...
    
    for batchStart := 0; batchStart < totalTxs; batchStart += maxTransactionsPerBatch {
        <-rateLimiter.C  // ⚠️ BLOCKING: Đợi 50ms giữa mỗi batch
        // ...
        semaphore <- struct{}{}  // ⚠️ BLOCKING: Đợi nếu semaphore đầy (100 goroutines)
        go func() {
            // Gửi transaction
        }()
    }
}
```

**Vấn đề:**
- Rate limiter: 50ms = 20 batches/giây
- Nếu có 100 transactions (5 batches), sẽ mất 5 * 50ms = 250ms chỉ để rate limit
- Main loop bị block bởi rate limiter

### 2. **Semaphore Blocking**

```go
semaphore := make(chan struct{}, 100)  // Max 100 concurrent sends

// Trong loop:
semaphore <- struct{}{}  // ⚠️ BLOCK nếu đã có 100 goroutines đang chạy
go func() {
    defer func() { <-semaphore }()
    // Gửi transaction (có thể mất vài giây nếu connection pool exhausted)
}()
```

**Vấn đề:**
- Nếu 100 goroutines đang chờ connection từ pool (timeout 2s mỗi goroutine)
- Semaphore sẽ đầy và block main loop
- Main loop không thể spawn goroutines mới
- Hệ thống bị đứng

### 3. **Connection Pool Exhaustion Cascade**

```
Timeline:
T=0ms:   Batch 1 spawns goroutine → Acquires semaphore → Waits for connection (2s timeout)
T=50ms:  Batch 2 spawns goroutine → Acquires semaphore → Waits for connection (2s timeout)
T=100ms: Batch 3 spawns goroutine → Acquires semaphore → Waits for connection (2s timeout)
...
T=5000ms: 100 goroutines đang chờ connection, semaphore đầy
T=5000ms: Main loop bị block tại `semaphore <- struct{}{}`
T=7000ms: Goroutines bắt đầu timeout và release semaphore
T=7000ms: Main loop tiếp tục, nhưng connection pool vẫn exhausted
```

**Vấn đề:**
- Connection pool: 100 connections
- Nếu tất cả connections đang được sử dụng, mỗi goroutine timeout 2s
- Monitor chỉ check mỗi 2s, có thể không kịp tạo connections mới
- Cascade failure: Pool exhausted → Timeouts → Semaphore đầy → Main loop block

### 4. **Non-blocking Semaphore Issue**

```go
// Current code:
semaphore <- struct{}{}  // BLOCKING
go func() {
    defer func() { <-semaphore }()
    // ...
}()
```

**Vấn đề:**
- Nếu semaphore đầy, main loop bị block
- Không thể xử lý transactions mới
- Hệ thống bị đứng

## Giải pháp

### 1. **Non-blocking Semaphore với Timeout**

```go
// Thay vì:
semaphore <- struct{}{}  // BLOCKING

// Dùng:
select {
case semaphore <- struct{}{}:
    // Acquired semaphore, spawn goroutine
    go func() {
        defer func() { <-semaphore }()
        // Gửi transaction
    }()
case <-time.After(100 * time.Millisecond):
    // Semaphore đầy, log warning và skip batch này
    logger.Warn("Semaphore exhausted, skipping batch (will retry next tick)")
    continue
}
```

### 2. **Tách Rate Limiting khỏi Main Loop**

```go
// Thay vì rate limit trong main loop:
for batchStart := 0; batchStart < totalTxs; batchStart += maxTransactionsPerBatch {
    <-rateLimiter.C  // BLOCKING
    // ...
}

// Dùng channel để queue batches:
batchChan := make(chan []types.Transaction, 1000)
rateLimiter := time.NewTicker(50 * time.Millisecond)

// Producer: Main loop
go func() {
    for {
        <-ticker.C
        // Process transactions và gửi vào batchChan
        select {
        case batchChan <- batchTxs:
        default:
            logger.Warn("Batch channel full, dropping batch")
        }
    }
}()

// Consumer: Rate-limited sender
go func() {
    for batch := range batchChan {
        <-rateLimiter.C  // Rate limit ở đây, không block main loop
        // Send batch với non-blocking semaphore
    }
}()
```

### 3. **Tăng Connection Pool và Monitor Frequency**

```go
// Tăng pool size:
poolSize := 200  // Từ 100 lên 200

// Tăng monitor frequency:
ticker := time.NewTicker(500 * time.Millisecond)  // Từ 2s xuống 500ms
```

### 4. **Giảm Timeout và Tăng Retry**

```go
// Giảm pool timeout:
case <-time.After(1 * time.Second):  // Từ 2s xuống 1s

// Tăng retry trong goroutine:
maxRetries := 10  // Từ 5 lên 10
```

## Implementation Plan

1. ✅ Thay đổi semaphore thành non-blocking với timeout
2. ✅ Tách rate limiting khỏi main loop
3. ✅ Tăng connection pool size
4. ✅ Tăng monitor frequency
5. ✅ Giảm timeout và tăng retry

