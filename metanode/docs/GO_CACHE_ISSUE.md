# Go Cache Issue - Logs mới không xuất hiện

## Vấn đề

Mặc dù script `run_full_system.sh` sử dụng `go run .` (tự động compile code mới), nhưng logs mới không xuất hiện vì:

1. **Go cache**: Go có thể cache binary cũ, dẫn đến code mới không được compile
2. **Binary cũ**: Process có thể đang chạy với binary cũ từ cache

## Giải pháp

Script `run_full_system.sh` đã được cập nhật để **clean Go cache** trước khi chạy `go run`:

```bash
# Clean Go cache để đảm bảo code mới được compile
go clean -cache >/dev/null 2>&1 || true

# Sau đó mới chạy go run
go run . -config=config-sub-write.json
```

## Cách hoạt động

1. **`go clean -cache`**: Xóa Go build cache, đảm bảo code mới được compile từ đầu
2. **`go run .`**: Compile và chạy code mới (không dùng cache cũ)

## Verify

Sau khi chạy script, kiểm tra logs:
- Go Sub: `tail -f mtn-simple-2025/cmd/simple_chain/sample/App.log | grep "TX CLIENT"`
- Bạn sẽ thấy logs mới:
  - `📤 [TX CLIENT] Đang gửi transaction`
  - `📤 [TX CLIENT] writeData: payload_size=...`

## Manual Clean (nếu cần)

Nếu vẫn có vấn đề, có thể clean cache thủ công:

```bash
cd /home/abc/chain-new/mtn-simple-2025
go clean -cache
go clean -modcache  # Optional: clean module cache
```

## Note

- `go run` thường tự động detect code changes và recompile
- Nhưng Go cache có thể giữ binary cũ trong một số trường hợp
- Clean cache đảm bảo code mới luôn được compile

