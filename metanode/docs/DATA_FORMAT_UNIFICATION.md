# Data Format Unification

## Tổng quan

Đảm bảo tất cả các luồng giao dịch sử dụng cùng một format dữ liệu: **`pb.Transactions` protobuf message**.

## Luồng hiện tại

### 1. Go-Sub → Rust (UDS)

**Format**: `pb.Transactions` protobuf message
- Go: `transaction.MarshalTransactions(batchTxs)` → `pb.Transactions{Transactions: [...]}`
- Rust: `Transactions::decode(tx_data)` → Split thành individual `Transaction` messages

**Status**: ✅ Đã đồng nhất

### 2. Rust Client → Rust (HTTP POST)

**Format**: Raw bytes (hex hoặc text)
- Rust Client: Gửi raw bytes qua HTTP POST body
- Rust RPC Server: 
  - Nếu là length-prefixed → decode `Transactions` protobuf
  - Nếu là HTTP POST → parse HTTP request, body có thể là raw bytes hoặc protobuf

**Status**: ⚠️ Cần kiểm tra và đồng nhất

## Vấn đề

Rust client (`main.rs`) gửi raw bytes, nhưng Rust server chỉ decode `Transactions` protobuf. Cần đảm bảo:

1. **Rust client** cũng gửi `Transactions` protobuf (hoặc `Transaction` protobuf)
2. **Rust server** xử lý cả `Transactions` và `Transaction` protobuf (backward compatibility)

## Giải pháp

### Option 1: Rust Client gửi `Transactions` protobuf (Recommended)

**Thay đổi Rust Client**:
- Tạo `Transactions` protobuf message từ transaction data
- Gửi protobuf bytes qua HTTP POST

**Lợi ích**:
- Đồng nhất với Go-sub
- Rust server không cần thay đổi

### Option 2: Rust Server hỗ trợ cả raw bytes và protobuf

**Thay đổi Rust Server**:
- Thử decode `Transactions` protobuf trước
- Nếu fail, thử decode `Transaction` protobuf
- Nếu fail, xử lý như raw bytes (backward compatibility)

**Lợi ích**:
- Backward compatible
- Rust client không cần thay đổi

## Recommendation

**Option 1** (Rust Client gửi protobuf) là tốt hơn vì:
- Đồng nhất format
- Dễ maintain
- Performance tốt hơn (protobuf encoding/decoding)

## Implementation

### Rust Client: Gửi `Transactions` protobuf

```rust
// Tạo Transactions protobuf message
let transactions = Transactions {
    transactions: vec![transaction], // Single transaction wrapped in Transactions
};

// Encode protobuf
let mut buf = Vec::new();
transactions.encode(&mut buf)?;

// Gửi qua HTTP POST
client.post(&url).body(buf).send().await?;
```

### Rust Server: Chỉ decode `Transactions` (đã có)

```rust
// Chỉ decode Transactions protobuf
let transactions_to_submit = match Transactions::decode(tx_data.as_slice()) {
    Ok(transactions_msg) => {
        // Split thành individual transactions
        ...
    }
    Err(e) => {
        // Error: không phải Transactions protobuf
        ...
    }
};
```

## Current Status

- ✅ Go-sub → Rust (UDS): `pb.Transactions` protobuf
- ⚠️ Rust Client → Rust (HTTP): Raw bytes (cần update để gửi protobuf)
