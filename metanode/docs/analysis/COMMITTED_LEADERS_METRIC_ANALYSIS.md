# Phân Tích Vấn Đề: Committed Leaders Metric Không Thay Đổi Sau Epoch Transition

## 🔍 Vấn Đề

Metric `committed_leaders_total` không thay đổi sau khi chuyển đổi epoch, có vẻ như nó đang giữ nguyên giá trị từ epoch đầu tiên.

## 📊 Phân Tích Nguyên Nhân

### 1. **Cơ Chế Epoch Transition**

Khi epoch transition xảy ra (từ `node.rs:836-851`):

```rust
let authority = ConsensusAuthority::start(
    // ... các parameters ...
    Registry::new(),  // ← Tạo Registry MỚI cho epoch mới
    self.boot_counter,
)
.await;
```

**Vấn đề:**
- Mỗi epoch tạo một `Registry::new()` mới
- Registry mới này chứa metrics mới, bắt đầu từ 0
- Nhưng **metrics server vẫn expose registry cũ** (registry từ epoch đầu tiên)

### 2. **Metrics Server Setup**

Trong `main.rs` (dòng 93-96):

```rust
let registry = if let Some(ref rs) = registry_service {
    rs.default_registry()  // ← Lấy registry mặc định (epoch đầu tiên)
} else {
    prometheus::Registry::new()
};

let node = Arc::new(Mutex::new(ConsensusNode::new_with_registry(node_config.clone(), registry).await?));
```

**Vấn đề:**
- Metrics server được khởi tạo với `registry_service.default_registry()`
- Registry này được tạo một lần khi node khởi động
- Khi epoch transition, registry mới được tạo nhưng **KHÔNG được thêm vào `registry_service`**
- Metrics server vẫn expose registry cũ → metrics không thay đổi

### 3. **Metric Type: IntCounterVec**

`committed_leaders_total` là một **IntCounterVec** (Prometheus counter):
- Là counter tích lũy, chỉ tăng, không tự động reset
- Được tạo trong registry mới mỗi epoch
- Nhưng registry mới không được expose qua metrics server

## 🔧 Giải Pháp Đề Xuất

### Giải Pháp 1: Thêm Registry Mới Vào RegistryService (Khuyến Nghị)

Khi epoch transition, thêm registry mới vào `RegistryService`:

```rust
// Trong node.rs, sau khi tạo authority mới
let new_registry = Registry::new();
let authority = ConsensusAuthority::start(
    // ...
    new_registry.clone(),  // Sử dụng registry mới
    self.boot_counter,
)
.await;

// Thêm registry mới vào registry_service
if let Some(ref rs) = self.registry_service {
    let _registry_id = rs.add(new_registry);
    // Có thể remove registry cũ nếu cần
}
```

**Ưu điểm:**
- Metrics server sẽ expose cả registry cũ và mới
- Có thể theo dõi metrics của cả hai epoch
- Không mất dữ liệu metrics

**Nhược điểm:**
- Metrics sẽ tích lũy từ nhiều epoch
- Cần quản lý lifecycle của registries

### Giải Pháp 2: Reset Metrics Trong Registry Cũ

Reset tất cả metrics trong registry cũ khi epoch transition:

```rust
// Trong node.rs, trước khi tạo authority mới
if let Some(ref rs) = self.registry_service {
    let registry = rs.default_registry();
    // Reset tất cả counters về 0
    // (Cần implement function reset cho Prometheus Registry)
}
```

**Ưu điểm:**
- Metrics bắt đầu từ 0 mỗi epoch
- Dễ theo dõi metrics theo epoch

**Nhược điểm:**
- Prometheus Registry không có built-in reset function
- Cần implement custom reset logic
- Mất dữ liệu metrics của epoch cũ

### Giải Pháp 3: Thêm Epoch Label Vào Metric

Thêm label `epoch` vào metric `committed_leaders_total`:

```rust
// Trong metrics.rs
committed_leaders_total: register_int_counter_vec_with_registry!(
    "committed_leaders_total",
    "Total number of (direct or indirect) committed leaders per authority",
    &["authority", "commit_type", "epoch"],  // ← Thêm "epoch" label
    registry,
).unwrap(),

// Khi update metric
context
    .metrics
    .node_metrics
    .committed_leaders_total
    .with_label_values(&[leader_host, &status, &format!("{}", context.epoch)])
    .inc();
```

**Ưu điểm:**
- Có thể theo dõi metrics theo từng epoch
- Không cần reset metrics
- Dữ liệu metrics được preserve

**Nhược điểm:**
- Cần sửa code trong nhiều nơi
- Metrics sẽ có nhiều time series hơn

### Giải Pháp 4: Sử Dụng Cùng Registry Cho Tất Cả Epoch

Thay vì tạo registry mới, sử dụng lại registry cũ:

```rust
// Trong node.rs
let registry = if let Some(ref rs) = self.registry_service {
    rs.default_registry()  // Sử dụng registry cũ
} else {
    Registry::new()
};

let authority = ConsensusAuthority::start(
    // ...
    registry,  // Sử dụng registry cũ
    self.boot_counter,
)
.await;
```

**Ưu điểm:**
- Đơn giản, không cần thay đổi nhiều
- Metrics tiếp tục tích lũy

**Nhược điểm:**
- Metrics không reset khi epoch thay đổi
- Khó phân biệt metrics của epoch nào

## 📝 Khuyến Nghị

**Giải pháp tốt nhất: Giải Pháp 3 (Thêm Epoch Label)**

Lý do:
1. Cho phép theo dõi metrics theo từng epoch
2. Không mất dữ liệu metrics
3. Phù hợp với best practices của Prometheus (sử dụng labels để phân biệt)
4. Có thể query metrics theo epoch: `committed_leaders_total{epoch="1"}`, `committed_leaders_total{epoch="2"}`

## 🔍 Kiểm Tra Hiện Tại

Để xác nhận vấn đề:

```bash
# Kiểm tra metrics hiện tại
curl -s http://127.0.0.1:9100/metrics | grep "committed_leaders_total"

# Kiểm tra epoch hiện tại
curl -s http://127.0.0.1:9100/metrics | grep "epoch"

# Kiểm tra commit index (sẽ reset về 0 mỗi epoch)
curl -s http://127.0.0.1:9100/metrics | grep "last_commit_index"
```

## 📊 Tác Động

**Hiện tại:**
- `committed_leaders_total` giữ nguyên giá trị từ epoch đầu tiên
- Không thể phân biệt metrics của epoch nào
- Metrics không phản ánh đúng trạng thái hiện tại

**Sau khi sửa:**
- Có thể theo dõi metrics theo từng epoch
- Metrics phản ánh đúng trạng thái hiện tại
- Có thể so sánh performance giữa các epoch

