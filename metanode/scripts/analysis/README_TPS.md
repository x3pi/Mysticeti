# 🚀 TPS Calculator Scripts

**3 scripts tính TPS** với độ chính xác khác nhau từ MetaNode metrics endpoint.

## 📊 Các Script TPS

### 1. `calculate_tps.sh` - TPS Ước tính từ Blocks
- **Dựa trên**: `committed_leaders_total` × `TX/block estimate`
- **Ưu điểm**: Simple, dễ hiểu
- **Nhược điểm**: Ước tính, không chính xác số TX thực tế

### 2. `tps_simple.sh` - TPS Đơn giản từ Blocks
- **Dựa trên**: `committed_leaders_total` × `TX/block estimate`
- **Ưu điểm**: Output đơn giản, real-time
- **Nhược điểm**: Ước tính, không chính xác

### 3. `calculate_real_tps.sh` - **TPS CHÍNH XÁC từ Transactions**
- **Dựa trên**: `finalizer_transaction_status{status="direct_finalize"}`
- **Ưu điểm**: **Đếm số transactions THỰC TẾ được finalize**
- **Nhược điểm**: Chỉ đếm transactions đã finalize (không phải proposed)

## 📋 Cách sử dụng

### Chạy script:

#### Script chính xác (Khuyến nghị):
```bash
./calculate_real_tps.sh [node_port] [interval_seconds]

# Ví dụ:
./calculate_real_tps.sh 9103 5    # Node 3, cập nhật 5s
./calculate_real_tps.sh           # Mặc định: port 9103, 5s
```

#### Script ước tính:
```bash
# Script đầy đủ
./calculate_tps.sh [node_port] [interval] [tx_per_block]
./calculate_tps.sh 9103 5 10     # Node 3, 5s, 10 tx/block

# Script đơn giản
./tps_simple.sh [node_port]
./tps_simple.sh 9103              # Node 3, mặc định 10 tx/block
```

### Output mẫu:

**Script chính xác (`calculate_real_tps.sh`):**
```
┌─────────────────────────────────────────────────────────────────────┐
│ Time     │ TPS (Finalized) │ TPS (Blocks) │ Finalized TX │ Blocks │
├─────────────────────────────────────────────────────────────────────┤
│ 12:01:13 │           0.0 │        0.00 │       34289 │      0 │
│ 12:01:16 │           2.2 │        0.00 │       34298 │      0 │
│ 12:01:19 │           2.7 │        0.00 │       34306 │      0 │
```

**Script ước tính (`calculate_tps.sh`):**
```
┌─────────────────────────────────────────────────────────────────────────┐
│ Time     │ Blocks/sec │ TPS Est. │ Committed │ Accepted │ Verified │
├─────────────────────────────────────────────────────────────────────────┤
│ 11:56:58 │     2.34 │     23.4 │      1234 │    49278 │     1189 │
│ 11:57:01 │     1.87 │     18.7 │      1245 │    49291 │     1201 │
```

**Script đơn giản (`tps_simple.sh`):**
```
📊 TPS Monitor - Node 9103 (TX/block: 10)
11:57:55 - Blocks/sec: 4.00, TPS: 40.0, Total committed: 49437
```

## 📊 Metrics được sử dụng

### Script Chính xác:
| Metric | Ý nghĩa | Cách tính |
|--------|---------|-----------|
| `finalizer_transaction_status{status="direct_finalize"}` | **Số transactions đã finalize** | **TPS THỰC TẾ** |
| `committed_leaders_total` | Số blocks committed | Blocks/sec (để so sánh) |

### Script Ước tính:
| Metric | Ý nghĩa | Cách tính |
|--------|---------|-----------|
| `committed_leaders_total` | Số blocks đã commit thành công | Blocks committed / giây |
| `accepted_blocks{source="own"}` | Blocks được accept từ node này | Throughput estimate |
| `verified_blocks` | Blocks đã được verify | Processing rate |

## 🎯 TPS Calculation

```
TPS = (Committed Blocks/Second) × (Estimated TX/Block)
```

### Ví dụ:
- Nếu commit 2.5 blocks/giây
- Với 10 tx/block → TPS = 25

## ⚙️ Parameters

### Script Chính xác:
| Parameter | Mặc định | Ý nghĩa |
|-----------|----------|---------|
| `node_port` | 9103 | Port metrics của node (9100-9103) |
| `interval` | 5 | Thời gian cập nhật (giây) |

### Script Ước tính:
| Parameter | Mặc định | Ý nghĩa |
|-----------|----------|---------|
| `node_port` | 9103 | Port metrics của node (9100-9103) |
| `interval` | 5 | Thời gian cập nhật (giây) |
| `tx_per_block` | 10 | Số tx ước tính mỗi block |

## 🎨 Color Coding

- 🟢 **Xanh**: TPS > 100 (High throughput)
- 🟡 **Vàng**: TPS 10-100 (Medium throughput)
- 🔴 **Đỏ**: TPS < 10 (Low throughput)

## 📝 Notes

1. **TX/Block estimate**: Điều chỉnh theo workload thực tế
2. **Real-time**: Script cập nhật liên tục
3. **Accuracy**: TPS ước tính dựa trên committed blocks
4. **Requirements**: MetaNode phải chạy với `enable_metrics = true`

## 🛠️ Troubleshooting

### Không kết nối được metrics:
```bash
# Kiểm tra node có chạy không
curl http://localhost:9103/metrics

# Kiểm tra config
grep "metrics_port" ~/chain-n/Mysticeti/metanode/config/node_*.toml
```

### TPS luôn = 0:
- Node chưa commit blocks
- Network issues
- Consensus problems

### Cần dừng script:
```bash
Ctrl+C
```

## 📈 Advanced Usage

### Theo dõi nhiều nodes cùng lúc:
```bash
# Terminal 1
./calculate_tps.sh 9100 5 10

# Terminal 2
./calculate_tps.sh 9101 5 10

# Terminal 3
./calculate_tps.sh 9102 5 10

# Terminal 4
./calculate_tps.sh 9103 5 10
```

### Log to file:
```bash
./calculate_tps.sh 9103 5 10 | tee tps_node3_$(date +%Y%m%d_%H%M%S).log
```
