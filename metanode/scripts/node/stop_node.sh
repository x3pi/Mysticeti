#!/bin/bash
# Usage: ./stop_node.sh <node_id>
# Gracefully stop a single node (Go Master + Go Sub + Rust Metanode)

set -e

NODE_ID="${1:?Usage: $0 <node_id> (0-4)}"

# Validate
if [[ ! "$NODE_ID" =~ ^[0-4]$ ]]; then
    echo "❌ Invalid node_id: $NODE_ID (must be 0-4)"
    exit 1
fi

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

echo -e "${GREEN}🛑 Stopping Node $NODE_ID...${NC}"

# ─── Config Maps ─────────────────────────────────────────────
GO_MASTER_SESSION=("go-master-0" "go-master-1" "go-master-2" "go-master-3" "go-master-4")
GO_SUB_SESSION=("go-sub-0" "go-sub-1" "go-sub-2" "go-sub-3" "go-sub-4")
RUST_SESSION=("metanode-0" "metanode-1" "metanode-2" "metanode-3" "metanode-4")

GO_MASTER_SOCKET=("/tmp/rust-go-node0-master.sock" "/tmp/rust-go-node1-master.sock" "/tmp/rust-go-node2-master.sock" "/tmp/rust-go-node3-master.sock" "/tmp/rust-go-node4-master.sock")
GO_SUB_SOCKET=("/tmp/rust-go-node0.sock" "/tmp/rust-go-node1.sock" "/tmp/rust-go-node2.sock" "/tmp/rust-go-node3.sock" "/tmp/rust-go-node4.sock")
EXECUTOR_SOCKET=("/tmp/executor0.sock" "/tmp/executor1.sock" "/tmp/executor2.sock" "/tmp/executor3.sock" "/tmp/executor4.sock")

GO_MASTER_CONFIG=("config-master-node0.json" "config-master-node1.json" "config-master-node2.json" "config-master-node3.json" "config-master-node4.json")

# ─── Step 1: SIGTERM Go processes (LevelDB flush) ────────────
# Find and SIGTERM the Go process for this node's config
GO_CFG="${GO_MASTER_CONFIG[$NODE_ID]}"
GO_PIDS=$(pgrep -f "$GO_CFG" 2>/dev/null || true)
if [ -n "$GO_PIDS" ]; then
    echo -e "${YELLOW}  📤 Sending SIGTERM to Go processes ($GO_CFG)...${NC}"
    kill $GO_PIDS 2>/dev/null || true
fi

# Also SIGTERM Rust
RUST_PIDS=$(pgrep -f "node_${NODE_ID}.toml" 2>/dev/null || true)
if [ -n "$RUST_PIDS" ]; then
    echo -e "${YELLOW}  📤 Sending SIGTERM to Rust metanode...${NC}"
    kill $RUST_PIDS 2>/dev/null || true
fi

# Wait for LevelDB flush
echo "  ⏳ Waiting 5s for graceful shutdown..."
sleep 5

# ─── Step 2: Kill tmux sessions ──────────────────────────────
for sess in "${GO_MASTER_SESSION[$NODE_ID]}" "${GO_SUB_SESSION[$NODE_ID]}" "${RUST_SESSION[$NODE_ID]}"; do
    tmux kill-session -t "$sess" 2>/dev/null || true
done

# ─── Step 3: Force kill stragglers ───────────────────────────
GO_PIDS=$(pgrep -f "$GO_CFG" 2>/dev/null || true)
if [ -n "$GO_PIDS" ]; then
    kill -9 $GO_PIDS 2>/dev/null || true
fi
RUST_PIDS=$(pgrep -f "node_${NODE_ID}.toml" 2>/dev/null || true)
if [ -n "$RUST_PIDS" ]; then
    kill -9 $RUST_PIDS 2>/dev/null || true
fi

# ─── Step 4: Clean sockets ───────────────────────────────────
rm -f "${GO_MASTER_SOCKET[$NODE_ID]}" "${GO_SUB_SOCKET[$NODE_ID]}" "${EXECUTOR_SOCKET[$NODE_ID]}" 2>/dev/null || true

echo -e "${GREEN}✅ Node $NODE_ID stopped.${NC}"
