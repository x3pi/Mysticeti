# Giải thích: Delay trong Connection Reuse

## Delay là gì?

**Delay** = Thời gian chờ (sleep) trước khi trả connection về pool để tái sử dụng.

## Tại sao cần delay?

### Vấn đề không có delay:

```
Timeline không có delay:
T=0ms:   Go client gửi batch A qua connection X
T=1ms:   Go client trả connection X về pool NGAY LẬP TỨC
T=2ms:   Go client lấy connection X từ pool (reuse)
T=2ms:   Go client gửi batch B qua connection X
T=3ms:   Rust đang đọc batch A nhưng nhận batch B → DATA BỊ MIX ❌
```

### Với delay:

```
Timeline có delay:
T=0ms:   Go client gửi batch A qua connection X
T=1ms:   Go client ĐỢI (delay) 50ms
T=51ms:  Rust đã đọc xong batch A ✅
T=51ms:  Go client trả connection X về pool
T=52ms:  Go client lấy connection X từ pool (reuse)
T=52ms:  Go client gửi batch B qua connection X
T=53ms:  Rust đọc batch B → OK ✅
```

## Delay hoạt động như thế nào?

### Code hiện tại:

```go
// Sau khi gửi batch thành công
err := writeData(conn, transactionPayload)
if err == nil {
    // Delay dựa trên payload size
    delayMs := payloadSize / 1024  // 1ms per KB
    if delayMs < 10 {
        delayMs = 10  // Tối thiểu 10ms
    } else if delayMs > 100 {
        delayMs = 100  // Tối đa 100ms
    }
    
    // Đợi Rust đọc xong
    time.Sleep(time.Duration(delayMs) * time.Millisecond)
    
    // Sau đó mới trả connection về pool
    c.conns <- conn
}
```

### Ví dụ:

**Batch nhỏ (5KB - 5 transactions):**
- `delayMs = 5KB / 1024 = 4ms`
- `delayMs = 10ms` (minimum)
- Đợi 10ms trước khi trả connection về pool

**Batch trung bình (20KB - 20 transactions):**
- `delayMs = 20KB / 1024 = 19ms`
- Đợi 19ms trước khi trả connection về pool

**Batch lớn (100KB - nhiều batches):**
- `delayMs = 100KB / 1024 = 97ms`
- `delayMs = 100ms` (maximum)
- Đợi 100ms trước khi trả connection về pool

## Tại sao delay dựa trên payload size?

### Batch lớn hơn = Rust cần nhiều thời gian hơn để đọc

```
Batch 5KB:   Rust đọc ~5ms
Batch 20KB:  Rust đọc ~20ms
Batch 100KB: Rust đọc ~100ms
```

**Delay phù hợp với batch size:**
- Batch nhỏ → Delay ngắn (10ms)
- Batch lớn → Delay dài hơn (tối đa 100ms)

## Có cách nào tốt hơn không?

### Option 1: Delay (Hiện tại) ✅

**Ưu điểm:**
- Đơn giản
- Không cần thay đổi Rust code
- Hoạt động tốt trên localhost

**Nhược điểm:**
- Chỉ là estimate, không chính xác 100%
- Có thể delay quá nhiều hoặc quá ít

### Option 2: Đợi response từ Rust (Tốt hơn) ⭐

**Cách hoạt động:**
```rust
// Rust RPC server
// Sau khi đọc xong batch
let response = b"OK";
stream.write_all(response).await?;
```

```go
// Go client
// Sau khi gửi batch
conn.Write(fullMessage)

// Đợi response từ Rust
conn.SetReadDeadline(time.Now().Add(1 * time.Second))
buf := make([]byte, 2)
_, err := conn.Read(buf)
if err == nil && string(buf) == "OK" {
    // Rust đã đọc xong, có thể trả connection về pool
    c.conns <- conn
}
```

**Ưu điểm:**
- Chính xác 100%: Đợi Rust đọc xong thực sự
- Không cần estimate
- Hoạt động tốt với mọi batch size

**Nhược điểm:**
- Cần thay đổi Rust code
- Phức tạp hơn một chút

## Kết luận

**Delay hiện tại** là giải pháp đơn giản và hiệu quả cho localhost:
- Đảm bảo Rust có đủ thời gian đọc xong batch
- Delay phù hợp với batch size
- Không cần thay đổi Rust code

**Nếu vẫn có vấn đề**, nên implement **Option 2 (Đợi response từ Rust)** để chính xác hơn.

