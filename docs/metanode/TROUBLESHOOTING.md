# Troubleshooting Guide

## Vấn đề đã phát hiện và sửa

### 1. Multiaddr Parsing Error

**Lỗi:**
```
Error Network config error: "Cannot convert address to host:port: \"unsupported multiaddr: /ip4/127.0.0.1/tcp/9000\""
```

**Nguyên nhân:**
Hàm `to_host_port_str` trong `sui-consensus/core/src/network/tonic_network.rs` chỉ hỗ trợ `Udp` protocol, nhưng Multiaddr của chúng ta đang sử dụng `Tcp`.

**Giải pháp:**
Đã sửa hàm `to_host_port_str` để hỗ trợ cả `Tcp` và `Udp` protocols.

### 2. Port Conflict

**Lỗi:**
```
Error starting consensus server: Os { code: 98, kind: AddrInUse, message: "Address already in use" }
```

**Nguyên nhân:**
Port 9000 và 9001 đã bị chiếm bởi các process khác (ví dụ: `simple_ch`).

**Giải pháp:**
1. Dừng các process đang chiếm port:
   ```bash
   lsof -i :9000 -i :9001  # Kiểm tra process
   kill <PID>              # Dừng process
   ```

2. Hoặc sử dụng script `stop_nodes.sh`:
   ```bash
   ./stop_nodes.sh
   ```

3. Sau đó khởi động lại nodes:
   ```bash
   ./run_nodes.sh
   ```

## Kiểm tra hệ thống

### Kiểm tra nodes đang chạy:
```bash
ps aux | grep metanode
tmux ls
```

### Kiểm tra ports:
```bash
lsof -i :9000 -i :9001 -i :9002 -i :9003
```

### Xem logs:
```bash
tail -f logs/node_0.log
tail -f logs/node_1.log
tail -f logs/node_2.log
tail -f logs/node_3.log
```

## Các lỗi thường gặp khác

### Node không thể kết nối với peers

**Triệu chứng:**
- Logs hiển thị "Not enough stake: 0 out of 4 total stake"
- Nodes không thể sync blocks

**Giải pháp:**
1. Đảm bảo tất cả 4 nodes đều đang chạy
2. Kiểm tra network connectivity giữa các nodes
3. Kiểm tra firewall settings
4. Xem logs để tìm lỗi cụ thể

### Server không khởi động được

**Triệu chứng:**
- "Address already in use" errors
- Node panic với "Failed to start consensus server within required deadline"

**Giải pháp:**
1. Dừng tất cả nodes: `./stop_nodes.sh`
2. Đợi vài giây để ports được giải phóng
3. Khởi động lại: `./run_nodes.sh`

## Best Practices

1. **Luôn dừng nodes trước khi khởi động lại:**
   ```bash
   ./stop_nodes.sh
   sleep 2
   ./run_nodes.sh
   ```

2. **Kiểm tra logs thường xuyên:**
   ```bash
   tail -f logs/node_*.log
   ```

3. **Sử dụng tmux để quản lý nodes:**
   ```bash
   tmux ls              # Xem sessions
   tmux attach -t node_0  # Attach vào session
   ```

4. **Giữ ports sạch sẽ:**
   - Không chạy nhiều instance cùng lúc
   - Dừng nodes đúng cách trước khi khởi động lại

