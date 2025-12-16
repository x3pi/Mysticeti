# Fix: Genesis Blocks Synchronization

## Vấn đề

Hệ thống không thể đạt consensus vì mỗi node tạo genesis blocks với **timestamp khác nhau**, dẫn đến hash khác nhau. Khi node nhận block từ peer, nó không tìm thấy genesis block của peer trong DAG state.

**Lỗi trong logs:**
```
Invalid block from [X]: Ancestor B0([X],...) not found among genesis blocks!
```

## Nguyên nhân

Mỗi node sử dụng `SystemTime::now()` làm epoch start timestamp khi khởi động:
- Node 0: timestamp `1765874700068`
- Node 1: timestamp `1765874701088` (khác 1 giây)
- Node 2: timestamp `1765874702108` (khác 2 giây)
- Node 3: timestamp `1765874703130` (khác 3 giây)

→ Genesis blocks có hash khác nhau → không thể validate blocks từ peers

## Giải pháp đã triển khai

1. **Lưu epoch timestamp vào file** khi generate config
   - File: `config/epoch_timestamp.txt`
   - Tất cả nodes sẽ đọc cùng một timestamp từ file này

2. **Sử dụng cùng timestamp** khi khởi động nodes
   - Thay `SystemTime::now()` bằng `config.load_epoch_timestamp()`
   - Đảm bảo tất cả nodes sử dụng cùng epoch start timestamp

## Cách sử dụng

### 1. Tạo lại config (quan trọng!)
```bash
cd /home/abc/chain-new/Mysticeti/metanode
rm -rf config/
cargo run --release --bin metanode -- generate --count 4 --output config
```

### 2. Khởi động lại nodes
```bash
./stop_nodes.sh
./run_nodes.sh
```

## Kiểm tra

Sau khi khởi động, kiểm tra logs:
```bash
grep "epoch start timestamp" logs/node_*.log
```

Tất cả nodes phải có **cùng một timestamp**!

## Lưu ý

- **Phải tạo lại config** để có file `epoch_timestamp.txt`
- Nếu không có file này, hệ thống sẽ fallback về `SystemTime::now()` (không khuyến khích)
- Đảm bảo tất cả nodes đọc từ cùng một config directory

