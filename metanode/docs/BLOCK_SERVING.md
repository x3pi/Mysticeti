# Block Serving từ Validators

## Tổng quan

Validators có thể lưu trữ bộ đệm (cache) blocks để giúp các full node hoặc validator khác đồng bộ. Blocks có thể được lấy từ:
1. **In-memory cache** (MemStore trong consensus layer)
2. **Database** (nếu có persistent storage)
3. **Block cache** (InMemoryBlockStore được thêm vào)

## Kiến trúc

### 1. Block Cache cho Validators

Validators sẽ có một `block_cache` (InMemoryBlockStore) để lưu trữ committed blocks:
- Blocks được lưu vào cache khi commit processor xử lý committed subdags
- Cache có thể được query qua RPC API
- Full nodes có thể request blocks từ validators qua HTTP

### 2. RPC API để Serve Blocks

Thêm endpoint vào RPC server:
- `GET /blocks/:index` - Lấy block theo global_exec_index
- `GET /blocks/latest` - Lấy block mới nhất
- `GET /blocks/range?from=:from&to=:to` - Lấy range blocks

### 3. NetworkClient để Request Blocks

Full nodes sử dụng `NetworkClient::request_blocks` để:
- Query blocks từ validator peers
- Sync missing blocks
- Update local block store

## Implementation Status

✅ **Đã hoàn thành:**
- Thêm `block_cache` field vào `ConsensusNode`
- Khởi tạo block_cache cho validators

⏳ **Đang triển khai:**
- Lưu blocks vào cache trong commit processor
- Thêm RPC API endpoints để serve blocks
- Implement NetworkClient::request_blocks để fetch blocks từ validators

## Cách sử dụng

### Validator Configuration
```toml
# Validators tự động có block_cache khi là validator
# Không cần config thêm
```

### Full Node Configuration
```toml
network_sync_enabled = true
network_sync_interval_seconds = 30
network_sync_batch_size = 100
```

Full nodes sẽ tự động:
1. Discover validator peers từ committee
2. Request missing blocks từ validators
3. Store blocks vào local block_store
4. Execute blocks nếu `local_execution_enabled = true`

## Lợi ích

1. **Hiệu quả**: Full nodes có thể sync nhanh từ validators thay vì phải chờ consensus network broadcasts
2. **Độ tin cậy**: Validators có thể serve blocks từ cache hoặc database
3. **Linh hoạt**: Full nodes có thể chọn validator nào để sync từ
4. **Scalability**: Giảm tải cho consensus network bằng cách distribute block serving
