# LVM Epoch Snapshot Manager & Download Server

Tự động tạo snapshot LVM mỗi khi chuyển epoch + HTTP server phục vụ tải snapshot.

## Tính năng

- 📸 **Tạo snapshot tự động** mỗi epoch transition
- 🔄 **Xoay vòng** giữ tối đa 2 bản mới nhất
- 🌐 **HTTP Download Server** với:
  - ✅ **Range requests** — tiếp tục tải nếu bị lỗi (resume)
  - ✅ **Streaming** — không giới hạn dung lượng (hỗ trợ hàng trăm TB)
  - ✅ **Đa luồng** — tải nhiều file/nhiều kết nối đồng thời
  - ✅ **Tương thích** wget, curl, aria2c, rsync

## Cấu hình `config.toml`

```toml
vg_name = "ubuntu-vg"
lv_name = "ubuntu-lv"
snap_prefix = "snap_id"
max_snapshots = 2
base_path = "/mnt/lvm_public"
sudo_password = "your_password"
serve_port = 8600
```

## Build

```sh
cargo build --release
```

## Sử dụng

### 1. Tạo snapshot (tự động hoặc thủ công)

```sh
# Tạo snapshot cho epoch 144
sudo ./target/release/lvm-snap-rsync snapshot --id 144

# Legacy mode (tương thích ngược với Rust integration)
sudo ./target/release/lvm-snap-rsync --id 144
```

### 2. Khởi động Download Server

```sh
# Mặc định: http://0.0.0.0:8600
./target/release/lvm-snap-rsync serve

# Custom port và bind
./target/release/lvm-snap-rsync serve --port 9000 --bind 0.0.0.0
```

### 3. Tải snapshot từ node khác

```sh
# wget (resume với -c)
wget -c -r -np -nH --cut-dirs=1 http://<server>:8600/snap_id_000144/

# aria2c (đa luồng 16, resume, nhanh nhất cho file lớn)
aria2c -x 16 -s 16 -c http://<server>:8600/snap_id_000144/data.db

# curl (resume)
curl -C - -O http://<server>:8600/snap_id_000144/path/to/file
```

## Tích hợp tự động trong Metanode

Config trong `node_X.toml`:

```toml
enable_lvm_snapshot = true
lvm_snapshot_bin_path = "/path/to/lvm-snap-rsync"
lvm_snapshot_delay_seconds = 5
```

Mỗi epoch transition, Rust sẽ tự động gọi tạo snapshot.
