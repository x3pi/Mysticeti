# Fork-Safety Analysis: System Transaction Epoch Transition

## Vấn đề phát hiện

### 1. ❌ Non-deterministic Timestamp
**Vấn đề**: `new_epoch_timestamp_ms` được tạo bằng `SystemTime::now()` trong mỗi node
- Mỗi node có thể có clock khác nhau (clock drift)
- Timestamp khác nhau → system transaction khác nhau → fork!

**Vị trí**: `system_transaction_provider.rs:109`

### 2. ❌ Non-deterministic Commit Index
**Vấn đề**: `current_commit_index` có thể khác nhau giữa các nodes
- Node A: commit_index = 100
- Node B: commit_index = 102 (do network delay)
- → `transition_commit_index` khác nhau → fork!

**Vị trí**: `system_transaction_provider.rs:115`

### 3. ❌ Multiple System Transactions
**Vấn đề**: Nhiều nodes có thể tạo system transaction cùng lúc
- Node A tạo system transaction với timestamp T1, commit_index C1
- Node B tạo system transaction với timestamp T2, commit_index C2
- → Nhiều system transactions khác nhau trong network → fork!

**Vị trí**: `core.rs:650-678`

### 4. ❌ Không có Quorum Check
**Vấn đề**: System transaction không có quorum mechanism
- Proposal/Vote/Quorum đảm bảo 2f+1 nodes đồng ý
- System transaction không có cơ chế này → không đảm bảo consensus

## Giải pháp

### Giải pháp 1: Leader-based System Transaction (Khuyến nghị)

Chỉ leader của round tạo system transaction:
- Deterministic: Chỉ một node tạo system transaction
- Fork-safe: Tất cả nodes nhận cùng system transaction từ leader
- Đơn giản: Không cần quorum check

**Implementation**:
1. Chỉ inject system transaction khi node là leader
2. Sử dụng timestamp từ block (deterministic)
3. Sử dụng commit_index từ committed block (deterministic)

### Giải pháp 2: Quorum-based System Transaction

Thêm quorum check cho system transaction:
- Tương tự Proposal/Vote/Quorum
- Đảm bảo 2f+1 nodes đồng ý
- Phức tạp hơn nhưng an toàn hơn

### Giải pháp 3: Deterministic Values Only

Sử dụng chỉ các giá trị deterministic:
- Timestamp: Từ committed block hoặc từ epoch start + duration
- Commit index: Từ committed block (không phải current)
- Epoch: current_epoch + 1 (deterministic)

## Khuyến nghị

**Sử dụng Giải pháp 1 (Leader-based)** vì:
1. Đơn giản nhất
2. Fork-safe (chỉ một node tạo)
3. Deterministic (tất cả nodes nhận cùng transaction)
4. Phù hợp với Sui's EndOfPublish (system transaction từ leader)
