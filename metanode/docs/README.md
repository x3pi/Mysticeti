# MetaNode Documentation

Tài liệu chi tiết về hệ thống MetaNode Consensus Engine với Go executor integration.

## 📚 Mục lục

### Tổng quan
- [README.md](../Readme.md) - Tổng quan và hướng dẫn nhanh

### Tài liệu kỹ thuật
- [ARCHITECTURE.md](./ARCHITECTURE.md) - Kiến trúc hệ thống và các thành phần
- [CONSENSUS.md](./CONSENSUS.md) - Cơ chế consensus và DAG
- [TRANSACTIONS.md](./TRANSACTIONS.md) - Xử lý transactions và commit processing
- [TRANSACTION_FLOW.md](./TRANSACTION_FLOW.md) - Luồng transaction từ Go Sub Node → Rust Consensus → Go Master
- [RPC_API.md](./RPC_API.md) - RPC API documentation
- [COMMITTEE.md](./COMMITTEE.md) - Committee management và Go integration (tất cả nodes lấy từ Go)
- [RECOVERY.md](./RECOVERY.md) - Recovery process và commit replay khi khởi động
- [EPOCH.md](./EPOCH.md) - Epoch và cách triển khai epoch transition
- [EPOCH_PRODUCTION.md](./EPOCH_PRODUCTION.md) - Best practices cho epoch transition trong production
- [FORK_SAFETY.md](./FORK_SAFETY.md) - Fork-safety mechanisms, progress guarantee và verification
- [QUORUM_LOGIC.md](./QUORUM_LOGIC.md) - Logic quorum cho epoch transition

### Hướng dẫn sử dụng
- [CONFIGURATION.md](./CONFIGURATION.md) - Cấu hình hệ thống
- [DEPLOYMENT.md](./DEPLOYMENT.md) - Triển khai và vận hành
- [DEPLOYMENT_CHECKLIST.md](./DEPLOYMENT_CHECKLIST.md) - Checklist deploy
- [TROUBLESHOOTING.md](./TROUBLESHOOTING.md) - Xử lý sự cố và debugging
- [FAQ.md](./FAQ.md) - Câu hỏi thường gặp về khởi động, recovery, và các vấn đề khác

### Scripts và Tools
- [../scripts/README.md](../scripts/README.md) - Hướng dẫn sử dụng các script tiện ích
- [../scripts/README_FULL_SYSTEM.md](../scripts/README_FULL_SYSTEM.md) - Hướng dẫn chạy full system
- [analysis/](./analysis/) - Các analysis reports và debugging tools

## 🚀 Bắt đầu nhanh

1. Đọc [ARCHITECTURE.md](./ARCHITECTURE.md) để hiểu kiến trúc tổng thể
2. Xem [COMMITTEE.md](./COMMITTEE.md) để hiểu cách tất cả nodes lấy committee từ Go
3. Tham khảo [TRANSACTION_FLOW.md](./TRANSACTION_FLOW.md) để hiểu luồng transaction
4. Sử dụng [scripts/run_full_system.sh](../scripts/run_full_system.sh) để chạy full system

## 🔑 Điểm quan trọng

### Committee Loading
- **Tất cả nodes đều lấy committee từ Go state** qua Unix Domain Socket
- Không phụ thuộc vào `executor_enabled`
- Script `sync_committee_to_genesis.py` tạo `delegator_stakes` từ stake trong committee.json

### Transaction Flow
- Go Sub Node gửi transactions đến Rust qua Unix Domain Socket
- Rust xử lý consensus và commit blocks
- Node 0 (executor_enabled=true) gửi commits đến Go Master
- Transactions được queue trong barrier phase để tránh mất giao dịch

### Epoch Transition
- Tất cả nodes lấy committee mới từ Go state tại epoch transition
- Fork-safety đảm bảo tất cả nodes transition cùng lúc
- Queued transactions được submit lại sau epoch transition

## 📖 Tài liệu tham khảo

- [Sui Documentation](https://docs.sui.io/)
- [Mysticeti Consensus Paper](https://arxiv.org/pdf/2310.14821)
- [Sui GitHub Repository](https://github.com/MystenLabs/sui)
