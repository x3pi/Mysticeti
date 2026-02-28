# Hướng dẫn Khởi chạy Hệ thống với Node 0 mới

## 1. Cấu hình Firewall (Chỉ cần làm một lần)
Mở các port cần thiết để Node 0 và các node khác có thể giao tiếp qua mạng LAN:
```bash
cd Mysticeti/metanode/scripts/node
sudo ./setup_firewall.sh
```

## 2. Dừng toàn bộ hệ thống
Để đảm bảo trạng thái sạch, hãy dừng tất cả các tiến trình đang chạy:
```bash
cd Mysticeti/metanode/scripts/node
./stop_all.sh
```

## 2. Khởi chạy Node 0 (IP: 192.168.1.231)
Node 0 đóng vai trò Master, các node khác sẽ kết nối tới IP LAN của nó.
```bash
./run_node_0.sh
```
*Script này dọn dẹp data Node 0 và bắt đầu Go Master, Go Sub, Metanode-0.*

## 3. Khởi chạy các Node còn lại (1, 2, 3)
Sau khi Node 0 đã ổn định, hãy chạy cụm node cộng sự:
```bash
./run_nodes_123.sh
```
*Script này dọn dẹp data và chạy cụm Node 1, 2, 3.*

## 4. Kiểm tra trạng thái
Kiểm tra các phiên tmux:
```bash
tmux ls
```
Các session mong đợi:
- **Node 0**: `go-master-0`, `go-sub-0`, `metanode-0`
- **Node 1-3**: `go-master-1..3`, `go-sub-1..3`, `metanode-1..3`

## 5. Log & Debug
Log nằm tại: `Mysticeti/metanode/logs/node_N/`
- Rust log: `tail -f logs/node_0/rust.log`
- Go log: `tail -f logs/node_0/go-master-stdout.log`

## 6. Cấu hình Công cụ (tx_sender, block_hash_checker)
Đảm bảo các công cụ kết nối đúng IP của Node 0:
- **tx_sender**: Đã cập nhật `cmd/tool/tx_sender/config.json` để trỏ về `192.168.1.231:4200`.
- **block_hash_checker**: Nếu chạy từ máy khác, hãy đổi `localhost:8747` thành `192.168.1.231:8747`.

