# Hướng dẫn Sử dụng Mã Nguồn Mở của Sui cho Blockchain

## 📚 Tổng quan

Để phát triển blockchain sử dụng lại phần **node giao dịch** và **đồng thuận** của Sui, bạn nên sử dụng các repository mã nguồn mở sau:

---

## 🔗 Các Repository Chính

### 1. **Sui Main Repository** (Khuyến nghị chính)
**URL:** https://github.com/MystenLabs/sui

**Mô tả:**
- Repository chính của Sui blockchain
- Chứa toàn bộ mã nguồn: node, consensus, transaction execution, storage
- **Narwhal consensus** được tích hợp tại: `sui/narwhal/`
- **Transaction processing** tại: `sui/sui-execution/`, `sui/sui-core/`
- **Node implementation** tại: `sui/sui-node/`

**Các thành phần quan trọng:**
- `sui/narwhal/` - Narwhal consensus engine (DAG-based mempool + BFT consensus)
- `sui/sui-execution/` - Transaction execution layer
- `sui/sui-core/` - Core blockchain logic
- `sui/sui-node/` - Full node implementation
- `sui/sui-types/` - Data types và structures
- `sui/sui-storage/` - Storage layer

**License:** Apache 2.0

---

### 2. **Narwhal Standalone Repository** (Độc lập)
**URL:** https://github.com/MystenLabs/narwhal

**Mô tả:**
- Repository độc lập cho Narwhal consensus
- **Lưu ý:** Development chính hiện tại diễn ra trong Sui repo
- Vẫn được publish lên `crates.io` để sử dụng như dependency
- Phù hợp nếu bạn chỉ cần consensus engine, không cần toàn bộ Sui

**Các thành phần:**
- Narwhal DAG mempool
- Bullshark consensus (partially synchronous)
- Tusk consensus (fully asynchronous)
- Worker và Primary nodes

**License:** Apache 2.0

---

### 3. **Mysticeti** (Consensus mới của Sui)
**URL:** https://github.com/MystenLabs/sui/tree/main/mysticeti

**Mô tả:**
- Consensus protocol mới của Sui, được thiết kế để thay thế Narwhal
- Hiệu suất cao hơn, độ trễ thấp hơn
- Vẫn đang trong quá trình phát triển
- Nằm trong Sui main repository

**Khi nào nên dùng:**
- Nếu bạn muốn sử dụng consensus mới nhất của Sui
- Dự án có thể chấp nhận các thay đổi thường xuyên

---

## 🎯 Khuyến nghị Sử dụng

### **Kịch bản 1: Xây dựng Blockchain hoàn chỉnh dựa trên Sui**

**Sử dụng:** https://github.com/MystenLabs/sui

**Lý do:**
- Có đầy đủ các thành phần: consensus, transaction execution, storage, networking
- Được maintain và update thường xuyên
- Có documentation và examples đầy đủ
- Có thể fork và customize theo nhu cầu

**Các bước:**
1. Fork repository: `git clone https://github.com/MystenLabs/sui.git`
2. Nghiên cứu cấu trúc tại `sui/narwhal/` (consensus) và `sui/sui-execution/` (transaction)
3. Customize theo nhu cầu của bạn
4. Build và test

---

### **Kịch bản 2: Chỉ cần Consensus Engine (Narwhal/Bullshark)**

**Sử dụng:** 
- **Option A:** https://github.com/MystenLabs/sui/tree/main/narwhal (khuyến nghị - version mới nhất)
- **Option B:** https://github.com/MystenLabs/narwhal (standalone, ổn định hơn)

**Lý do:**
- Nếu bạn đã có transaction execution layer riêng
- Chỉ cần tích hợp consensus mechanism
- Có thể sử dụng như Rust crate từ `crates.io`

**Cách sử dụng:**
```toml
# Cargo.toml
[dependencies]
narwhal-consensus = { git = "https://github.com/MystenLabs/sui", branch = "main", package = "narwhal-consensus" }
# hoặc
narwhal-consensus = "x.y.z" # từ crates.io
```

---

### **Kịch bản 3: Sử dụng Transaction Processing của Sui**

**Sử dụng:** https://github.com/MystenLabs/sui

**Thành phần cần:**
- `sui/sui-execution/` - Execution engine
- `sui/sui-core/` - Core transaction logic
- `sui/sui-types/` - Transaction types
- `sui/sui-storage/` - State storage

**Lưu ý:**
- Transaction execution của Sui được thiết kế cho Move language
- Nếu bạn dùng Solidity/EVM, cần customize hoặc tìm giải pháp khác

---

## 📦 Các Dependencies Quan trọng

### **FastCrypto**
**URL:** https://github.com/MystenLabs/fastcrypto

**Mô tả:**
- Cryptographic library được Sui sử dụng
- Hỗ trợ BLS signatures, Ed25519, và các thuật toán crypto khác
- Cần thiết cho consensus và transaction signing

---

### **Move Language** (nếu dùng Sui execution)
**URL:** https://github.com/MystenLabs/move

**Mô tả:**
- Programming language cho smart contracts trên Sui
- Nếu bạn muốn sử dụng transaction execution của Sui, cần hiểu Move

---

## 🛠️ Các Bước Bắt đầu

### 1. **Clone Repository**
```bash
git clone https://github.com/MystenLabs/sui.git
cd sui
```

### 2. **Build Project**
```bash
# Cài đặt dependencies
cargo build --release

# Hoặc build chỉ consensus
cd narwhal
cargo build --release
```

### 3. **Nghiên cứu Code Structure**
- Đọc `sui/README.md` để hiểu tổng quan
- Xem `sui/narwhal/README.md` cho consensus
- Xem `sui/sui-execution/README.md` cho transaction execution

### 4. **Chạy Test Network**
```bash
# Chạy local testnet
cargo run --bin sui-test-validator
```

---

## 📖 Tài liệu Tham khảo

1. **Sui Documentation:** https://docs.sui.io/
2. **Narwhal Paper:** https://arxiv.org/pdf/2105.11827.pdf
3. **Bullshark Paper:** https://arxiv.org/pdf/2209.05633.pdf
4. **Sui Blog:** https://blog.sui.io/

---

## ⚠️ Lưu ý Quan trọng

1. **License:** Tất cả đều dùng Apache 2.0 - cho phép sử dụng thương mại
2. **Maintenance:** Sui main repo được update thường xuyên, có thể có breaking changes
3. **Compatibility:** Đảm bảo Rust version tương thích (thường là 1.70+)
4. **Customization:** Cần hiểu rõ architecture trước khi customize để tránh lỗi

---

## 🎯 Kết luận

**Khuyến nghị chính:** Sử dụng **https://github.com/MystenLabs/sui** vì:
- ✅ Có đầy đủ các thành phần bạn cần
- ✅ Được maintain tốt
- ✅ Có documentation đầy đủ
- ✅ Cộng đồng hỗ trợ tốt
- ✅ License thân thiện (Apache 2.0)

**Nếu chỉ cần consensus:** Sử dụng `sui/narwhal/` hoặc standalone `narwhal` repo.

---

**Cập nhật:** Tháng 12, 2025

