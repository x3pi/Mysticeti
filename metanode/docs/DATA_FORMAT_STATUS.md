# Data Format Status: Đồng nhất Format Dữ Liệu

## Tổng quan

Đảm bảo tất cả các luồng giao dịch sử dụng format dữ liệu đồng nhất: **Protobuf messages** (`Transactions` hoặc `Transaction`).

## Luồng và Format

### 1. Go-Sub → Rust (UDS) ✅

**Format**: `pb.Transactions` protobuf message
- **Go**: `transaction.MarshalTransactions(batchTxs)` → `pb.Transactions{Transactions: [...]}`
- **Rust UDS Server**: `Transactions::decode(tx_data)` → Split thành individual `Transaction` messages
- **Status**: ✅ Đã đồng nhất

### 2. Rust Client → Rust (HTTP POST) ✅

**Format**: Hỗ trợ cả `Transactions` và `Transaction` protobuf
- **Rust Client**: Có thể gửi raw bytes (hex/text) hoặc protobuf
- **Rust RPC Server**: 
  - Thử decode `Transactions` protobuf trước
  - Nếu fail, thử decode `Transaction` protobuf
  - Nếu fail, reject với error message rõ ràng
- **Status**: ✅ Đã hỗ trợ backward compatibility

## Format Chi Tiết

### pb.Transactions (Nhiều transactions)

```protobuf
message Transactions {
  repeated Transaction Transactions = 1;
}
```

**Sử dụng bởi**:
- Go-sub (luôn gửi `Transactions`)

### pb.Transaction (Single transaction)

```protobuf
message Transaction {
  bytes ToAddress = 1;
  bytes Amount = 3;
  bytes FromAddress = 13;
  bytes Nonce = 12;
  // ... các fields khác
}
```

**Sử dụng bởi**:
- Rust client (có thể gửi single `Transaction`)

## Rust Server Processing

### UDS Server (`tx_socket_server.rs`)

```rust
// Thử decode Transactions trước
match Transactions::decode(&tx_data[..]) {
    Ok(transactions_msg) => {
        // Split thành individual transactions
        ...
    }
    Err(_) => {
        // Thử decode như single Transaction
        match Transaction::decode(&tx_data[..]) {
            Ok(tx) => {
                // Encode và submit
                ...
            }
            Err(e) => {
                // Reject với error
                ...
            }
        }
    }
}
```

### RPC Server (`rpc.rs`)

```rust
// Tương tự UDS server
// Hỗ trợ cả Transactions và Transaction protobuf
```

## Lợi ích

1. **Đồng nhất**: Tất cả luồng sử dụng protobuf
2. **Backward compatible**: Rust server hỗ trợ cả `Transactions` và `Transaction`
3. **Rõ ràng**: Error messages chỉ rõ format mong đợi
4. **Maintainable**: Dễ maintain và debug

## Migration Path

### Rust Client (Optional)

Rust client có thể update để gửi `Transactions` protobuf thay vì raw bytes:

```rust
// Tạo Transactions protobuf
let transactions = Transactions {
    transactions: vec![transaction],
};

// Encode và gửi
let mut buf = Vec::new();
transactions.encode(&mut buf)?;
client.post(&url).body(buf).send().await?;
```

**Lợi ích**:
- Đồng nhất hoàn toàn với Go-sub
- Performance tốt hơn (protobuf encoding/decoding)

**Không bắt buộc**: Rust server đã hỗ trợ cả 2 format.

## Current Status

- ✅ Go-sub → Rust (UDS): `pb.Transactions` protobuf
- ✅ Rust Client → Rust (HTTP): Hỗ trợ cả `Transactions` và `Transaction` protobuf
- ✅ Rust Server: Xử lý cả 2 format (backward compatible)

## Recommendation

1. **Go-sub**: Tiếp tục gửi `pb.Transactions` (đã đúng)
2. **Rust client**: Nên update để gửi `Transactions` protobuf (optional, không bắt buộc)
3. **Rust server**: Đã hỗ trợ cả 2 format (không cần thay đổi)

