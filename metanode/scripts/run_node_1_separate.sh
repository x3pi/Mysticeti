#!/bin/bash

# Script để chạy riêng Node 1 với đầy đủ Go Master + Go Sub + Rust Consensus Node
# Các ports đã được đổi để tránh xung đột với hệ thống mặc định:
# - Rust Node: 9011
# - Go Master: RPC 10747, Listen 10000, Conn 6201
# - Go Sub: RPC 10646, Listen 10001, Conn 6200
# - Sockets: /tmp/executor1.sock, /tmp/rust-go-node1.sock

set -e
set -o pipefail

# Full clean switches
FULL_CLEAN_BUILD="${FULL_CLEAN_BUILD:-1}"
FULL_CLEAN_GO_MODCACHE="${FULL_CLEAN_GO_MODCACHE:-0}"

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

# Step 1: Clean Data specific to Node 1
print_step "Bước 1: Cleanup dữ liệu Node 1..."

# Clean sockets
rm -f /tmp/rust-go-node1.sock 2>/dev/null || true
rm -f /tmp/executor1.sock 2>/dev/null || true

# Clean Go Data for Node 1
GO_NODE1_DATA="$GO_PROJECT_ROOT/cmd/simple_chain/sample/node1"
if [ -d "$GO_NODE1_DATA" ]; then
    print_info "Cleaning $GO_NODE1_DATA..."
    rm -rf "$GO_NODE1_DATA"
fi

# Recreate structure
mkdir -p "$GO_NODE1_DATA/data/data/xapian_node"
mkdir -p "$GO_NODE1_DATA/data-write/data/xapian_node"
# Ensure blocks DB deleted
rm -rf "$GO_NODE1_DATA/data/data/blocks"
rm -rf "$GO_NODE1_DATA/data-write/data/blocks"

# Clean Rust storage for Node 1
RUST_NODE1_STORAGE="$METANODE_ROOT/config/storage/node_1_separate"
if [ -d "$RUST_NODE1_STORAGE" ]; then
    print_info "Cleaning Rust storage $RUST_NODE1_STORAGE..."
    rm -rf "$RUST_NODE1_STORAGE"
fi
mkdir -p "$(dirname "$RUST_NODE1_STORAGE")"

# Step 2: Stop running Node 1 instances
print_step "Bước 2: Dừng Node 1 cũ..."
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

# Step 3: Build Rust Binary (if needed - assumes generic build already done or doing it now)
# Reuse existing binary if possible to save time, unless forced
if [ "$FULL_CLEAN_BUILD" = "1" ] || [ ! -f "$METANODE_ROOT/target/release/metanode" ]; then
    print_step "Bước 3: Build Rust binary..."
    cd "$METANODE_ROOT"
    cargo build --release --bin metanode
fi
BINARY="$METANODE_ROOT/target/release/metanode"

# Step 4: Ensure genesis (Node 1 shares same genesis)
if [ ! -f "$GO_PROJECT_ROOT/cmd/simple_chain/genesis.json" ]; then
    print_error "Genesis.json not found! Run full system setup first to generate initial genesis/keys."
    exit 1
fi

# Step 5: Start Go Master Node 1
print_step "Bước 5: Start Go Master Node 1..."
cd "$GO_PROJECT_ROOT/cmd/simple_chain"
tmux new-session -d -s go-master-1 -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/node1/data/data/xapian_node' && go run . -config=config-master-node1.json 2>&1 | tee /tmp/go-master-1.log"

print_info "Waiting for Go Master 1 to init..."
sleep 15
# Optional: could add logic to check if started successfully

# Step 6: Start Go Sub Node 1
print_step "Bước 6: Start Go Sub Node 1..."
tmux new-session -d -s go-sub-1 -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/node1/data-write/data/xapian_node' && go run . -config=config-sub-node1.json 2>&1 | tee /tmp/go-sub-1.log"

print_info "Waiting for Go Sub 1 to connect..."
sleep 10

# Step 7: Start Rust Node 1
print_step "Bước 7: Start Rust Node 1..."
cd "$METANODE_ROOT"
tmux new-session -d -s metanode-1-sep -c "$METANODE_ROOT" \
    "$BINARY start --config config/node_1_separate.toml 2>&1 | tee /tmp/metanode-1-sep.log"

print_info "Waiting for Rust Node 1 to start..."
sleep 5

# Summary
print_step "🎉 Node 1 Separate System Started!"
print_info "  - Rust Node 1: tmux attach -t metanode-1-sep"
print_info "  - Go Master 1: tmux attach -t go-master-1"
print_info "  - Go Sub 1:    tmux attach -t go-sub-1"
print_info "  Logs:"
print_info "    /tmp/metanode-1-sep.log"
print_info "    /tmp/go-master-1.log"
print_info "    /tmp/go-sub-1.log"
