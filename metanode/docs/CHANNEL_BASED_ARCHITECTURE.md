# Channel-Based Architecture for Go-Sub to Rust Communication

## Phân tích kiến trúc hiện tại

### Vấn đề hiện tại

1. **Connection Pool (TCP)**: Go-sub sử dụng connection pool để gửi transactions
   - Mỗi transaction cần mượn connection từ pool
   - Connection được trả về pool ngay sau khi gửi
   - Race condition: Connection được tái sử dụng trước khi Rust đọc xong

2. **Rust RPC Server**: Nhận qua TCP với semaphore
   - Mỗi connection được spawn vào một tokio task
   - Timeout 5s để đọc length prefix
   - Nhiều connections timeout vì Go trả connection về pool quá nhanh

3. **Throughput bị giới hạn**: 
   - Delay 1ms để tránh race condition
   - Connection pool có thể exhausted
   - Không tận dụng được localhost speed

## Giải pháp đề xuất: Channel-Based với Multiple Workers

### Kiến trúc mới

```
Go-Sub Node:
┌─────────────────────────────────────────┐
│ Transaction Pool                        │
│ (batches of transactions)               │
└──────────────┬──────────────────────────┘
               │
               ▼
┌─────────────────────────────────────────┐
│ Buffered Channel (1000 batches)        │
│ chan []Transaction                      │
└──────────────┬──────────────────────────┘
               │
               ├──► Worker 1 ──┐
               ├──► Worker 2 ──┤
               ├──► Worker 3 ──┼──► UDS/TCP ──► Rust
               ├──► ...        │
               └──► Worker N ──┘
```

### Rust Side:

```
Rust RPC/UDS Server:
┌─────────────────────────────────────────┐
│ UDS Listener (Unix Domain Socket)      │
│ hoặc TCP Listener                       │
└──────────────┬──────────────────────────┘
               │
               ├──► Handler 1 ──┐
               ├──► Handler 2 ──┤
               ├──► Handler 3 ──┼──► Process & Submit
               ├──► ...        │
               └──► Handler N ──┘
```

## Implementation Plan

### 1. Go-Sub: Channel-Based Sender

**Thay đổi:**
- Tạo một buffered channel để queue transaction batches
- Nhiều worker goroutines đọc từ channel và gửi
- Mỗi worker có connection riêng (persistent connection)
- Fire-and-forget: không đợi response

**Lợi ích:**
- Go-sub có thể đùn liên tục vào channel
- Workers xử lý song song
- Không bị block bởi network I/O
- Tận dụng được localhost speed

### 2. Rust: UDS Server với Multiple Handlers

**Thay đổi:**
- Sử dụng Unix Domain Socket (UDS) thay vì TCP cho localhost
- Nhiều handlers xử lý song song
- Không cần response (fire-and-forget)
- Buffer lớn để nhận nhiều batches

**Lợi ích:**
- UDS nhanh hơn TCP trên localhost
- Ít overhead hơn TCP
- Xử lý song song hiệu quả

## Chi tiết Implementation

### Go-Sub: Channel-Based Architecture

```go
type ChannelBasedSender struct {
    // Buffered channel để queue batches
    batchChan chan []byte
    
    // Workers: nhiều goroutines đọc từ channel và gửi
    workers []*Worker
    
    // Connection pool cho mỗi worker (persistent)
    // Mỗi worker giữ một connection riêng
}

type Worker struct {
    id        int
    conn      net.Conn  // Persistent connection
    batchChan <-chan []byte
    done      chan struct{}
}

func (w *Worker) Start() {
    for {
        select {
        case batch := <-w.batchChan:
            // Gửi batch qua persistent connection
            w.sendBatch(batch)
        case <-w.done:
            return
        }
    }
}
```

### Rust: UDS Server với Multiple Handlers

```rust
pub struct UDSServer {
    transaction_client: Arc<dyn TransactionSubmitter>,
    socket_path: String,
    max_handlers: usize,
}

impl UDSServer {
    pub async fn start(self) -> Result<()> {
        // Listen on UDS
        let listener = UnixListener::bind(&self.socket_path)?;
        
        // Spawn multiple handlers
        for i in 0..self.max_handlers {
            let client = self.transaction_client.clone();
            tokio::spawn(async move {
                // Handler loop: nhận và xử lý batches
            });
        }
    }
}
```

## So sánh

| Aspect | Hiện tại (TCP Pool) | Đề xuất (Channel + UDS) |
|--------|---------------------|-------------------------|
| **Protocol** | TCP | UDS (localhost) |
| **Connection** | Pool (tái sử dụng) | Persistent per worker |
| **Concurrency** | Limited by pool size | Unlimited (channel buffer) |
| **Throughput** | Bị giới hạn bởi pool | Cao (nhiều workers) |
| **Latency** | 1ms delay cần thiết | Không cần delay |
| **Race condition** | Có (connection reuse) | Không (persistent conn) |
| **Complexity** | Trung bình | Thấp (channel pattern) |

## Migration Path

1. **Phase 1**: Implement channel-based sender trong Go
   - Tạo buffered channel
   - Implement workers
   - Giữ TCP connection (tạm thời)

2. **Phase 2**: Switch sang UDS
   - Implement UDS server trong Rust
   - Update Go client để dùng UDS
   - Test và benchmark

3. **Phase 3**: Optimize
   - Tune channel buffer size
   - Tune worker count
   - Remove delays

## Expected Performance

- **Throughput**: 10x-100x improvement (tùy vào số workers)
- **Latency**: Giảm từ ~1ms xuống <0.1ms
- **CPU**: Tăng nhẹ (nhiều goroutines) nhưng tận dụng được multi-core
- **Memory**: Tăng nhẹ (channel buffer) nhưng acceptable

