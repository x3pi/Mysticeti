#!/bin/bash

# Script để dừng toàn bộ hệ thống (Rust nodes + Go nodes)

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Get script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Script is in scripts/, so metanode root is one level up
METANODE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

print_info() {
    echo -e "${GREEN}ℹ️  $1${NC}"
}

print_warn() {
    echo -e "${YELLOW}⚠️  $1${NC}"
}

print_error() {
    echo -e "${RED}❌ $1${NC}"
}

print_step() {
    echo -e "${BLUE}📋 $1${NC}"
}

print_step "Dừng toàn bộ hệ thống..."

# Step 1: Stop Go nodes
print_step "Bước 1: Dừng Go nodes..."

# Stop Go Sub Node
if tmux has-session -t go-sub 2>/dev/null; then
    print_info "Dừng Go Sub Node..."
    tmux kill-session -t go-sub 2>/dev/null || true
    sleep 1
fi

# Stop Go Master Node
if tmux has-session -t go-master 2>/dev/null; then
    print_info "Dừng Go Master Node..."
    tmux kill-session -t go-master 2>/dev/null || true
    sleep 1
fi

# Kill any remaining Go processes
pkill -f "simple_chain.*config-sub-write" 2>/dev/null || true
pkill -f "simple_chain.*config-master" 2>/dev/null || true

print_info "✅ Đã dừng Go nodes"

# Step 2: Stop Rust nodes
print_step "Bước 2: Dừng Rust consensus nodes..."

cd "$METANODE_ROOT"

if [ -f "$METANODE_ROOT/scripts/node/stop_nodes.sh" ]; then
    print_info "Dừng Rust nodes..."
    bash "$METANODE_ROOT/scripts/node/stop_nodes.sh"
else
    print_warn "Không tìm thấy script stop_nodes.sh, dừng thủ công..."
    # Kill all metanode processes
    pkill -f "metanode.*start" 2>/dev/null || true
    # Kill tmux sessions
    for i in 0 1 2 3; do
        tmux kill-session -t "metanode-$i" 2>/dev/null || true
    done
fi

sleep 2

print_info "✅ Đã dừng Rust nodes"

# Step 3: Clean up sockets
print_step "Bước 3: Xóa sockets..."

rm -f /tmp/metanode-tx-*.sock
rm -f /tmp/executor*.sock
rm -f /tmp/rust-go.sock_*

print_info "✅ Đã xóa sockets"

print_info "=========================================="
print_info "✅ Đã dừng toàn bộ hệ thống"
print_info "=========================================="

