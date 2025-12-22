# Debug Connection Issues

## Vấn đề: Transaction bị đứng - Go gửi nhưng Rust không nhận

### Triệu chứng
- Go Sub log: "Successfully sent batch"
- Rust log: **KHÔNG CÓ** "connection accepted" hoặc "Received length-prefixed"
- Transaction bị đứng, không được xử lý

### Nguyên nhân có thể

1. **Connection không được tạo**
   - Go client không tạo connection thành công
   - Connection pool exhausted
   - Network issue

2. **Connection bị đóng trước khi Rust accept**
   - Go client gửi xong và đóng connection ngay
   - Rust chưa kịp accept connection

3. **Protocol mismatch**
   - Go gửi length-prefixed nhưng Rust expect HTTP
   - Hoặc ngược lại

### Debugging Steps

#### 1. Kiểm tra Rust RPC Server
```bash
# Check if RPC server is listening
netstat -tlnp | grep 10100
# or
ss -tlnp | grep 10100

# Check Rust logs
tail -f Mysticeti/metanode/logs/latest/node_0.log | grep -E "RPC server|connection accepted|Waiting for new connection"
```

#### 2. Kiểm tra Go Client
```bash
# Check Go logs
tail -f mtn-simple-2025/cmd/simple_chain/sample/App.log | grep -E "TX CLIENT|writeData|connection"
```

#### 3. Logs đã thêm

**Go Client:**
- `📤 [TX CLIENT] Đang gửi transaction`: Trước khi gửi
- `📤 [TX CLIENT] writeData`: Chi tiết writeData
- `✅ [TX CLIENT] writeData: Đã gửi xong`: Sau khi gửi xong
- `✅ [TX CLIENT] Đã gửi transaction thành công`: Sau khi writeData thành công

**Rust Server:**
- `🔌 [TX FLOW] Waiting for new connection`: Đang đợi connection
- `🔌 [TX FLOW] New connection accepted`: Đã accept connection
- `📥 [TX FLOW] Spawned handler`: Đã spawn handler
- `📥 [TX FLOW] Waiting to read length prefix`: Đang đợi đọc length prefix

### Expected Flow

```
Go Client:
1. 📤 [TX CLIENT] Đang gửi transaction
2. 📤 [TX CLIENT] writeData: payload_size=656, full_message_size=660
3. 📤 [TX CLIENT] writeData: Đã gửi 660/660 bytes
4. ✅ [TX CLIENT] writeData: Đã gửi xong 660 bytes
5. ✅ [TX CLIENT] Đã gửi transaction thành công

Rust Server:
1. 🔌 [TX FLOW] Waiting for new connection
2. 🔌 [TX FLOW] New connection accepted from 127.0.0.1:xxxxx
3. 📥 [TX FLOW] Spawned handler for connection
4. 📥 [TX FLOW] Waiting to read length prefix (4 bytes)
5. 📥 [TX FLOW] Read length prefix: 656 bytes
6. 📥 [TX FLOW] Reading 656 bytes of transaction data
7. 📥 [TX FLOW] Received length-prefixed transaction data: size=656 bytes
```

### Nếu Go gửi nhưng Rust không nhận

**Check 1: Connection có được tạo không?**
- Xem Go logs: Có "Đang gửi transaction" không?
- Xem Go logs: Có "writeData" không?

**Check 2: Rust có accept connection không?**
- Xem Rust logs: Có "New connection accepted" không?
- Nếu không có → Connection không đến Rust

**Check 3: Protocol có đúng không?**
- Go gửi length-prefixed (4 bytes length + data)
- Rust expect length-prefixed hoặc HTTP
- Check Rust logs: "Received length-prefixed" hay "Received HTTP POST"?

### Fixes Applied

1. ✅ Thêm logging chi tiết trong Go `writeData`
2. ✅ Thêm logging chi tiết trong Go `SendTransaction`
3. ✅ Thêm logging chi tiết trong Rust `accept` loop
4. ✅ Improved error handling trong Rust `accept`

### Next Steps

1. Rebuild Go và Rust
2. Restart hệ thống
3. Gửi transaction mới
4. Kiểm tra logs để xem transaction bị đứng ở đâu

