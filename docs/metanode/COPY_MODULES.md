# Hướng dẫn Copy Modules từ Sui

## Đã copy

1. ✅ `consensus-core` - Core consensus logic
2. ✅ `consensus-config` - Configuration
3. ✅ `consensus-types` - Types
4. ✅ `sui-http` - HTTP server (đã loại bỏ dev-dependencies)
5. ✅ `sui-tls` - TLS handling (đã sửa để loại bỏ axum-server)

## Cấu trúc

```
metanode/
├── sui-consensus/
│   ├── config/          # Consensus configuration
│   ├── core/            # Core consensus engine
│   ├── types/           # Consensus types
│   ├── sui-http/        # HTTP server (local copy)
│   └── sui-tls/         # TLS handling (local copy, đã sửa)
└── src/                 # MetaNode code
```

## Các thay đổi đã thực hiện

### 1. Loại bỏ axum-server dependency
- Comment out dev-dependencies trong `sui-http`
- Sửa `sui-tls` để không dùng `axum-server`
- Sửa `acceptor.rs` để dùng implementation đơn giản hơn

### 2. Sửa dependencies
- Chuyển từ workspace dependencies sang explicit versions
- Sửa các version conflicts (webpki, tower-layer, etc.)

### 3. Sửa code compatibility
- Sửa edition issues (2024 -> 2021)
- Sửa syntax issues (let expressions)

## Các lỗi còn lại

Cần sửa thêm một số lỗi compile trong:
- `consensus-core` - một số API changes
- `sui-tls` - webpki API changes

## Cách tiếp tục

1. Sửa các lỗi compile còn lại
2. Test consensus engine
3. Build binary

