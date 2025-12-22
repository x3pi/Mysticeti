# Localhost Fire-and-Forget Optimization

## Tổng quan

Tối ưu hóa cho localhost: loại bỏ response, giữ connection mở, và gửi nhiều transactions liên tục.

## Thay đổi chính

### 1. **Loại bỏ Response (Go Client)**

**Trước:**
```go
// Đợi response từ Rust (5s timeout)
conn.SetReadDeadline(time.Now().Add(5 * time.Second))
successByte := make([]byte, 1)
_, err = conn.Read(successByte)
// ... đọc message length và message
```

**Sau:**
```go
// LOCALHOST OPTIMIZATION: Không đọc response từ Rust
// Vì là localhost, connection rất nhanh và đáng tin cậy
// Giữ connection mở để gửi nhiều transactions liên tục (fire-and-forget)
// Connection sẽ được trả về pool ngay sau khi gửi xong để tái sử dụng
```

### 2. **Loại bỏ Response (Rust Server)**

**Trước:**
```rust
if is_length_prefixed {
    // Send binary response for length-prefixed protocol
    if let Err(e) = Self::send_binary_response(stream, true, "OK").await {
        error!("❌ [TX FLOW] Failed to send binary response...");
    }
}
```

**Sau:**
```rust
if is_length_prefixed {
    // LOCALHOST OPTIMIZATION: Không gửi response cho localhost
    // Go client không đọc response, giữ connection mở để gửi nhiều transactions liên tục
    // Chỉ log để debug, không gửi response để tối ưu throughput
    // Connection sẽ được giữ mở và tái sử dụng cho batch tiếp theo
}
```

### 3. **Tăng Pool Size và Throughput**

**Go Client:**
- Pool size: `200` → `500` connections
- Connection timeout: `1s` → `100ms` (localhost nhanh)

**Go Sub Node:**
- Max concurrent sends: `100` → `200` goroutines
- Batch size: `20` → `50` transactions per batch
- Rate limiting: `50ms` → `10ms` (tối đa 100 batches/giây)

### 4. **Loại bỏ Delay**

**Trước:**
```go
time.Sleep(10 * time.Millisecond) // Đợi 10ms để Rust có thời gian bắt đầu đọc
```

**Sau:**
```go
// LOCALHOST OPTIMIZATION: Không cần delay vì localhost rất nhanh
// Connection sẽ được giữ mở để gửi nhiều transactions liên tục
```

## Lợi ích

1. **Throughput cao hơn**: Không cần đợi response → gửi nhanh hơn
2. **Connection reuse**: Connection được giữ mở và tái sử dụng ngay
3. **Ít overhead**: Không cần serialize/deserialize response
4. **Đơn giản hơn**: Fire-and-forget pattern dễ implement và maintain

## Expected Flow

```
Timeline (localhost):
T=0ms:   Go client gửi batch 1 (50 transactions)
T=1ms:   Go client trả connection về pool ngay
T=2ms:   Go client gửi batch 2 (50 transactions) với connection khác
T=3ms:   Go client trả connection về pool ngay
T=4ms:   Rust đang xử lý batch 1 (async)
T=5ms:   Rust đang xử lý batch 2 (async)
...
```

## Lưu ý

1. **Chỉ dùng cho localhost**: Vì không có response, không biết transaction có được xử lý thành công hay không
2. **Cần monitoring**: Theo dõi logs để đảm bảo Rust nhận được transactions
3. **Connection health**: Background monitor vẫn kiểm tra connection health
4. **Error handling**: Nếu gửi thất bại, transaction sẽ được retry trong tick tiếp theo

## Testing

Sau khi rebuild:
1. Go client sẽ gửi batch và trả connection về pool ngay
2. Rust sẽ xử lý transactions async (không gửi response)
3. Throughput sẽ cao hơn đáng kể (không bị block bởi response)
4. Connection pool sẽ được tái sử dụng hiệu quả hơn

