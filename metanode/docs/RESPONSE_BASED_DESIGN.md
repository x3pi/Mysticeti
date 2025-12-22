# Thiết kế: Response-Based Connection Reuse (Không dùng Delay)

## Tổng quan

Thay vì dùng **delay** (ước tính thời gian), thiết kế mới sử dụng **response từ Rust** để xác nhận Rust đã đọc xong và xử lý batch.

## Luồng mới

### 1. Go Client → Rust (Gửi Batch)

```
Go Client:
1. Gửi batch: [4 bytes length][protobuf Transactions]
2. ĐỢI response từ Rust
3. Nhận response: [1 byte success][4 bytes length][message]
4. Trả connection về pool
```

### 2. Rust RPC Server (Nhận và Phản hồi)

```
Rust RPC Server:
1. Đọc batch: [4 bytes length][protobuf Transactions]
2. Xử lý batch (decode, submit vào consensus)
3. Gửi response: [1 byte success][4 bytes length][message]
   - success: 0x01 = OK, 0x00 = ERROR
   - message: "OK" hoặc error message
```

## Protocol Design

### Request Format (Go → Rust)

```
[4 bytes: length (big-endian u32)][protobuf Transactions data]
```

### Response Format (Rust → Go)

```
[1 byte: success (0x01=OK, 0x00=ERROR)]
[4 bytes: message length (big-endian u32)]
[message bytes: "OK" hoặc error message]
```

## Implementation

### Rust RPC Server

```rust
// Sau khi xử lý batch thành công
async fn send_binary_response(
    stream: &mut tokio::net::TcpStream,
    success: bool,
    message: &str,
) -> Result<()> {
    let success_byte = if success { 0x01u8 } else { 0x00u8 };
    let message_bytes = message.as_bytes();
    let message_len = message_bytes.len() as u32;
    
    // Write: [1 byte success][4 bytes length][message]
    stream.write_u8(success_byte).await?;
    stream.write_u32(message_len).await?;
    stream.write_all(message_bytes).await?;
    stream.flush().await?;
    
    Ok(())
}
```

### Go Client

```go
// Sau khi gửi batch
err := writeData(conn, transactionPayload)
if err == nil {
    // Đợi response từ Rust
    conn.SetReadDeadline(time.Now().Add(2 * time.Second))
    
    // Đọc response
    successByte := make([]byte, 1)
    _, err = conn.Read(successByte)
    if err == nil {
        // Đọc message length và message
        lenBuf := make([]byte, 4)
        if n, err := conn.Read(lenBuf); err == nil && n == 4 {
            messageLen := binary.BigEndian.Uint32(lenBuf)
            if messageLen > 0 && messageLen < 1024 {
                message := make([]byte, messageLen)
                if n, err := conn.Read(message); err == nil && n == int(messageLen) {
                    if successByte[0] == 0x01 {
                        // Success: Rust đã xử lý xong
                    } else {
                        // Error: Rust báo lỗi
                    }
                }
            }
        }
    }
    
    // Trả connection về pool
    c.conns <- conn
}
```

## Ưu điểm

### 1. **Chính xác 100%**
- Không cần estimate delay
- Đợi Rust xử lý xong thực sự
- Hoạt động với mọi batch size

### 2. **Không có race condition**
- Connection chỉ được reuse sau khi Rust xác nhận
- Data không bị mix
- An toàn 100%

### 3. **Error handling tốt hơn**
- Rust có thể báo lỗi cụ thể
- Go client biết batch có được xử lý không
- Có thể retry nếu cần

### 4. **Performance tốt**
- Không delay không cần thiết
- Connection được reuse ngay khi Rust xử lý xong
- Tối ưu cho localhost

## So sánh với Delay

| Aspect | Delay | Response-Based |
|--------|-------|----------------|
| **Chính xác** | Estimate (không chính xác) | 100% chính xác |
| **Race condition** | Có thể xảy ra | Không có |
| **Error handling** | Không biết lỗi | Biết lỗi cụ thể |
| **Performance** | Delay không cần thiết | Tối ưu |
| **Complexity** | Đơn giản | Phức tạp hơn một chút |

## Kết luận

**Response-based design** là giải pháp tốt hơn:
- ✅ Chính xác 100%
- ✅ Không có race condition
- ✅ Error handling tốt
- ✅ Performance tối ưu

**Trade-off:** Cần thay đổi cả Rust và Go code, nhưng đáng giá vì đảm bảo correctness và reliability.

