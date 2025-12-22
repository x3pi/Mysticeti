# Response Protocol: Go ↔ Rust Communication

## Protocol Design

### Request (Go → Rust)

```
Format: [4 bytes: length][protobuf Transactions data]

Example:
[0x00 0x00 0x01 0x23] [protobuf data 291 bytes]
  ↑ length (291 bytes)    ↑ Transactions message
```

### Response (Rust → Go)

```
Format: [1 byte: success][4 bytes: message length][message bytes]

Success Response:
[0x01] [0x00 0x00 0x00 0x02] ["OK"]
  ↑ OK    ↑ length (2 bytes)   ↑ "OK"

Error Response:
[0x00] [0x00 0x00 0x00 0x15] ["Transaction submission failed"]
  ↑ ERROR ↑ length (21 bytes)  ↑ error message
```

## Implementation Details

### Rust RPC Server

**Function: `send_binary_response`**
```rust
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
    stream.write_u32(message_len).await?; // big-endian
    stream.write_all(message_bytes).await?;
    stream.flush().await?;
    
    Ok(())
}
```

**Usage:**
- Success: `send_binary_response(stream, true, "OK").await?`
- Error: `send_binary_response(stream, false, "Error message").await?`

### Go Client

**Function: `SendTransaction` (after sending batch)**
```go
// Đợi response từ Rust
conn.SetReadDeadline(time.Now().Add(2 * time.Second))
defer conn.SetReadDeadline(time.Time{})

// Đọc response
successByte := make([]byte, 1)
_, err = conn.Read(successByte)
if err == nil {
    // Đọc message length
    lenBuf := make([]byte, 4)
    if n, err := conn.Read(lenBuf); err == nil && n == 4 {
        messageLen := binary.BigEndian.Uint32(lenBuf)
        if messageLen > 0 && messageLen < 1024 {
            // Đọc message
            message := make([]byte, messageLen)
            if n, err := conn.Read(message); err == nil && n == int(messageLen) {
                if successByte[0] == 0x01 {
                    // Success
                } else {
                    // Error
                }
            }
        }
    }
}
```

## Flow Diagram

```
┌─────────────────┐
│   Go Client     │
└────────┬────────┘
         │ 1. Send batch
         │    [4 bytes length][protobuf data]
         ▼
┌─────────────────┐
│  Rust RPC       │
│  Server         │
│                 │
│  2. Read batch  │
│  3. Process     │
│  4. Send resp   │
└────────┬────────┘
         │ 5. Response
         │    [1 byte][4 bytes length][message]
         ▼
┌─────────────────┐
│   Go Client     │
│                 │
│  6. Read resp   │
│  7. Return conn │
│     to pool     │
└─────────────────┘
```

## Benefits

1. **100% Accurate**: Đợi Rust xử lý xong thực sự
2. **No Race Condition**: Connection chỉ được reuse sau khi Rust xác nhận
3. **Error Handling**: Rust có thể báo lỗi cụ thể
4. **Optimal Performance**: Không delay không cần thiết

## Error Cases

### 1. Timeout (2 seconds)
- Go client không nhận được response
- Log warning nhưng vẫn trả connection về pool
- Rust sẽ đọc xong sau (connection vẫn open)

### 2. Connection Closed
- Rust đóng connection trước khi gửi response
- Go client detect và tạo connection mới
- Background monitor sẽ maintain pool

### 3. Invalid Response
- Response format không đúng
- Go client log warning và trả connection về pool
- Không ảnh hưởng đến batch đã gửi

