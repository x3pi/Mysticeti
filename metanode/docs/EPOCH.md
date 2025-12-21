# Epoch và Epoch Transition (Hệ thống hiện tại)

## Tổng quan

**Epoch** là một giai đoạn mà network chạy với **committee cố định**. Khi sang epoch mới:
- **Consensus state (DAG/round)** được reset sạch theo epoch
- Node thực hiện **in-process restart** của authority (không exit process)
- Consensus DB được tách theo epoch để tránh “dính state cũ”

## Nguồn dữ liệu epoch (không còn `epoch_timestamp.txt`)

Hệ thống hiện tại lưu epoch start timestamp trong `committee.json`:
- `epoch`: u64
- `epoch_timestamp_ms`: u64 (milliseconds)

Mục tiêu: **tất cả nodes dùng đúng cùng timestamp**, tránh divergence khi start/restart.

## Cơ chế chuyển epoch đang dùng (production-ready)

### 1) Trigger (time-based)

Nếu `time_based_epoch_change = true` và đủ `epoch_duration_seconds` thì node sẽ tạo proposal “epoch+1”.

### 2) Đồng thuận (vote/quorum)

Proposal/vote được lan truyền qua blocks, node auto-vote idempotent, và proposal chỉ "approved" khi đạt:
- **quorum = 2f+1 stake**

**Vote Propagation:**
- Votes được include trong blocks và propagate đến tất cả nodes
- **CRITICAL**: Votes tiếp tục được broadcast ngay cả sau khi đạt quorum để đảm bảo tất cả nodes đều thấy quorum
- Nếu một node đạt quorum và dừng broadcast votes, các nodes khác sẽ không thấy quorum → không transition → fork

### 3) Fork-safety (commit-index barrier + deterministic values)

Dù quorum đã đạt, node vẫn **chờ commit-index barrier** rồi mới transition để đảm bảo:
- proposal/votes đã được lan truyền đủ rộng
- nodes chuyển epoch ở cùng "điểm logic" theo commit index ⇒ giảm rủi ro fork

**Fork-Safety Validations:**
1. **Commit Index Barrier**: Tất cả nodes phải đạt barrier (`proposal_commit_index + 10`) trước khi transition
2. **Quorum Check**: Phải đạt quorum (2f+1 votes) trước khi transition
3. **Deterministic last_commit_index**: Tất cả nodes dùng `transition_commit_index` (barrier) làm `last_commit_index`, không dùng `current_commit_index`
4. **Deterministic global_exec_index**: Tất cả nodes tính cùng `global_exec_index` từ cùng `last_commit_index`
5. **Proposal Hash Consistency**: Verify proposal hash được tính giống nhau ở tất cả nodes
6. **Timestamp Consistency**: Verify `epoch_timestamp_ms` giống nhau ở tất cả nodes

### 4) Transition (in-process authority restart + per-epoch DB)

Khi đủ điều kiện:
- `committee.json` được ghi atomically (epoch + epoch_timestamp_ms)
- authority được restart ngay trong process
- DB path chuyển sang:

```
config/storage/node_X/epochs/epoch_N/consensus_db
```

## Clock/NTP gate (khuyến nghị production)

Nếu `enable_ntp_sync = true`, node đọc clock offset từ **chrony** (`chronyc tracking`).
Nếu drift > `max_clock_drift_seconds` thì node **không propose epoch** (để tránh propose sai thời điểm khi clock lệch).

## Dữ liệu được làm mới và dữ liệu được giữ nguyên khi chuyển epoch

Khi chuyển từ epoch N sang epoch N+1, hệ thống thực hiện **in-process authority restart** với cơ chế **per-epoch database paths**. Dưới đây là chi tiết về dữ liệu nào được **làm mới (reset)** và dữ liệu nào được **giữ nguyên (preserved)**.

### ✅ Dữ liệu được làm mới (Reset)

#### 1. **Consensus Database (RocksDB) - Per-Epoch Path**

**Được reset hoàn toàn:**
- **DAG state**: Tất cả blocks, rounds, votes của epoch cũ
- **Commit history**: Lịch sử commits của epoch cũ
- **Block references**: Tất cả block references và ancestors
- **Leader schedule**: Lịch trình leader election của epoch cũ

**Cơ chế:**
- Mỗi epoch sử dụng **database path riêng biệt**:
  ```
  config/storage/node_X/epochs/epoch_N/consensus_db/
  config/storage/node_X/epochs/epoch_N+1/consensus_db/  ← DB mới, sạch
  ```
- Khi transition, hệ thống **tạo DB mới** cho epoch N+1 (không xóa DB cũ của epoch N)
- Authority mới khởi động với DB path mới → **DAG/round bắt đầu từ 0**

**Lợi ích:**
- Trạng thái consensus **sạch sẽ** cho mỗi epoch
- Không bị "dính" dữ liệu cũ không cần thiết
- Dễ dàng quản lý và backup dữ liệu theo epoch

#### 2. **Commit Index**

**Được reset về 0:**
```rust
self.current_commit_index.store(0, Ordering::SeqCst);
```

- Commit index của epoch mới bắt đầu từ **0**
- Mỗi epoch có commit index riêng, độc lập

**Lưu ý:** Commit index chỉ reset trong **epoch mới**, không ảnh hưởng đến commit index của epoch cũ (đã được lưu trong DB cũ).

#### 3. **EpochChangeManager State**

**Được reset:**
- `pending_proposals`: Xóa tất cả proposals của epoch cũ
- `proposal_votes`: Xóa tất cả votes của epoch cũ
- `seen_proposals`: Xóa lịch sử proposals đã xử lý
- `quorum_logged`: Reset trạng thái log quorum

**Cơ chế:**
```rust
mgr.reset_for_new_epoch(
    proposal.new_epoch,
    Arc::new(proposal.new_committee.clone()),
    proposal.new_epoch_timestamp_ms,
);
```

#### 4. **Commit Consumer & Commit Processor**

**Được tạo mới:**
- `CommitConsumerArgs::new(0, 0)` → Consumer mới với commit index 0
- `CommitProcessor` mới cho epoch mới
- Block receiver mới

**Lý do:** Đảm bảo xử lý commits của epoch mới hoàn toàn độc lập.

#### 5. **ConsensusAuthority Instance**

**Được tạo mới:**
- Authority cũ được **graceful shutdown**
- Authority mới được khởi động với:
  - Committee mới (epoch N+1)
  - Epoch timestamp mới
  - DB path mới (per-epoch)
  - Boot counter tăng lên (để tracking)

### ✅ Dữ liệu được giữ nguyên (Preserved)

#### 1. **Committee Configuration File (`committee.json`)**

**Được cập nhật (không xóa):**
- File `committee.json` được **ghi đè atomically** với:
  - `epoch`: Tăng từ N → N+1
  - `epoch_timestamp_ms`: Timestamp mới của epoch N+1
  - `committee`: Committee mới (có thể giống hoặc khác epoch cũ)

**Cơ chế atomic write:**
```rust
// Write to temp file, then rename (atomic)
let temp_path = committee_path.with_extension("json.tmp");
fs::write(&temp_path, committee_json)?;
fs::rename(&temp_path, committee_path)?;
```

**Lưu ý:**
- File được **ghi đè**, không giữ lịch sử các epoch trước trong cùng file
- Nếu cần lịch sử, bạn có thể backup `committee.json` trước khi transition

#### 2. **Storage Paths của Epoch Cũ**

**Được giữ nguyên (không xóa):**
- Tất cả thư mục `epochs/epoch_0/`, `epochs/epoch_1/`, ..., `epochs/epoch_N/` **vẫn tồn tại**
- Code có comment rõ ràng: `// do NOT delete old epoch DB`

**Cấu trúc storage sau nhiều epoch transitions:**
```
config/storage/node_0/
├── epochs/
│   ├── epoch_0/
│   │   └── consensus_db/     ← Giữ nguyên
│   ├── epoch_1/
│   │   └── consensus_db/     ← Giữ nguyên
│   ├── ...
│   ├── epoch_N/
│   │   └── consensus_db/     ← Giữ nguyên
│   └── epoch_N+1/
│       └── consensus_db/     ← DB mới cho epoch hiện tại
```

**Lợi ích:**
- Có thể **audit/replay** lại lịch sử các epoch cũ
- Có thể **rollback** về epoch trước nếu cần (với tooling phù hợp)
- Dễ dàng **backup** từng epoch riêng biệt

**Lưu ý về disk space:**
- Mỗi epoch tạo DB mới → **disk usage tăng dần**
- Hiện tại **không có cơ chế auto-prune** (xóa epoch cũ tự động)
- Nếu cần, bạn có thể:
  - Manual cleanup: Xóa thư mục `epochs/epoch_<old>/` khi không cần
  - Hoặc implement retention policy (ví dụ: chỉ giữ 50 epochs gần nhất)

#### 3. **Application State (Nếu có)**

**Được giữ nguyên (nếu bạn lưu riêng):**
- Nếu bạn có **application state riêng** (không lưu trong consensus DB), nó sẽ **không bị ảnh hưởng**
- Ví dụ:
  - Application database riêng (PostgreSQL, MongoDB, ...)
  - File-based state ngoài `consensus_db/`
  - External storage (S3, etc.)

**Lưu ý quan trọng:**
- Consensus DB (RocksDB) **chỉ chứa consensus state** (DAG, blocks, commits)
- **Application state** nên được lưu **riêng biệt** nếu bạn muốn nó persist qua các epoch

#### 4. **Node Configuration Files**

**Được giữ nguyên:**
- `node_X.toml`: Config file không thay đổi
- `node_X_protocol_key.json`: Keypair không thay đổi
- `node_X_network_key.json`: Keypair không thay đổi

#### 5. **Process State (In-Process Restart)**

**Được giữ nguyên:**
- Process ID (PID) không thay đổi (vì là in-process restart, không exit)
- RPC server tiếp tục chạy (không restart)
- Network connections có thể được giữ (tùy implementation)

### 📊 Tóm tắt

| Dữ liệu | Trạng thái khi chuyển epoch | Vị trí |
|---------|----------------------------|--------|
| **Consensus DB (DAG/rounds/blocks)** | ✅ Reset hoàn toàn (DB mới) | `storage/node_X/epochs/epoch_N+1/consensus_db/` |
| **Commit Index** | ✅ Reset về 0 | In-memory (`current_commit_index`) |
| **EpochChangeManager** | ✅ Reset (proposals/votes cũ bị xóa) | In-memory |
| **Commit Consumer/Processor** | ✅ Tạo mới | In-memory |
| **ConsensusAuthority** | ✅ Tạo mới (shutdown cũ) | In-memory |
| **Committee.json** | 🔄 Cập nhật (ghi đè) | `config/committee_node_X.json` |
| **Storage paths epoch cũ** | ✅ Giữ nguyên (không xóa) | `storage/node_X/epochs/epoch_0..N/` |
| **Node config files** | ✅ Giữ nguyên | `config/node_X.toml`, keys |
| **Application state (riêng)** | ✅ Giữ nguyên (nếu lưu riêng) | External storage |

### 🔍 Kiểm tra sau khi chuyển epoch

Để xác nhận transition đã thành công và dữ liệu đúng:

1. **Check committee.json:**
   ```bash
   cat config/committee_node_0.json | jq '.epoch, .epoch_timestamp_ms'
   ```

2. **Check DB paths:**
   ```bash
   ls -la config/storage/node_0/epochs/
   # Sẽ thấy: epoch_0/, epoch_1/, ..., epoch_N/, epoch_N+1/
   ```

3. **Check logs:**
   ```bash
   grep "Epoch transition COMPLETE" logs/latest/node_0.log
   grep "now running epoch" logs/latest/node_0.log
   ```

4. **Check commit index reset:**
   ```bash
   grep "current_commit_index=0" logs/latest/node_0.log | tail -n 1
   ```

## Tham khảo

- `EPOCH_CHANGE_VOTING.md`: vote/quorum.
- `FORK_SAFETY_VERIFICATION_FINAL.md`: fork-safety & commit-index barrier.
- `DEPLOYMENT.md` + `DEPLOYMENT_CHECKLIST.md`: deploy/ops.


