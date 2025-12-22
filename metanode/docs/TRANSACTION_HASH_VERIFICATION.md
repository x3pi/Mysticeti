# Transaction Hash Verification

## Tổng quan

Tài liệu này xác minh rằng cách tính hash transaction trong Rust (`narwhal-bullshark`) khớp hoàn toàn với cách tính trong Go (`mtn-simple-2025`).

## Quy trình tính hash

### Go Implementation (`mtn-simple-2025/pkg/transaction/transaction.go`)

```go
func (t *Transaction) Hash() common.Hash {
    hashPb := &pb.TransactionHashData{
        FromAddress:   t.proto.FromAddress,
        ToAddress:     t.proto.ToAddress,
        Amount:        t.proto.Amount,
        MaxGas:        t.proto.MaxGas,
        MaxGasPrice:   t.proto.MaxGasPrice,
        MaxTimeUse:    t.proto.MaxTimeUse,
        Data:          t.proto.Data,
        Type:          t.proto.Type,
        LastDeviceKey: t.proto.LastDeviceKey,
        NewDeviceKey:  t.proto.NewDeviceKey,
        Nonce:         t.proto.Nonce,
        ChainID:       t.proto.ChainID,
        R:             t.proto.R,
        S:             t.proto.S,
        V:             t.proto.V,
        GasTipCap:     t.proto.GasTipCap,
        GasFeeCap:     t.proto.GasFeeCap,
        AccessList:    t.proto.AccessList,
    }
    
    bHashPb, _ := proto.Marshal(hashPb)
    hash := crypto.Keccak256Hash(bHashPb)
    return hash
}
```

### Rust Implementation (`narwhal-bullshark/worker/src/transaction_logger.rs`)

```rust
pub fn calculate_transaction_hash(tx: &Transaction) -> Vec<u8> {
    let hash_data = transaction::TransactionHashData {
        from_address: tx.from_address.clone(),
        to_address: tx.to_address.clone(),
        amount: tx.amount.clone(),
        max_gas: tx.max_gas,
        max_gas_price: tx.max_gas_price,
        max_time_use: tx.max_time_use,
        data: tx.data.clone(),
        r#type: tx.r#type,
        last_device_key: tx.last_device_key.clone(),
        new_device_key: tx.new_device_key.clone(),
        nonce: tx.nonce.clone(),
        chain_id: tx.chain_id,
        r: tx.r.clone(),
        s: tx.s.clone(),
        v: tx.v.clone(),
        gas_tip_cap: tx.gas_tip_cap.clone(),
        gas_fee_cap: tx.gas_fee_cap.clone(),
        access_list: tx
            .access_list
            .iter()
            .map(|at| AccessTuple {
                address: at.address.clone(),
                storage_keys: at.storage_keys.clone(),
            })
            .collect(),
    };
    
    let mut buf = Vec::new();
    hash_data.encode(&mut buf)?;
    let hash = Keccak256::digest(&buf);
    hash.to_vec()
}
```

## So sánh chi tiết

### 1. Field Mapping

Cả hai implementation đều map các field từ `Transaction` sang `TransactionHashData` theo cùng một thứ tự:

| TransactionHashData Field | Go Source | Rust Source | Protobuf Field Number |
|---------------------------|-----------|-------------|----------------------|
| FromAddress | `t.proto.FromAddress` | `tx.from_address` | 1 |
| ToAddress | `t.proto.ToAddress` | `tx.to_address` | 2 |
| Amount | `t.proto.Amount` | `tx.amount` | 3 |
| MaxGas | `t.proto.MaxGas` | `tx.max_gas` | 4 |
| MaxGasPrice | `t.proto.MaxGasPrice` | `tx.max_gas_price` | 5 |
| MaxTimeUse | `t.proto.MaxTimeUse` | `tx.max_time_use` | 6 |
| Data | `t.proto.Data` | `tx.data` | 7 |
| Type | `t.proto.Type` | `tx.r#type` | 8 |
| LastDeviceKey | `t.proto.LastDeviceKey` | `tx.last_device_key` | 9 |
| NewDeviceKey | `t.proto.NewDeviceKey` | `tx.new_device_key` | 10 |
| Nonce | `t.proto.Nonce` | `tx.nonce` | 11 |
| ChainID | `t.proto.ChainID` | `tx.chain_id` | 12 |
| R | `t.proto.R` | `tx.r` | 13 |
| S | `t.proto.S` | `tx.s` | 14 |
| V | `t.proto.V` | `tx.v` | 15 |
| GasTipCap | `t.proto.GasTipCap` | `tx.gas_tip_cap` | 16 |
| GasFeeCap | `t.proto.GasFeeCap` | `tx.gas_fee_cap` | 17 |
| AccessList | `t.proto.AccessList` | `tx.access_list` (mapped) | 18 |

**✅ Kết luận**: Field mapping hoàn toàn khớp nhau.

### 2. Protobuf Encoding

- **Go**: Sử dụng `proto.Marshal(hashPb)` từ `google.golang.org/protobuf/proto`
- **Rust**: Sử dụng `hash_data.encode(&mut buf)` từ `prost::Message`

Cả hai đều tuân theo [Protocol Buffers Wire Format](https://developers.google.com/protocol-buffers/docs/encoding), do đó sẽ tạo ra cùng một byte sequence cho cùng một input data.

**✅ Kết luận**: Protobuf encoding là deterministic và khớp nhau.

### 3. Hash Algorithm

- **Go**: `crypto.Keccak256Hash(bHashPb)` từ `github.com/ethereum/go-ethereum/crypto`
- **Rust**: `Keccak256::digest(&buf)` từ `sha3` crate

Cả hai đều implement [Keccak-256](https://en.wikipedia.org/wiki/SHA-3) (SHA-3 variant), do đó sẽ tạo ra cùng một hash cho cùng một input.

**✅ Kết luận**: Hash algorithm khớp nhau.

## Protobuf Definition

File định nghĩa protobuf (`narwhal-bullshark/node/proto/transaction.proto`):

```protobuf
message TransactionHashData {
  bytes FromAddress = 1;
  bytes ToAddress = 2;
  bytes Amount = 3;
  uint64 MaxGas = 4;
  uint64 MaxGasPrice = 5;
  uint64 MaxTimeUse = 6;
  bytes Data = 7;
  uint64 Type = 8;
  bytes LastDeviceKey = 9;
  bytes NewDeviceKey = 10;
  bytes Nonce = 11;
  uint64 ChainID = 12;
  bytes R = 13;
  bytes S = 14;
  bytes V = 15;
  bytes GasTipCap = 16;
  bytes GasFeeCap = 17;
  repeated AccessTuple AccessList = 18;
}
```

## Kết luận

✅ **Implementation trong Rust (`narwhal-bullshark/worker/src/transaction_logger.rs`) hoàn toàn khớp với implementation trong Go (`mtn-simple-2025/pkg/transaction/transaction.go`).**

Cả hai đều:
1. Tạo `TransactionHashData` từ `Transaction` với cùng field mapping
2. Encode `TransactionHashData` thành protobuf bytes
3. Tính Keccak256 hash của encoded bytes

Do đó, cùng một transaction sẽ có cùng một hash trong cả Go và Rust.

## Lưu ý

- Hash được tính từ **encoded protobuf bytes** của `TransactionHashData`, không phải từ raw transaction data
- Điều này đảm bảo tính nhất quán giữa các implementation khác nhau (Go, Rust, TypeScript, v.v.)
- Nếu cần thay đổi cách tính hash, phải cập nhật cả Go và Rust cùng lúc để đảm bảo tính tương thích

