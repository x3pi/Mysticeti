# MetaNode Documentation

Tài liệu chi tiết về hệ thống MetaNode Consensus Engine.

## 📚 Mục lục

### Tổng quan
- [README.md](../Readme.md) - Tổng quan và hướng dẫn nhanh

### Tài liệu kỹ thuật
- [ARCHITECTURE.md](./ARCHITECTURE.md) - Kiến trúc hệ thống và các thành phần
- [CONSENSUS.md](./CONSENSUS.md) - Cơ chế consensus và DAG
- [TRANSACTIONS.md](./TRANSACTIONS.md) - Xử lý transactions và commit processing
- [RPC_API.md](./RPC_API.md) - RPC API documentation
- [COMMITTEE.md](./COMMITTEE.md) - Giải thích về committee.json và cấu hình authorities
- [RECOVERY.md](./RECOVERY.md) - Recovery process và commit replay khi khởi động
- [EPOCH.md](./EPOCH.md) - Epoch và cách triển khai epoch transition
- [EPOCH_PRODUCTION.md](./EPOCH_PRODUCTION.md) - Best practices cho epoch transition trong production
- [BCS_BACKWARD_COMPATIBILITY.md](./BCS_BACKWARD_COMPATIBILITY.md) - BCS backward compatibility và migration strategy

### Hướng dẫn sử dụng
- [CONFIGURATION.md](./CONFIGURATION.md) - Cấu hình hệ thống
- [DEPLOYMENT.md](./DEPLOYMENT.md) - Triển khai và vận hành
- [DEPLOYMENT_CHECKLIST.md](./DEPLOYMENT_CHECKLIST.md) - Checklist deploy (đã cập nhật theo hệ thống hiện tại)
- [TROUBLESHOOTING.md](./TROUBLESHOOTING.md) - Xử lý sự cố và debugging
- [FAQ.md](./FAQ.md) - Câu hỏi thường gặp về khởi động, recovery, và các vấn đề khác

### Tài liệu lịch sử (tham khảo, không phải “source of truth”)

Các file dưới đây chủ yếu là log/phân tích theo từng giai đoạn debug/triển khai; nội dung có thể trùng lặp hoặc không còn cần cho vận hành hằng ngày:
- `EPOCH_CHANGE_ANALYSIS.md`
- `EPOCH_CHANGE_FIXES.md`
- `EPOCH_CHANGE_IMPLEMENTATION_COMPLETE.md`
- `EPOCH_CHANGE_TIMING.md`
- `FORK_SAFETY_IMPROVEMENTS.md`
- `FORK_SAFETY_VERIFICATION.md`
- `FORK_SAFETY_VERIFICATION_FINAL.md`
- `EPOCH_CHANGE_VOTING.md`

## 🚀 Bắt đầu nhanh

1. Đọc [ARCHITECTURE.md](./ARCHITECTURE.md) để hiểu kiến trúc tổng thể
2. Xem [CONFIGURATION.md](./CONFIGURATION.md) để cấu hình hệ thống
3. Tham khảo [DEPLOYMENT.md](./DEPLOYMENT.md) để triển khai
4. Sử dụng [RPC_API.md](./RPC_API.md) để tích hợp client

## 📖 Tài liệu tham khảo

- [Sui Documentation](https://docs.sui.io/)
- [Mysticeti Consensus Paper](https://arxiv.org/pdf/2310.14821)
- [Sui GitHub Repository](https://github.com/MystenLabs/sui)

