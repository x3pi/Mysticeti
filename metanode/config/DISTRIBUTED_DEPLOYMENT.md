# Distributed Deployment Example - 3 Nodes on Different Machines

## Topology
- Machine 1 (192.168.1.100): Node 0 
- Machine 2 (192.168.1.101): Node 1
- Machine 3 (192.168.1.102): Node 2

## Key Principle

**Local Communication (Rust ↔ Go on same machine): Unix Socket**
**Peer-to-Peer (between different machines): TCP Socket**

---

## Machine 1 - Node 0 Config

```toml
node_id = 0
network_address = "192.168.1.100:9000"

# Local Rust ↔ Go: Unix sockets (same machine)
executor_send_socket_path = "/tmp/executor0.sock"
executor_receive_socket_path = "/tmp/rust-go-master.sock"

# Peer Discovery: TCP to OTHER nodes
peer_go_master_sockets = [
    "tcp://192.168.1.101:19201",  # Node 1 Go Master
    "tcp://192.168.1.102:19202",  # Node 2 Go Master
]
peer_rpc_port = 19200  # This node's Go Master TCP port for peer queries
```

**Go Master:** Must bind to `tcp://0.0.0.0:19200` để accept requests từ peers

---

## Machine 2 - Node 1 Config

```toml
node_id = 1
network_address = "192.168.1.101:9001"

# Local Rust ↔ Go: Unix sockets
executor_send_socket_path = "/tmp/executor1.sock"
executor_receive_socket_path = "/tmp/rust-go-master.sock"

# Peer Discovery: TCP to OTHER nodes
peer_go_master_sockets = [
    "tcp://192.168.1.100:19200",  # Node 0
    "tcp://192.168.1.102:19202",  # Node 2
]
peer_rpc_port = 19201
```

---

## Machine 3 - Node 2 Config

```toml
node_id = 2
network_address = "192.168.1.102:9002"

# Local Rust ↔ Go: Unix sockets
executor_send_socket_path = "/tmp/executor2.sock"
executor_receive_socket_path = "/tmp/rust-go-master.sock"

# Peer Discovery: TCP to OTHER nodes
peer_go_master_sockets = [
    "tcp://192.168.1.100:19200",  # Node 0
    "tcp://192.168.1.101:19201",  # Node 1
]
peer_rpc_port = 19202
```

---

## Go Master Setup

Each Go Master needs to listen on TCP for peer queries:

### Update cmd/simple_chain/main.go

```go
// Current (Unix only):
requestSockPath := "/tmp/rust-go-master.sock"

// Add TCP listener for peer queries (NEW):
peerListenerSockPath := "tcp://0.0.0.0:19200"  // Use node's peer_rpc_port
```

You need **TWO listeners** on Go side:
1. **Unix socket** - For local Rust node
2. **TCP socket** - For remote Rust nodes (peer discovery)

---

## Firewall Configuration

On each machine:
```bash
# Allow peer discovery ports (19200, 19201, 19202)
sudo ufw allow from 192.168.1.0/24 to any port 19200:19202 proto tcp

# Allow consensus P2P (9000, 9001, 9002)
sudo ufw allow from 192.168.1.0/24 to any port 9000:9002 proto tcp
```

---

## Summary

✅ **Rust ↔ Go (local):** Unix socket - unchanged  
✅ **Peer Discovery (cross-machine):** TCP socket - `tcp://IP:PORT`  
✅ **Go Master:** Needs 2 listeners (Unix for local, TCP for peers)  
✅ **Config:** Just change `peer_go_master_sockets` to TCP format
