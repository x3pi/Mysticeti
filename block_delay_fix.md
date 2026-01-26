# Hướng dẫn Cấu hình Delay Đóng Block 10 Giây (Mysticeti)

Tài liệu này tóm tắt các thay đổi đã thực hiện để ép hệ thống consensus đóng block mỗi 10 giây thay vì tốc độ mặc định (~200ms).

## 1. Nguyên nhân lỗi ban đầu
Trước khi sửa, dù bạn đã đặt `adaptive_delay_ms = 10000` trong file `.toml`, block vẫn ra nhanh vì:
- **`adaptive_delay_ms`** chỉ là delay phụ trội khi node chạy nhanh hơn mạng, không phải tham số gốc điều khiển khoảng cách block.
- **Code Rust** (`metanode/src/node/mod.rs`) đang bỏ qua (ignore) tham số `min_round_delay_ms` từ file config.

## 2. Các thay đổi đã thực hiện

### A. Sửa Code Rust (`metanode/src/node/mod.rs`)
Mình đã cập nhật logic khởi tạo node để hệ thống luôn đọc và áp dụng các tham số timing từ file `.toml`:
- Luôn ưu tiên `min_round_delay_ms` và `leader_timeout_ms` nếu chúng xuất hiện trong config.
- Thêm log "Final timing" khi khởi động để dễ dàng kiểm tra.

### B. Cập nhật Script Khởi động (`scripts/run_mixed_system.sh`)
Script hiện tại sẽ tự động chèn các cấu hình sau vào tất cả các file `node_X.toml` trước khi chạy:
- `min_round_delay_ms = 10000`: Quy định khoảng cách tối thiểu giữa các block là 10 giây.
- `leader_timeout_ms = 15000`: Thời gian tối đa chờ Leader (phải lớn hơn delay tối thiểu).

## 3. Ý nghĩa các tham số quan trọng

| Tham số | Giá trị | Ý nghĩa thực tế |
| :--- | :--- | :--- |
| **`min_round_delay_ms`** | **10000 (10s)** | **Quan trọng nhất.** Node sẽ chờ ít nhất 10s mới đóng block mới. |
| **`leader_timeout_ms`** | **15000 (15s)** | Nếu leader không ra block, sau 15s node khác sẽ tự nhảy vào thay. |
| **`adaptive_delay_ms`** | 10000 (10s) | Delay "thích ứng" bổ sung (không dùng làm delay gốc). |

## 4. Cách kiểm tra kết quả
Sau khi chạy `./run_mixed_system.sh`, bạn có thể kiểm tra log của node Rust:
```bash
grep "Final timing" logs/metanode-0.log
```
Kết quả mong đợi:
`INFO metanode::node: 📊 Final timing: min_round_delay=10s, leader_timeout=15s`

Và xem log Go hoặc Rust để thấy các block/round mới được tạo ra cách nhau đúng 10 giây.
