#!/bin/bash

# Script để TIẾP TỤC Mixed System từ nơi dừng lại
# - KHÔNG xóa dữ liệu Go/Rust
# - KHÔNG generate keys mới
# - KHÔNG reset genesis timestamp
# - KHÔNG build lại binary (trừ khi chưa có)
# - Chỉ dừng processes cũ và khởi động lại
#
# Standard System: Nodes 0, 2, 3, 4 (Go Master/Sub standard, Rust Nodes standard)
# Special Node 1:  Node 1 Separate (Go Master/Sub separate, Rust Node separate ports)

set -e
set -o pipefail

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Paths
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
METANODE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
MYSTICETI_ROOT="$(cd "$METANODE_ROOT/.." && pwd)"
GO_PROJECT_ROOT="$(cd "$METANODE_ROOT/../.." && pwd)/mtn-simple-2025"
LOG_DIR="$METANODE_ROOT/logs"

print_info() { echo -e "${GREEN}ℹ️  $1${NC}"; }
print_warn() { echo -e "${YELLOW}⚠️  $1${NC}"; }
print_error() { echo -e "${RED}❌ $1${NC}"; }
print_step() { echo -e "${BLUE}📋 $1${NC}"; }

if [ ! -d "$GO_PROJECT_ROOT" ]; then
    print_error "Cannot find Go project at $GO_PROJECT_ROOT"
    exit 1
fi

echo ""
echo -e "${GREEN}═══════════════════════════════════════════════════════════════${NC}"
echo -e "${GREEN}  🔄 RESUME MIXED SYSTEM (giữ nguyên dữ liệu)${NC}"
echo -e "${GREEN}═══════════════════════════════════════════════════════════════${NC}"
echo ""

# ==============================================================================
# Step 1: Stop ALL Running Nodes (nhưng KHÔNG xóa dữ liệu)
# ==============================================================================
print_step "Bước 1: Dừng tất cả nodes cũ (giữ nguyên dữ liệu)..."

# Stop Standard Sessions
tmux kill-session -t go-master 2>/dev/null || true
tmux kill-session -t go-sub 2>/dev/null || true
for i in 0 1 2 3 4; do
    tmux kill-session -t "metanode-$i" 2>/dev/null || true
done

# Stop Separate Node 1 Sessions
tmux kill-session -t go-master-1 2>/dev/null || true
tmux kill-session -t go-sub-1 2>/dev/null || true
tmux kill-session -t metanode-1-sep 2>/dev/null || true

# Force Kill Ports and Processes
kill_port() {
    local port=$1
    local pids=$(lsof -ti :$port 2>/dev/null || true)
    if [ -n "$pids" ]; then
        print_warn "Killing process on port $port: $pids"
        kill -9 $pids 2>/dev/null || true
    fi
}

print_info "🔪 Force killing any lingering metanode processes..."
pkill -9 -f "metanode start" 2>/dev/null || true
pkill -9 -f "metanode run" 2>/dev/null || true

# Standard ports
kill_port 9000; kill_port 9001; kill_port 9002; kill_port 9003; kill_port 9004
# Node 1 Separate ports
kill_port 9011
kill_port 10747; kill_port 10000; kill_port 6201; kill_port 9081
kill_port 10646; kill_port 10001; kill_port 6200

sleep 2

# ==============================================================================
# Step 2: Clean ONLY sockets (NOT data)
# ==============================================================================
print_step "Bước 2: Xóa sockets cũ (giữ nguyên dữ liệu)..."
rm -f /tmp/metanode-tx-*.sock /tmp/executor*.sock /tmp/rust-go.sock_* 2>/dev/null || true
rm -f /tmp/rust-go-node1.sock /tmp/executor1.sock /tmp/executor1-sep.sock 2>/dev/null || true
rm -f /tmp/rust-go-node1-master.sock /tmp/rust-go-standard-master.sock 2>/dev/null || true
print_info "✅ Đã xóa sockets cũ, dữ liệu được giữ nguyên"

# ==============================================================================
# Step 3: Ensure binary exists (build nếu chưa có)
# ==============================================================================
print_step "Bước 3: Kiểm tra binary..."
cd "$METANODE_ROOT"
BINARY="$METANODE_ROOT/target/release/metanode"

if [ ! -f "$BINARY" ]; then
    print_info "Building Rust binary..."
    cargo build --release --bin metanode
else
    print_info "✅ Binary đã tồn tại: $BINARY"
fi

# ==============================================================================
# Step 4: Verify configs exist
# ==============================================================================
print_step "Bước 4: Kiểm tra configs..."

for id in 0 1 2 3 4; do
    if [ ! -f "config/node_$id.toml" ]; then
        print_error "config/node_$id.toml không tìm thấy! Cần chạy run_mixed_system.sh trước."
        exit 1
    fi
done
print_info "✅ Tất cả config files tồn tại"

# Ensure log dir exists
mkdir -p "$LOG_DIR"

# ==============================================================================
# Step 5: Start Go Processes
# ==============================================================================
print_step "Bước 5: Khởi động Go processes..."
cd "$GO_PROJECT_ROOT/cmd/simple_chain"

GENESIS_OUTPUT="$GO_PROJECT_ROOT/cmd/simple_chain/genesis.json"

# 5.1 Start Standard Go Master
print_info "🚀 Starting Standard Go Master (go-master)..."
tmux new-session -d -s go-master -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/simple/data/data/xapian_node' && go run . -config=config-master.json 2>&1 | tee \"$LOG_DIR/go-master.log\""
sleep 5

# 5.2 Start Standard Go Sub
print_info "🚀 Starting Standard Go Sub (go-sub)..."
tmux new-session -d -s go-sub -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/simple/data-write/data/xapian_node' && go run . -config=config-sub-write.json 2>&1 | tee \"$LOG_DIR/go-sub.log\""
sleep 5

# 5.3 Start Node 1 Go Master
print_info "🚀 Starting Node 1 Go Master (go-master-1)..."
tmux new-session -d -s go-master-1 -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/node1/data/data/xapian_node' && go run . -config=config-master-node1.json 2>&1 | tee \"$LOG_DIR/go-master-1.log\""
sleep 5

# 5.4 Start Node 1 Go Sub
print_info "🚀 Starting Node 1 Go Sub (go-sub-1)..."
tmux new-session -d -s go-sub-1 -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/node1/data-write/data/xapian_node' && go run . -config=config-sub-node1.json 2>&1 | tee \"$LOG_DIR/go-sub-1.log\""
sleep 5

print_info "⏳ Waiting for Go nodes to stabilize (10s)..."
sleep 10

# ==============================================================================
# Step 6: Start Rust Nodes
# ==============================================================================
print_step "Bước 6: Khởi động Rust Consensus Nodes..."
cd "$METANODE_ROOT"

# 6.1 Start Standard Nodes (0, 2, 3, 4)
for id in 0 2 3 4; do
    print_info "🚀 Starting Rust Node $id (Standard)..."
    tmux new-session -d -s "metanode-$id" -c "$METANODE_ROOT" \
        "export RUST_LOG=info,consensus_core=debug; $BINARY start --config config/node_$id.toml 2>&1 | tee \"$LOG_DIR/metanode-$id.log\""
    sleep 1
done

# 6.2 Start Node 1 (Separate Config)
print_info "🚀 Starting Rust Node 1 (Separate)..."
tmux new-session -d -s "metanode-1-sep" -c "$METANODE_ROOT" \
    "export RUST_LOG=info,consensus_core=debug; $BINARY start --config config/node_1.toml 2>&1 | tee \"$LOG_DIR/metanode-1-sep.log\""

print_info "⏳ Waiting for Rust nodes to start..."
sleep 5

# ==============================================================================
# Step 7: Summary
# ==============================================================================

echo ""
print_info "=========================================="
print_info "🎉 MIXED SYSTEM RESUMED SUCCESSFULLY!"
print_info "=========================================="
echo ""
print_info "📊 Standard System (Nodes 0, 2, 3, 4):"
print_info "  - Rust Node 0: tmux attach -t metanode-0"
print_info "  - Rust Node 2: tmux attach -t metanode-2"
print_info "  - Rust Node 3: tmux attach -t metanode-3"
print_info "  - Rust Node 4: tmux attach -t metanode-4 (Full Node)"
print_info "  - Go Master:   tmux attach -t go-master"
print_info "  - Go Sub:      tmux attach -t go-sub"
print_info ""
print_info "📊 Node 1 Separate System (Node 1):"
print_info "  - Rust: tmux attach -t metanode-1-sep"
print_info "  - Go:   tmux attach -t go-master-1 / go-sub-1"
echo ""
print_info "Log files in $LOG_DIR/*.log"
echo ""