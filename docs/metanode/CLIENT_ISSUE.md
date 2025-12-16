# Client Issue - Hiện tại

## Vấn đề

Client hiện tại cố gắng khởi tạo một node mới để lấy TransactionClient, nhưng:
1. **Database lock**: Node đang chạy đã lock database
2. **Port conflict**: Node đang chạy đã chiếm port
3. **DAG state conflict**: Không thể tạo DAG state mới khi node đang chạy

## Giải pháp hiện tại (Tạm thời)

Client tạo một node tạm thời với:
- Temporary storage path
- Port khác (node_port + 10000)
- Cùng committee và keys

Nhưng vẫn có vấn đề với DAG state và network.

## Giải pháp đề xuất

### Option 1: RPC Endpoint (Khuyến nghị)

Tạo một RPC endpoint đơn giản trên node để client submit transactions:

```rust
// Trên node
async fn submit_transaction_rpc(tx: Vec<u8>) -> Result<BlockRef> {
    let client = authority.transaction_client();
    let (block_ref, _, _) = client.submit(vec![tx]).await?;
    Ok(block_ref)
}
```

Client sẽ gửi HTTP/gRPC request đến node đang chạy.

### Option 2: Shared TransactionClient

Expose TransactionClient qua shared memory hoặc file socket.

### Option 3: Client Mode

Tạo một "client-only" mode cho ConsensusAuthority không start network server và không cần persistent storage.

## Workaround hiện tại

Để submit transaction, bạn có thể:
1. Sử dụng trực tiếp từ code (không qua CLI)
2. Tạo một node tạm thời với storage và port riêng
3. Đợi implement RPC endpoint

## Xem thêm

- [DEPLOYMENT.md](./DEPLOYMENT.md) - Hướng dẫn triển khai
- [TROUBLESHOOTING.md](./TROUBLESHOOTING.md) - Xử lý sự cố

