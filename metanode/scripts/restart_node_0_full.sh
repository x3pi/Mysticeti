#!/bin/bash

# Script để khởi động lại Node 0 (Go Master + Go Sub + Rust Consensus Node)
# GIỮ NGUYÊN DỮ LIỆU CŨ, KHÔNG CLEANUP.
# Các ports:
# - Rust Node: 9000
# - Go Master: RPC, Listen, Conn (standard ports)
# - Go Sub: (write node)
# - Sockets: /tmp/executor0.sock, /tmp/rust-go-standard-master.sock

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

print_info() { echo -e "${GREEN}ℹ️  $1${NC}"; }
print_warn() { echo -e "${YELLOW}⚠️  $1${NC}"; }
print_error() { echo -e "${RED}❌ $1${NC}"; }
print_step() { echo -e "${BLUE}📋 $1${NC}"; }

# Check paths
if [ ! -d "$GO_PROJECT_ROOT" ]; then
    print_error "Cannot find Go project at $GO_PROJECT_ROOT"
    exit 1
fi

# Step 1: Cleanup sockets only (REQUIRED for restart)
print_step "Bước 1: Cleanup sockets cũ..."
rm -f /tmp/executor0.sock 2>/dev/null || true
rm -f /tmp/rust-go-standard-master.sock 2>/dev/null || true
rm -f /tmp/rust-go-standard-sub.sock 2>/dev/null || true
rm -f /tmp/metanode-tx-0.sock 2>/dev/null || true

# Step 2: Stop running Node 0 instances
print_step "Bước 2: Dừng Node 0 đang chạy (nếu có)..."
tmux kill-session -t go-master 2>/dev/null || true
tmux kill-session -t go-sub 2>/dev/null || true
tmux kill-session -t metanode-0 2>/dev/null || true

# Kill by ports
kill_port() {
    local port=$1
    local pids=$(lsof -ti :$port 2>/dev/null || true)
    if [ -n "$pids" ]; then
        print_warn "Killing process on port $port: $pids"
        kill -9 $pids 2>/dev/null || true
    fi
}
kill_port 9000     # Rust consensus
kill_port 9080     # RPC
kill_port 10100    # Metanode RPC

print_info "Đợi cleanup hoàn tất..."
sleep 2

# Step 3: Check Rust Binary
print_step "Bước 3: Kiểm tra Rust Binary..."
BINARY="$METANODE_ROOT/target/release/metanode"
if [ ! -f "$BINARY" ]; then
    print_error "Binary không tồn tại: $BINARY. Vui lòng build trước."
    exit 1
fi
print_info "✅ Binary tồn tại: $BINARY"

# Step 4: Ensure genesis
if [ ! -f "$GO_PROJECT_ROOT/cmd/simple_chain/genesis.json" ]; then
    print_error "Genesis.json not found!"
    exit 1
fi

# Step 5: Start Go Master Node 0
print_step "Bước 5: Restart Go Master Node 0..."
cd "$GO_PROJECT_ROOT/cmd/simple_chain"
tmux new-session -d -s go-master -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/simple/data/data/xapian_node' && go run . -config=config-master.json 2>&1 | tee -a /tmp/go-master.log"

print_info "⏳ Waiting for Go Master 0 to init (15 giây)..."
sleep 15

# Check if Go Master created the socket
if [ -S "/tmp/executor0.sock" ]; then
    print_info "✅ Go Master socket created: /tmp/executor0.sock"
else
    print_warn "⚠️ Socket /tmp/executor0.sock chưa tạo, đợi thêm..."
    sleep 5
fi

# Step 6: Start Go Sub Node 0 (Write node)
print_step "Bước 6: Restart Go Sub Node 0..."
tmux new-session -d -s go-sub -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/simple/data-write/data/xapian_node' && go run . -config=config-sub-write.json 2>&1 | tee -a /tmp/go-sub.log"

print_info "⏳ Waiting for Go Sub 0 to connect (5 giây)..."
sleep 5

# Step 7: Start Rust Node 0
print_step "Bước 7: Restart Rust Consensus Node 0..."
cd "$METANODE_ROOT"
tmux new-session -d -s metanode-0 -c "$METANODE_ROOT" \
    "$BINARY start --config config/node_0.toml 2>&1 | tee -a logs/metanode-0.log"

print_info "⏳ Waiting for Rust Node 0 to start (5 giây)..."
sleep 5

# Verify all services
print_step "Bước 8: Kiểm tra trạng thái..."
echo ""
if tmux has-session -t go-master 2>/dev/null; then
    print_info "✅ Go Master:    tmux attach -t go-master"
else
    print_error "❌ Go Master không chạy!"
fi

if tmux has-session -t go-sub 2>/dev/null; then
    print_info "✅ Go Sub:       tmux attach -t go-sub"
else
    print_error "❌ Go Sub không chạy!"
fi

if tmux has-session -t metanode-0 2>/dev/null; then
    print_info "✅ Rust Node 0:  tmux attach -t metanode-0"
else
    print_error "❌ Rust Node 0 không chạy!"
fi

echo ""
# Check sockets
if [ -S "/tmp/executor0.sock" ]; then
    print_info "✅ Socket: /tmp/executor0.sock"
else
    print_warn "⚠️ Socket /tmp/executor0.sock chưa tạo!"
fi

if [ -S "/tmp/rust-go-standard-master.sock" ]; then
    print_info "✅ Socket: /tmp/rust-go-standard-master.sock"
else
    print_warn "⚠️ Socket /tmp/rust-go-standard-master.sock chưa tạo!"
fi

echo ""

print_step "🎉 Node 0 System Restarted (Data Preserved)!"
