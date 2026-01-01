# LVM Snapshot Manager & Rsync Sharing

Quản lý snapshot LVM và tự động chia sẻ chỉ một thư mục con qua Rsync.

## 1. Mục đích & Ưu điểm
- Tạo snapshot LVM an toàn (Copy-on-Write, chụp toàn ổ, đảm bảo nhất quán dữ liệu).
- Chỉ công khai (share) **duy nhất một thư mục con** trong snapshot cho các node khác đồng bộ qua Rsync.
- Tự động giữ vòng quay tối đa `max_snapshots` bản mới nhất (mặc định 2 bản, luôn có symlink `latest`).

---

## 2. Cấu trúc dự án
```
lvm-manager/
├── Cargo.toml
├── config.toml  # Chỉnh thông số cho phù hợp
├── README.md    # (File này)
└── src/
    └── main.rs
```

---

## 3. Hướng dẫn sử dụng

### Bước 1: Cài đặt phụ thuộc
```sh
sudo apt update
sudo apt install lvm2 rsync
```

### Bước 2: Cấu hình `config.toml`
Sửa cho đúng với hệ thống của bạn:
```toml
vg_name = "vg_data"               # Tên Volume Group của bạn
lv_name = "lv_storage"            # Tên Logical Volume cần snapshot
snap_prefix = "snap_id"           # Tiền tố snapshot
max_snapshots = 2                  # Chỉ giữ 2 bản mới nhất
base_path = "/mnt/lvm_public"      # Chỗ mount và share dữ liệu
share_subdir = "my_folder"         # Đường dẫn thư mục con cần chia sẻ bên trong snapshot
```

### Bước 3: Build chương trình
```sh
cargo build --release
```

### Bước 4: Chuẩn bị thư mục và quyền
```sh
sudo mkdir -p /mnt/lvm_public
sudo chown $USER:$USER /mnt/lvm_public
```

### Bước 5: Tạo snapshot mới và mount chia sẻ
Ví dụ tạo snapshot cho epoch có ID = 123:
```sh
sudo ./target/release/lvm-snap-rsync --id 123
```
- **Lưu ý:** `id` nên tăng dần (mỗi lần snapshot cho một checkpoint quan trọng hoặc một epoch).
- Tự động xóa bản cũ nếu vượt `max_snapshots`, tự mount snapshot, và cập nhật symlink `latest` trỏ tới đúng thư mục con (`my_folder`) bên trong snapshot.

#### **Kiểm tra thư mục chia sẻ:**
```sh
ls -l /mnt/lvm_public/latest
# Sẽ thấy toàn bộ nội dung của snapshot/my_folder ở đây
```

### Bước 6: Cấu hình Rsync Daemon để share folder `latest`
Tạo file `/etc/rsyncd.conf` như sau:
```
[snapshots]
  path = /mnt/lvm_public
  read only = yes
  comment = Only share latest snapshot subdir
```
Khởi động lại rsync daemon:
```sh
sudo systemctl restart rsync
```

### Bước 7: Node khác tải dữ liệu qua Rsync
```sh
rsync -avz rsync://<IP-SERVER>/snapshots/ /path/to/local/backup/
```
Node khác sẽ chỉ lấy đúng thư mục **con** bạn mong muốn bên trong snapshot.

---

## 4. Ghi chú kỹ thuật nội bộ
- Chương trình chỉ thực sự xóa bản snapshot **cũ nhất** khi vượt số lượng bản tối đa.
- Nếu cần batch tự động (`crontab`) hoặc lệnh lặp, hãy tăng/ghi log ID snapshot ngoài (không lặp lại ID).
- LVM snapshot không tốn nhiều đĩa nếu dữ liệu không thay đổi nhiều (Copy-on-Write).
- Đảm bảo folder con bạn muốn share luôn tồn tại trong mỗi bản snapshot.

## 5. Xử lý lỗi thường gặp
- Nếu mount lỗi: kiểm tra quyền, trạng thái ổ LVM, hoặc tên VG/LV có đúng không.
- Nếu symlink `latest` không tạo, kiểm tra `share_subdir` có chính xác, thư mục có tồn tại thật bên trong snapshot.
- Nếu Rsync báo trống: Thư mục gốc mount ok nhưng thư mục con không tồn tại trong snapshot.
- Có thể cần `sudo` cho mọi lệnh thao tác với LVM.

---

**Liên hệ hoặc mở issue nếu bạn cần tự động hóa nâng cao hoặc tích hợp với process khác.**

