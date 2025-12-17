# RPC API Documentation

## Tổng quan

MetaNode cung cấp một HTTP RPC server đơn giản để submit transactions vào consensus network. Mỗi node có một RPC server riêng chạy trên port `metrics_port + 1000`.

## Endpoints

### POST /submit

Submit một transaction vào consensus network.

#### Request

**Method:** `POST`  
**Path:** `/submit`  
**Content-Type:** `text/plain` hoặc `application/octet-stream`

**Body:**
- Text data: Chuỗi text bất kỳ
- Hex data: Chuỗi hex (có hoặc không có prefix `0x`)

**Ví dụ với curl:**
```bash
# Submit text data
curl -X POST http://127.0.0.1:10100/submit \
  -H "Content-Type: text/plain" \
  -d "Hello, Blockchain!"

# Submit hex data
curl -X POST http://127.0.0.1:10100/submit \
  -H "Content-Type: text/plain" \
  -d "0x48656c6c6f2c20426c6f636b636861696e21"

# Submit raw bytes (hex encoded)
curl -X POST http://127.0.0.1:10100/submit \
  -H "Content-Type: text/plain" \
  -d "48656c6c6f2c20426c6f636b636861696e21"
```

#### Response

**Success (200 OK):**
```json
{
  "success": true,
  "tx_hash": "204d69c3943745b5",
  "block_ref": "B1106409([0],OhLiV2iSZBDx7VasA3akz6Erwp5FtymsC7DT5kREUxY=)",
  "indices": [0]
}
```

**Error (500 Internal Server Error):**
```json
{
  "success": false,
  "error": "Transaction submission failed: ..."
}
```

#### Response Fields

- `success`: `boolean` - Trạng thái thành công/thất bại
- `tx_hash`: `string` - Hash của transaction (8 bytes đầu của Blake2b256 hash, hex encoded)
- `block_ref`: `string` - Reference đến block chứa transaction
- `indices`: `array<number>` - Chỉ số của transaction trong block
- `error`: `string` - Thông báo lỗi (chỉ có khi success=false)

#### Retry Logic

RPC server có retry logic với exponential backoff:
- Retry tối đa: 10 lần
- Initial delay: 100ms
- Exponential backoff: 100ms, 200ms, 400ms, 800ms, ...

Điều này giúp xử lý trường hợp consensus authority chưa sẵn sàng ngay sau khi khởi động.

### GET /ready

Health check endpoint để kiểm tra RPC server đã sẵn sàng chưa.

#### Request

**Method:** `GET`  
**Path:** `/ready`

**Ví dụ:**
```bash
curl http://127.0.0.1:10100/ready
```

#### Response

**Success (200 OK):**
```json
{
  "ready": true
}
```

## Port Configuration

Mặc định, RPC port được tính như sau:
```
RPC Port = Metrics Port + 1000
```

Ví dụ:
- Node 0: Metrics port 9100 → RPC port 10100
- Node 1: Metrics port 9101 → RPC port 10101
- Node 2: Metrics port 9102 → RPC port 10102
- Node 3: Metrics port 9103 → RPC port 10103

## Transaction Processing

### Transaction Hash

Transaction hash được tính bằng Blake2b256:
- Hash toàn bộ transaction data
- Lấy 8 bytes đầu
- Encode thành hex string

**Ví dụ:**
```
Data: "Hello, Blockchain!"
Hash (full): 204d69c3943745b5e8f2a3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3
Hash (short): 204d69c3943745b5
```

### Transaction Submission Flow

```
1. Client → POST /submit
2. RPC Server → Parse request body
3. RPC Server → Calculate transaction hash
4. RPC Server → TransactionClient.submit()
5. Consensus → Add to transaction pool
6. Consensus → Include in block proposal
7. Consensus → Achieve consensus
8. Consensus → Commit
9. RPC Server → Return response with block_ref
```

### Block Reference Format

Block reference có format:
```
B<round>([<authority_index>],<digest>)
```

**Ví dụ:**
```
B1106409([0],OhLiV2iSZBDx7VasA3akz6Erwp5FtymsC7DT5kREUxY=)
```

- `B1106409`: Block ở round 1106409
- `[0]`: Được tạo bởi authority 0
- `OhLiV2iSZBDx7VasA3akz6Erwp5FtymsC7DT5kREUxY=`: Block digest (base64)

## Error Handling

### Common Errors

1. **Connection Refused**
   - RPC server chưa khởi động
   - Port bị chiếm
   - Firewall blocking

2. **Transaction Submission Failed**
   - Consensus authority chưa sẵn sàng
   - Transaction pool đầy
   - Network issues

3. **Timeout**
   - Consensus mất quá nhiều thời gian
   - Network latency cao

### Retry Strategy

Client nên implement retry logic:
```python
import requests
import time

def submit_transaction(data, endpoint, max_retries=5):
    for i in range(max_retries):
        try:
            response = requests.post(
                f"{endpoint}/submit",
                data=data,
                timeout=10
            )
            if response.status_code == 200:
                return response.json()
        except Exception as e:
            if i < max_retries - 1:
                time.sleep(2 ** i)  # Exponential backoff
            else:
                raise
    return None
```

## Client Libraries

### Python Example

```python
import requests
import json

class MetaNodeClient:
    def __init__(self, endpoint):
        self.endpoint = endpoint.rstrip('/')
    
    def submit(self, data):
        """Submit transaction data"""
        response = requests.post(
            f"{self.endpoint}/submit",
            data=data,
            headers={"Content-Type": "text/plain"},
            timeout=30
        )
        response.raise_for_status()
        return response.json()
    
    def is_ready(self):
        """Check if RPC server is ready"""
        try:
            response = requests.get(
                f"{self.endpoint}/ready",
                timeout=5
            )
            return response.status_code == 200
        except:
            return False

# Usage
client = MetaNodeClient("http://127.0.0.1:10100")
if client.is_ready():
    result = client.submit("Hello, Blockchain!")
    print(f"Transaction hash: {result['tx_hash']}")
    print(f"Block: {result['block_ref']}")
```

### JavaScript/Node.js Example

```javascript
const axios = require('axios');

class MetaNodeClient {
    constructor(endpoint) {
        this.endpoint = endpoint.replace(/\/$/, '');
    }
    
    async submit(data) {
        const response = await axios.post(
            `${this.endpoint}/submit`,
            data,
            {
                headers: { 'Content-Type': 'text/plain' },
                timeout: 30000
            }
        );
        return response.data;
    }
    
    async isReady() {
        try {
            const response = await axios.get(
                `${this.endpoint}/ready`,
                { timeout: 5000 }
            );
            return response.status === 200;
        } catch {
            return false;
        }
    }
}

// Usage
const client = new MetaNodeClient('http://127.0.0.1:10100');
if (await client.isReady()) {
    const result = await client.submit('Hello, Blockchain!');
    console.log(`Transaction hash: ${result.tx_hash}`);
    console.log(`Block: ${result.block_ref}`);
}
```

## Best Practices

1. **Always check /ready** trước khi submit
2. **Implement retry logic** với exponential backoff
3. **Handle timeouts** appropriately
4. **Log transaction hashes** để tracking
5. **Monitor response times** để detect issues

## Limitations

- RPC server là single-threaded (mỗi request được spawn trong tokio task)
- Không có rate limiting (có thể submit unlimited)
- Không có authentication (mọi người có thể submit)
- Response format đơn giản (không có pagination, filtering)

## Future Improvements

- [ ] Batch transaction submission
- [ ] Transaction status query endpoint
- [ ] Authentication/authorization
- [ ] Rate limiting
- [ ] WebSocket support for real-time updates
- [ ] GraphQL API

