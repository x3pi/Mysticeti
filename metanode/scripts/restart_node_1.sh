#!/bin/bash

# Script để khởi động lại Rust Consensus Node 1 (Validator)
# GIỮ NGUYÊN DỮ LIỆU CŨ, KHÔNG CLEANUP.
# - Node 1: Validator (không có executor)
# - Sử dụng config/node_1.toml
# - Chạy trong tmux session 'metanode-1'

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

# Node configuration
NODE_ID=1
PORT=9001
TMUX_SESSION="metanode-$NODE_ID"
CONFIG_FILE="$METANODE_ROOT/config/node_$NODE_ID.toml"
LOG_FILE="$METANODE_ROOT/logs/latest/node_$NODE_ID.log"

print_info() { echo -e "${GREEN}ℹ️  $1${NC}"; }
print_warn() { echo -e "${YELLOW}⚠️  $1${NC}"; }
print_error() { echo -e "${RED}❌ $1${NC}"; }
print_step() { echo -e "${BLUE}📋 $1${NC}"; }

# Step 1: Check Binary
BINARY="$METANODE_ROOT/target/release/metanode"
if [ ! -f "$BINARY" ]; then
    print_error "❌ Binary không tồn tại: $BINARY"
    exit 1
fi

# Step 2: Stop running instances
print_step "Bước 2: Dừng Node $NODE_ID đang chạy (nếu có)..."
tmux kill-session -t "$TMUX_SESSION" 2>/dev/null || true

# Kill by port
PIDS=$(lsof -ti :$PORT 2>/dev/null || true)
if [ -n "$PIDS" ]; then
    print_info "🔴 Killing processes on port $PORT: $PIDS"
    kill -9 $PIDS 2>/dev/null || true
    sleep 1
fi

# Step 3: Cleanup sockets only
print_step "Bước 3: Cleanup sockets cũ..."
rm -f "/tmp/metanode-tx-$NODE_ID.sock" 2>/dev/null || true
rm -f "/tmp/executor$NODE_ID.sock" 2>/dev/null || true

# Step 4: Start Node
print_step "Bước 4: Restart Rust Consensus Node $NODE_ID..."
cd "$METANODE_ROOT" || exit 1

# Start in tmux
tmux new-session -d -s "$TMUX_SESSION" -c "$METANODE_ROOT" \
    "$BINARY start --config $CONFIG_FILE 2>&1 | tee -a $LOG_FILE"

print_info "⏳ Đợi Node $NODE_ID khởi động (5 giây)..."
sleep 5

# Verify
if tmux has-session -t "$TMUX_SESSION" 2>/dev/null; then
    print_info "✅ Rust Consensus Node $NODE_ID đã khởi động lại thành công!"
    print_info "📺 Xem logs: tmux attach -t $TMUX_SESSION"
else
    print_error "❌ Lỗi khởi động Node $NODE_ID. Kiểm tra logs: $LOG_FILE"
    exit 1
fi
