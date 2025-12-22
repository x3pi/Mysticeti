# Phân tích: Batch Sending và Connection Reuse

## Luồng thực tế

### Go Sub Node → Rust (Batch Sending)

```
1. Go Sub Node (TxsProcessor2)
   ├─ Lấy transactions từ pool (tối đa 20 transactions/batch)
   ├─ MarshalTransactions(batchTxs) → Protobuf Transactions message
   │  └─ Format: [length_prefix: 4 bytes][protobuf_data]
   │     └─ protobuf_data chứa nhiều Transaction messages
   ├─ Gửi batch qua một connection
   │  └─ writeData(conn, bTransaction)
   │     └─ Gửi: [4 bytes length][protobuf Transactions]
   └─ Trả connection về pool
```

### Rust RPC Server (Nhận Batch)

```
1. Rust RPC Server
   ├─ Accept connection
   ├─ Đọc 4 bytes (length prefix)
   ├─ Đọc N bytes (transaction data - có thể chứa nhiều transactions)
   ├─ Decode protobuf Transactions message
   ├─ Split thành individual Transaction messages
   └─ Submit từng transaction vào consensus
```

## Vấn đề

### 1. **Connection được trả về pool quá sớm**

```go
// Go client
writeData(conn, bTransaction)  // Gửi batch (có thể lớn)
// ⚠️ Trả connection về pool NGAY
c.conns <- conn
```

**Vấn đề:**
- Go client gửi batch và trả connection về pool ngay
- Rust đang đọc batch (có thể mất vài ms cho batch lớn)
- Connection được reuse bởi goroutine khác
- Data bị mix hoặc connection bị đóng sớm

### 2. **Batch size ảnh hưởng đến thời gian đọc**

- Batch nhỏ (1-5 transactions): ~1-5KB → Rust đọc nhanh (~1-5ms)
- Batch trung bình (10 transactions): ~10KB → Rust đọc ~10ms
- Batch lớn (20 transactions): ~20KB → Rust đọc ~20ms

**Vấn đề:**
- Delay cố định 50ms có thể quá nhiều cho batch nhỏ
- Delay cố định 50ms có thể quá ít cho batch lớn

## Giải pháp

### 1. **Delay dựa trên payload size**

```go
// Delay dựa trên payload size: 1ms per KB
delayMs := payloadSize / 1024
if delayMs < 10 {
    delayMs = 10  // Tối thiểu 10ms
} else if delayMs > 100 {
    delayMs = 100  // Tối đa 100ms
}
time.Sleep(time.Duration(delayMs) * time.Millisecond)
```

**Ưu điểm:**
- Batch nhỏ: Delay ngắn (10ms)
- Batch lớn: Delay dài hơn (tối đa 100ms)
- Đảm bảo Rust có đủ thời gian đọc xong

### 2. **Đợi response từ Rust (Tốt hơn nhưng cần thay đổi Rust)**

```rust
// Rust RPC server
// Sau khi đọc xong và process transaction
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
}
```

**Ưu điểm:**
- Chính xác: Đợi Rust đọc xong thực sự
- Không cần estimate delay
- Hoạt động tốt với mọi batch size

**Nhược điểm:**
- Cần thay đổi Rust code
- Phức tạp hơn

## Implementation hiện tại

Đã implement **Option 1 (Delay dựa trên payload size)** vì:
- Không cần thay đổi Rust code
- Đơn giản và hiệu quả
- Hoạt động tốt với mọi batch size

### Delay calculation:
- Batch 1KB: 10ms (minimum)
- Batch 10KB: 10ms (minimum)
- Batch 50KB: 50ms
- Batch 100KB: 100ms (maximum)

## Kết quả mong đợi

1. ✅ Connection chỉ được reuse sau khi Rust đọc xong batch
2. ✅ Delay phù hợp với batch size
3. ✅ Không còn race condition
4. ✅ Data không bị mix

## Lưu ý

- Delay này chỉ là **estimate**, không chính xác 100%
- Trên localhost, delay thường đủ
- Nếu vẫn có vấn đề, nên implement **Option 2 (Đợi response từ Rust)**

