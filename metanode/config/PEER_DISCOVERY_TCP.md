# Peer-to-Peer Communication via TCP Sockets

## TL;DR

✅ **DONE!** Peer discovery (`peer_go_master_sockets`) đã hỗ trợ TCP socket!

**Nguyên tắc:**
- **Local communication** (Rust ↔ Go trên cùng máy): **Unix socket** 
- **Peer discovery** (giữa các nodes khác nhau): **TCP socket**

---

## Current Implementation Status

### ✅ Code Ready
- Rust `ExecutorClient` đã support TCP (via `SocketAddress::parse()`)
- Go socket abstraction đã có
- Peer discovery (`query_peer_epochs()`) tự động dùng TCP khi config TCP socket

### Config Format

#### Local Deployment (current - all nodes on same machine)
```toml
peer_go_master_sockets = ["/tmp/rust-go-standard-master.sock"]
```

#### Distributed Deployment (nodes on different machines)
```toml
peer_go_master_sockets = [
    "tcp://192.168.1.101:19201",  # Node 1
    "tcp://192.168.1.102:19202",  # Node 2  
]
```

---

## How It Works

### Scenario: 3 Nodes on 3 Machines

**Machine 1 (192.168.1.100) - Node 0:**
```toml
# Local Rust-Go communication
executor_send_socket_path = "/tmp/executor0.sock"
executor_receive_socket_path = "/tmp/rust-go-master.sock"

# Peer discovery to OTHER nodes
peer_go_master_sockets = [
    "tcp://192.168.1.101:19201",  # Node 1
    "tcp://192.168.1.102:19202",  # Node 2
]
peer_rpc_port = 19200  # This node listens on 19200 for peer queries
```

**Machine 2 (192.168.1.101) - Node 1:**
```toml
executor_send_socket_path = "/tmp/executor1.sock"
executor_receive_socket_path = "/tmp/rust-go-master.sock"

peer_go_master_sockets = [
    "tcp://192.168.1.100:19200",  # Node 0
    "tcp://192.168.1.102:19202",  # Node 2
]
peer_rpc_port = 19201
```

**Machine 3 (192.168.1.102) - Node 2:**
```toml
executor_send_socket_path = "/tmp/executor2.sock"
executor_receive_socket_path = "/tmp/rust-go-master.sock"

peer_go_master_sockets = [
    "tcp://192.168.1.100:19200",  # Node 0
    "tcp://192.168.1.101:19201",  # Node 1
]
peer_rpc_port = 19202
```

---

## Go Master Configuration

**CRITICAL:** Go Master cần 2 listeners:

1. **Unix socket** - For local Rust node
2. **TCP socket** - For peer discovery from remote nodes

### Update Required in `cmd/simple_chain/main.go`

```go
// Current: Only Unix socket for local Rust
requestSockPath := "/tmp/rust-go-master.sock"

// NEEDED: Add TCP listener for peer queries
// Option 1: Update existing socket to dual-mode (complex)
// Option 2: Start TWO servers (recommended)

// Listener 1: Unix socket for local Rust
go executor.RunSocketExecutor("/tmp/rust-go-master.sock", ...)

// Listener 2: TCP socket for peer discovery
go executor.RunSocketExecutor("tcp://0.0.0.0:19200", ...)  // Use peer_rpc_port
```

**Why 2 listeners?**
- Unix socket: High performance for local Rust ↔ Go
- TCP socket: Enable remote nodes to query this node's Go Master

---

## Code Flow

### Peer Discovery Process

1. **Node 0 starts** and needs to check if it's behind the network

2. **`CatchupManager::check_sync_status()`** is called

3. **Queries `peer_go_master_sockets`** (line 240 in catchup.rs):
   ```rust
   for peer_socket in peer_sockets {
       let peer_client = ExecutorClient::new(
           true, false, 
           String::new(), 
           peer_socket.clone(),  // "tcp://192.168.1.101:19201"
           None
       );
       let epoch = peer_client.get_current_epoch().await?;
   }
   ```

4. **`SocketAddress::parse()` detects TCP** from `peer_socket`:
   ```rust
   // "tcp://192.168.1.101:19201" → SocketAddress::Tcp(192.168.1.101:19201)
   ```

5. **`SocketStream::connect()` establishes TCP connection**

6. **Node 0 queries Node 1's Go Master** for current epoch/block

7. **Compares** and decides if catchup is needed

---

## Files Modified

### Config Files
1. ✅ `node_0.toml` - Added peer discovery TCP example
2. ✅ `node_1.toml` - Added peer discovery TCP example
3. ✅ `node_network_example.toml` - Full distributed example
4. ✅ `DISTRIBUTED_DEPLOYMENT.md` - Comprehensive guide

### Code Files (Already Done in Phase 1 & 2!)
- ✅ `executor_client.rs` - Socket abstraction with TCP support
- ✅ `catchup.rs` - Uses ExecutorClient for peer queries → auto TCP support!

---

## Testing

### Test Peer Discovery with TCP

**Step 1: Start Go Master with TCP listener**
```bash
# On Machine 1
# Update main.go to listen on tcp://0.0.0.0:19200
./simple_chain
```

**Step 2: Update Node 1 config**
```toml
peer_go_master_sockets = ["tcp://192.168.1.100:19200"]
```

**Step 3: Start Node 1 and check logs**
```bash
cargo run --release --config config/node_1.toml
```

**Expected logs:**
```
🔍 [PEER EPOCH] Querying 1 peer Go Masters for epoch discovery...
📊 [PEER EPOCH] Peer Go Master (tcp://192.168.1.100:19200): epoch=5, block=1234
✅ [PEER EPOCH] New best peer candidate: epoch=5 block=1234 from tcp://192.168.1.100:19200
```

---

## Firewall Rules

```bash
# Allow peer discovery ports
sudo ufw allow from 192.168.1.0/24 to any port 19200:19202 proto tcp

# Allow consensus P2P
sudo ufw allow from 192.168.1.0/24 to any port 9000:9002 proto tcp
```

---

## Summary

✅ **Rust code**: Already supports TCP for peer discovery  
✅ **Config updated**: Examples added for both local & distributed  
⚠️ **Go Master**: Needs TCP listener addition (2nd socket server)  
📝 **Documentation**: Complete deployment guide available  

**Next Step:** Update Go Master to add TCP listener for `peer_rpc_port`
