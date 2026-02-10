#!/bin/bash

# Script để khởi động lại Node 1 (Go Master + Go Sub + Rust Consensus Node)
# GIỮ NGUYÊN DỮ LIỆU CŨ, KHÔNG CLEANUP.
# Các ports:
# - Rust Node: 9011
# - Go Master: RPC 10747, Listen 10000, Conn 6201
# - Go Sub: RPC 10646, Listen 10001, Conn 6200
# - Sockets: /tmp/executor1.sock (dùng socket cũ nếu có)

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

# Check paths
if [ ! -d "$GO_PROJECT_ROOT" ]; then
    print_error "Cannot find Go project at $GO_PROJECT_ROOT"
    exit 1
fi

# Step 1: Cleanup sockets only (REQUIRED for restart)
print_step "Bước 1: Cleanup sockets cũ..."
rm -f /tmp/rust-go-node1.sock 2>/dev/null || true
rm -f /tmp/executor1-sep.sock 2>/dev/null || true
rm -f /tmp/rust-go-node1-master.sock 2>/dev/null || true

# Step 2: Stop running Node 1 instances
print_step "Bước 2: Dừng Node 1 đang chạy (nếu có)..."
tmux kill-session -t go-master-1 2>/dev/null || true
tmux kill-session -t go-sub-1 2>/dev/null || true
tmux kill-session -t metanode-1-sep 2>/dev/null || true

# Kill by ports
kill_port() {
    local port=$1
    local pids=$(lsof -ti :$port 2>/dev/null || true)
    if [ -n "$pids" ]; then
        print_warn "Killing process on port $port: $pids"
        kill -9 $pids 2>/dev/null || true
    fi
}
kill_port 9011
kill_port 10747
kill_port 10000
kill_port 6201
kill_port 9081
kill_port 10646
kill_port 10001
kill_port 6200

# Step 3: Check Rust Binary
BINARY="$METANODE_ROOT/target/release/metanode"
if [ ! -f "$BINARY" ]; then
    print_error "Binary không tồn tại: $BINARY. Vui lòng build trước."
    exit 1
fi

# Step 4: Ensure genesis
if [ ! -f "$GO_PROJECT_ROOT/cmd/simple_chain/genesis.json" ]; then
    print_error "Genesis.json not found!"
    exit 1
fi

# Step 5: Start Go Master Node 1
print_step "Bước 5: Restart Go Master Node 1..."
cd "$GO_PROJECT_ROOT/cmd/simple_chain"
mkdir -p "$LOG_DIR/node_1"
tmux new-session -d -s go-master-1 -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/node1/data/data/xapian_node' && go run . -config=config-master-node1.json >> \"$LOG_DIR/node_1/go-master-stdout.log\" 2>&1"

print_info "Waiting for Go Master 1 to init..."
sleep 10

# Step 6: Start Go Sub Node 1
print_step "Bước 6: Restart Go Sub Node 1..."
tmux new-session -d -s go-sub-1 -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/node1/data-write/data/xapian_node' && go run . -config=config-sub-node1.json >> \"$LOG_DIR/node_1/go-sub-stdout.log\" 2>&1"

print_info "Waiting for Go Sub 1 to connect..."
sleep 5

# Step 7: Start Rust Node 1
print_step "Bước 7: Restart Rust Node 1..."
cd "$METANODE_ROOT"
tmux new-session -d -s metanode-1-sep -c "$METANODE_ROOT" \
    "$BINARY start --config config/node_1_separate.toml >> \"$LOG_DIR/node_1/rust.log\" 2>&1"

print_info "Waiting for Rust Node 1 to start..."
sleep 5

# Summary
print_step "🎉 Node 1 Separate System Restarted (Data Preserved)!"
print_info "  - Rust Node 1: tmux attach -t metanode-1-sep"
print_info "  - Go Master 1: tmux attach -t go-master-1"
print_info "  - Go Sub 1:    tmux attach -t go-sub-1"
