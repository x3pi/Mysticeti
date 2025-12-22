# Fix: Connection Timing Issue

## Vấn đề

Giao dịch `2240d0e5e80a5484a0aa161838e98bbb33e4499b83331649ee05d02a6c131824` bị đứng vì:

1. **Go client gửi thành công**: "Đã gửi xong 660 bytes"
2. **Rust không đọc được**: "Timeout reading length prefix" (timeout sau 5s)
3. **Go client không nhận được response**: "Không nhận được response từ Rust: EOF"

## Root Cause

**Race condition giữa Go gửi và Rust đọc:**

```
Timeline:
T=0ms:   Go client gửi data (660 bytes)
T=1ms:   Go client đọc response ngay lập tức
T=2ms:   Rust bắt đầu đọc length prefix
T=3ms:   Go client timeout (5s) hoặc EOF → đóng connection
T=4ms:   Rust timeout (5s) → không đọc được
```

**Vấn đề:**
- Go client gửi data và ngay lập tức đọc response
- Rust chưa kịp đọc data
- Connection bị đóng hoặc timeout

## Fix

### 1. **Thêm delay sau khi gửi data (Go)**

```go
// Đợi một chút để đảm bảo Rust có thời gian đọc data trước khi Go đọc response
time.Sleep(10 * time.Millisecond) // Đợi 10ms để Rust có thời gian bắt đầu đọc
```

### 2. **Improved error logging (Rust)**

```rust
error!("❌ [TX FLOW] Failed to read length prefix from {:?}: {} (connection may be closed by client)", peer_addr, e);
error!("❌ [TX FLOW] Timeout reading length prefix from {:?} (timeout after 5s)", peer_addr);
```

## Expected Flow (sau fix)

```
Timeline:
T=0ms:   Go client gửi data (660 bytes)
T=10ms:  Go client đợi (delay) → Rust có thời gian bắt đầu đọc
T=11ms:  Rust bắt đầu đọc length prefix
T=12ms:  Rust đọc được length prefix (4 bytes)
T=13ms:  Rust đọc transaction data (656 bytes)
T=14ms:  Rust xử lý và gửi response
T=15ms:  Go client đọc response thành công
```

## Testing

Sau khi rebuild:
1. Go client sẽ đợi 10ms sau khi gửi
2. Rust sẽ có thời gian đọc data
3. Go client sẽ nhận được response
4. Transaction sẽ được xử lý thành công

## Note

Delay 10ms là tạm thời để fix race condition. Trong production, nên dùng response-based design (đã implement) để đảm bảo Rust đã đọc xong trước khi Go đọc response.

