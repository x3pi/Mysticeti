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

Proposal/vote được lan truyền qua blocks, node auto-vote idempotent, và proposal chỉ “approved” khi đạt:
- **quorum = 2f+1 stake**

### 3) Fork-safety (commit-index barrier)

Dù quorum đã đạt, node vẫn **chờ commit-index barrier** rồi mới transition để đảm bảo:
- proposal/votes đã được lan truyền đủ rộng
- nodes chuyển epoch ở cùng “điểm logic” theo commit index ⇒ giảm rủi ro fork

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

## Tham khảo

- `EPOCH_CHANGE_VOTING.md`: vote/quorum.
- `FORK_SAFETY_VERIFICATION_FINAL.md`: fork-safety & commit-index barrier.
- `DEPLOYMENT.md` + `DEPLOYMENT_CHECKLIST.md`: deploy/ops.


