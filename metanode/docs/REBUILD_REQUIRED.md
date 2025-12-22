# Rebuild Required

## Vấn đề

Giao dịch `73c8b16d2b636f84a9b2c2031c8cc8b3bb80d8a8e50aeddb8ab0117dc0fea0ed` bị đứng vì:

1. **Go binary cũ**: Binary được build Dec 13, không có logs mới
2. **Rust không nhận được**: Rust chỉ có logs cũ (01:57:33), không có logs mới cho transactions (04:48:20-04:48:25)
3. **Protocol mismatch**: Rust nhận HTTP POST nhỏ thay vì length-prefixed binary

## Giải pháp

### 1. Rebuild Go Binary

```bash
cd /home/abc/chain-new/mtn-simple-2025
go build ./pkg/txsender
```

Hoặc rebuild toàn bộ:
```bash
cd /home/abc/chain-new/mtn-simple-2025/cmd/simple_chain
go build .
```

### 2. Restart Go Sub Node

```bash
# Stop Go Sub Node
tmux kill-session -t go-sub

# Start Go Sub Node với binary mới
cd /home/abc/chain-new/mtn-simple-2025/cmd/simple_chain
tmux new-session -d -s go-sub -c "$(pwd)" \
  bash -c "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/simple/data/data/xapian_node' && go run . -config=config-sub-write.json"
```

### 3. Verify Logs

Sau khi restart, kiểm tra logs:
- Go Sub: `tail -f mtn-simple-2025/cmd/simple_chain/sample/App.log | grep "TX CLIENT"`
- Rust: `tail -f Mysticeti/metanode/logs/latest/node_0.log | grep "TX FLOW"`

### Expected Logs (sau rebuild)

**Go Client:**
```
📤 [TX CLIENT] Đang gửi transaction: size=656 bytes, hash_preview=...
📤 [TX CLIENT] writeData: payload_size=656, full_message_size=660
✅ [TX CLIENT] writeData: Đã gửi xong 660 bytes
✅ [TX CLIENT] Đã gửi transaction thành công
```

**Rust Server:**
```
🔌 [TX FLOW] Waiting for new connection
🔌 [TX FLOW] New connection accepted from 127.0.0.1:xxxxx
📥 [TX FLOW] Spawned handler for connection
📥 [TX FLOW] Waiting to read length prefix (4 bytes)
📥 [TX FLOW] Read length prefix: 656 bytes
📥 [TX FLOW] Received length-prefixed transaction data: size=656 bytes
```

## Root Cause

Go binary cũ không có:
- Logs mới (`TX CLIENT.*Đang gửi`, `TX CLIENT.*writeData`)
- Có thể có bug trong connection handling
- Có thể không gửi đúng length-prefixed protocol

## Next Steps

1. ✅ Rebuild Go binary
2. ✅ Restart Go Sub Node
3. ⏳ Test với transaction mới
4. ⏳ Verify logs mới xuất hiện

