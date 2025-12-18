# Deployment Checklist (Hệ thống hiện tại)

Checklist này đã được cập nhật theo implementation hiện tại: **time-based trigger + consensus vote + commit-index barrier + in-process authority restart + per-epoch DB + chrony/clock gate**.

## ✅ Trước khi deploy

- **Binary**
  - [ ] `cargo build --release` thành công
  - [ ] Tất cả nodes chạy đúng cùng binary version

- **Config**
  - [ ] `time_based_epoch_change = true`
  - [ ] `epoch_duration_seconds` đặt theo nhu cầu (prod thường 1h–24h, dev/test 5–10 phút)
  - [ ] `max_clock_drift_seconds` phù hợp infra (thường 5–10s)
  - [ ] `speed_multiplier` theo nhu cầu (test có thể <1.0 để chậm)

- **Clock / NTP (khuyến nghị production)**
  - [ ] Cài và enable **chrony** trên tất cả nodes
  - [ ] `chronyc tracking` báo synced (offset nhỏ)
  - [ ] `enable_ntp_sync = true`

- **Networking**
  - [ ] Mở port consensus (9000..)
  - [ ] Mở port metrics (9100..)
  - [ ] Mở port RPC (metrics_port + 1000)

## ✅ Khi chạy (verification)

- **RPC**
  - [ ] `GET /ready` trả `{"ready":true}`
  - [ ] `POST /submit` gửi tx OK

- **Epoch transition**
  - [ ] Có log `Time-based epoch change trigger`
  - [ ] Có log `QUORUM REACHED`
  - [ ] Có log `EPOCH TRANSITION WAITING FOR COMMIT INDEX`
  - [ ] Có log `Epoch transition COMPLETE in-process`
  - [ ] DB path đổi sang `storage/node_X/epochs/epoch_N/consensus_db`

## ✅ Sau deploy (ops)

- **Monitoring**
  - [ ] Theo dõi log “clock sync completed (chrony)”
  - [ ] Alert nếu “Skipping epoch proposal: clock/NTP sync unhealthy”
  - [ ] Theo dõi epoch.log (grep epoch/transition/quorum)

- **Rollback (dev / staging)**
  - [ ] Dừng nodes
  - [ ] Restore `committee.json` backup (epoch + epoch_timestamp_ms)
  - [ ] Restore `storage/node_X/epochs/epoch_<old>/consensus_db` nếu cần quay về state cũ

