# Phân tích: Race Condition và Connection Issues trên Localhost

## Vấn đề

Người dùng báo: **Gửi 1-2 giao dịch đầu tiên thì bị đứng**, mặc dù:
- Localhost nên không cần retry
- Connection pool đủ lớn (200)
- Không có timeout errors

## Phân tích

### 1. **Connection được trả về pool TRƯỚC KHI Rust đọc xong**

```go
// writeData() gửi data
err := writeData(conn, transactionPayload)

// Nếu thành công, TRẢ CONNECTION VỀ POOL NGAY LẬP TỨC
select {
case c.conns <- conn:  // ⚠️ Connection được reuse ngay
    // ...
}
```

**Vấn đề:**
- Go client gửi data và trả connection về pool ngay
- Nhưng Rust RPC server có thể chưa đọc xong data
- Connection được reuse bởi goroutine khác
- Data bị mix hoặc connection bị đóng sớm

### 2. **Rust RPC Server đọc data nhưng connection đã bị đóng**

```rust
// Rust RPC server
let read_len_result = tokio::time::timeout(
    Duration::from_secs(5),
    stream.read_exact(&mut len_buf)  // ⚠️ Đọc length prefix
).await;

// Nếu connection bị đóng sớm, read_exact sẽ fail
```

**Vấn đề:**
- Go client đóng connection hoặc trả về pool
- Rust đang đọc nhưng connection đã bị đóng
- Timeout hoặc connection reset

### 3. **Race Condition: Multiple goroutines dùng cùng connection**

```go
// Goroutine 1: Gửi transaction A
conn := <-c.conns
writeData(conn, txA)
c.conns <- conn  // Trả về pool

// Goroutine 2: Lấy cùng connection ngay lập tức
conn := <-c.conns  // ⚠️ Có thể là connection vừa được trả về
writeData(conn, txB)  // ⚠️ Rust có thể đang đọc txA
```

**Vấn đề:**
- Connection được reuse quá nhanh
- Rust chưa đọc xong data từ goroutine 1
- Goroutine 2 gửi data mới → data bị mix

### 4. **writeData không đợi Rust đọc xong**

```go
func writeData(conn net.Conn, payload []byte) error {
    // Gửi data
    conn.Write(fullMessage)
    
    // ⚠️ KHÔNG ĐỢI Rust đọc xong, return ngay
    return nil
}
```

**Vấn đề:**
- Go client gửi xong và return ngay
- Không đợi Rust đọc xong
- Connection được trả về pool → có thể bị reuse

## Giải pháp

### 1. **Đợi Rust đọc xong trước khi trả connection về pool**

```go
func writeData(conn net.Conn, payload []byte) error {
    // Gửi data
    conn.Write(fullMessage)
    
    // ⚠️ QUAN TRỌNG: Đợi Rust đọc xong (hoặc timeout)
    // Rust sẽ đóng connection hoặc gửi response khi đọc xong
    // Đọc response (hoặc đợi connection close)
    
    // Set read deadline ngắn để đợi response
    conn.SetReadDeadline(time.Now().Add(100 * time.Millisecond))
    buf := make([]byte, 1)
    _, err := conn.Read(buf)
    if err != nil {
        // Không có response hoặc connection đóng = Rust đã đọc xong
        // OK, có thể trả connection về pool
    }
    conn.SetReadDeadline(time.Time{})
    
    return nil
}
```

### 2. **Sử dụng connection per request (không reuse ngay)**

```go
// Thay vì trả connection về pool ngay:
// c.conns <- conn

// Đợi một chút để Rust đọc xong
time.Sleep(10 * time.Millisecond)  // Đợi Rust đọc xong

// Sau đó mới trả về pool
select {
case c.conns <- conn:
default:
    conn.Close()
}
```

### 3. **Tăng timeout cho Rust đọc data**

```rust
// Rust RPC server
let read_len_result = tokio::time::timeout(
    Duration::from_secs(10),  // Tăng từ 5s lên 10s
    stream.read_exact(&mut len_buf)
).await;
```

### 4. **Logging để debug**

```go
// Log khi gửi
fmt.Printf("📤 [TX CLIENT] Sending transaction, waiting for Rust to read...\n")
err := writeData(conn, transactionPayload)
fmt.Printf("✅ [TX CLIENT] Sent transaction, Rust should have read it\n")

// Đợi một chút trước khi trả connection
time.Sleep(50 * time.Millisecond)  // Đợi Rust đọc xong
fmt.Printf("🔄 [TX CLIENT] Returning connection to pool\n")
```

## Implementation

### Option 1: Đợi response từ Rust (Recommended)

Rust RPC server nên gửi response sau khi đọc xong:
```rust
// Sau khi đọc xong transaction data
info!("📥 [TX FLOW] Received transaction, processing...");
// Process transaction
// Gửi response
let response = b"OK";
stream.write_all(response).await?;
```

Go client đợi response:
```go
// Sau khi gửi data
conn.Write(fullMessage)

// Đợi response từ Rust
conn.SetReadDeadline(time.Now().Add(1 * time.Second))
buf := make([]byte, 2)  // "OK"
_, err := conn.Read(buf)
if err == nil && string(buf) == "OK" {
    // Rust đã đọc xong, có thể trả connection về pool
}
```

### Option 2: Delay trước khi trả connection (Simple)

```go
// Sau khi gửi thành công
err := writeData(conn, transactionPayload)
if err == nil {
    // Đợi Rust đọc xong (localhost nhanh, 50ms đủ)
    time.Sleep(50 * time.Millisecond)
    
    // Trả connection về pool
    select {
    case c.conns <- conn:
    default:
        conn.Close()
    }
}
```

## Recommended Fix

**Option 2 (Simple)** vì:
- Không cần thay đổi Rust code
- Localhost nhanh, 50ms delay không ảnh hưởng performance
- Đơn giản và dễ debug

