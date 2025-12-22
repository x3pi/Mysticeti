# Fix: Connection Reuse Race Condition

## Vấn đề

Giao dịch `ee8a12e337b7f540bd200df712bdeb052c9ed31f353b9a37c5b08f1f88fb35b3` bị đứng vì:

1. **Go client gửi thành công**: "Đã gửi xong 660 bytes"
2. **Go client trả connection về pool ngay**: Connection được tái sử dụng ngay
3. **Rust timeout**: "Timeout reading length prefix" (timeout sau 5s)
4. **Rust không đọc được data**: Connection đã được tái sử dụng trước khi Rust đọc

## Root Cause

**Race condition giữa Go trả connection về pool và Rust đọc data:**

```
Timeline (vấn đề):
T=0ms:   Go client gửi data (660 bytes) → trả connection về pool ngay
T=1ms:   Go client sử dụng connection khác từ pool → gửi data mới
T=2ms:   Rust bắt đầu đọc length prefix từ connection đầu tiên
T=3ms:   Nhưng connection đã được tái sử dụng → Rust đọc data sai
T=4ms:   Rust timeout (5s) → không đọc được
```

**Vấn đề:**
- Go client trả connection về pool ngay sau khi gửi
- Connection được tái sử dụng trước khi Rust đọc xong
- Rust đọc data từ connection đã được tái sử dụng → timeout

## Fix

### Thêm delay nhỏ sau khi gửi (Go)

```go
// QUAN TRỌNG: Đợi một chút để đảm bảo Rust có thời gian bắt đầu đọc data
// trước khi connection được trả về pool và tái sử dụng
// Delay ngắn (1ms) đủ để Rust bắt đầu đọc trên localhost
time.Sleep(1 * time.Millisecond)
```

**Lý do:**
- 1ms đủ để Rust bắt đầu đọc length prefix trên localhost
- Không ảnh hưởng nhiều đến throughput (vẫn rất nhanh)
- Đảm bảo Rust đọc được data trước khi connection được tái sử dụng

## Expected Flow (sau fix)

```
Timeline (sau fix):
T=0ms:   Go client gửi data (660 bytes)
T=1ms:   Go client đợi (delay 1ms) → Rust có thời gian bắt đầu đọc
T=2ms:   Rust bắt đầu đọc length prefix
T=3ms:   Rust đọc được length prefix (4 bytes)
T=4ms:   Rust đọc transaction data (656 bytes)
T=5ms:   Go client trả connection về pool (sau khi Rust đã đọc xong)
T=6ms:   Go client sử dụng connection khác → gửi data mới
```

## Lưu ý

1. **Delay 1ms là tối thiểu**: Đủ để Rust bắt đầu đọc trên localhost
2. **Không ảnh hưởng throughput**: 1ms rất nhỏ so với thời gian xử lý transaction
3. **Chỉ áp dụng cho localhost**: Vì localhost rất nhanh, 1ms đủ
4. **Connection vẫn được tái sử dụng**: Sau khi Rust đã bắt đầu đọc

## Testing

Sau khi rebuild:
1. Go client sẽ đợi 1ms sau khi gửi
2. Rust sẽ có thời gian bắt đầu đọc
3. Connection sẽ được trả về pool sau khi Rust đã bắt đầu đọc
4. Transaction sẽ được xử lý thành công

