#!/bin/bash

# Script khởi động Rust Consensus Node 4 riêng biệt
# - Node 4: Sync-Only Node (không tham gia validator ban đầu, chỉ đồng bộ data)
# - Sử dụng config/node_4.toml
# - Chạy trong tmux session 'metanode-4'
# - CẦN Go Master và Go Sub đã chạy trước

set -e
set -o pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Get script directory and change to project root
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Script is in scripts/, so metanode root is one level up
METANODE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# Mysticeti root is one level up from metanode
MYSTICETI_ROOT="$(cd "$METANODE_ROOT/.." && pwd)"
# Go project is at the same level as Mysticeti directory
GO_PROJECT_ROOT="$(cd "$METANODE_ROOT/../.." && pwd)/mtn-simple-2025"

# Node configuration
NODE_ID=4
PORT=9004
TMUX_SESSION="metanode-$NODE_ID"
CONFIG_FILE="$METANODE_ROOT/config/node_$NODE_ID.toml"
LOG_FILE="$METANODE_ROOT/logs/latest/node_$NODE_ID.log"

# Print colored messages
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

# Step 1: Check prerequisites
print_step "Bước 1: Kiểm tra điều kiện tiên quyết..."

# Check if Go nodes are running
if ! ps aux | grep -q "[s]imple_chain.*config-master"; then
    print_error "❌ Go Master Node chưa chạy!"
    print_error "   Vui lòng khởi động Go Master Node trước:"
    print_error "   bash $SCRIPT_DIR/run_go_master.sh"
    exit 1
fi

if ! ps aux | grep -q "[s]imple_chain.*config-sub-write"; then
    print_error "❌ Go Sub Node chưa chạy!"
    print_error "   Vui lòng khởi động Go Sub Node trước:"
    print_error "   bash $SCRIPT_DIR/run_go_sub.sh"
    exit 1
fi

print_info "✅ Go Master và Go Sub Nodes đang chạy"

# Check if binary exists
BINARY="$METANODE_ROOT/target/release/metanode"
if [ ! -f "$BINARY" ]; then
    print_error "❌ Binary không tồn tại: $BINARY"
    print_error "   Vui lòng build Rust project trước:"
    print_error "   cd $METANODE_ROOT && cargo build --release --bin metanode"
    exit 1
fi

# Check if config exists
if [ ! -f "$CONFIG_FILE" ]; then
    print_error "❌ Config file không tồn tại: $CONFIG_FILE"
    print_error "   Vui lòng tạo config trước bằng cách chạy:"
    print_error "   $BINARY generate --nodes 5 --output config"
    exit 1
fi

print_info "✅ Binary và config sẵn sàng"

# Step 2: Kill any existing Node process
print_step "Bước 2: Dừng Node $NODE_ID đang chạy (nếu có)..."

# Kill tmux session
tmux kill-session -t "$TMUX_SESSION" 2>/dev/null || true

# Kill processes using the port
PIDS=$(lsof -ti :$PORT 2>/dev/null || true)
if [ -n "$PIDS" ]; then
    print_info "🔴 Killing processes on port $PORT: $PIDS"
    for PID in $PIDS; do
        kill -9 "$PID" 2>/dev/null || true
    done
    sleep 1
fi

# Kill metanode processes for this node
pkill -f "metanode.*start.*--config.*node_$NODE_ID.toml" 2>/dev/null || true
ps aux | grep -E "[m]etanode.*node_$NODE_ID" | awk '{print $2}' | xargs -r kill -9 2>/dev/null || true

sleep 2

# Step 3: Clean up old logs and data
print_step "Bước 3: Xóa logs và data cũ của Node $NODE_ID..."

# Clean Rust logs for this node
if [ -d "$METANODE_ROOT/logs" ]; then
    rm -f "$LOG_FILE" 2>/dev/null || true
    print_info "✅ Đã xóa log cũ của node $NODE_ID"
fi

# Clean Unix sockets for this node
rm -f "/tmp/metanode-tx-$NODE_ID.sock" 2>/dev/null || true
rm -f "/tmp/executor$NODE_ID.sock" 2>/dev/null || true
print_info "✅ Đã xóa Unix sockets cũ của node $NODE_ID"

# Step 4: Start Node
print_step "Bước 4: Khởi động Rust Consensus Node $NODE_ID..."

cd "$METANODE_ROOT" || exit 1

print_info "🚀 Khởi động Node $NODE_ID (Sync-Only Node) trong tmux session '$TMUX_SESSION'..."

# Start in tmux
print_info "Creating tmux session '$TMUX_SESSION'..."
if ! tmux new-session -d -s "$TMUX_SESSION" -c "$METANODE_ROOT" \
    "$BINARY start --config $CONFIG_FILE 2>&1 | tee $METANODE_ROOT/logs/latest/node_4.log" 2>/dev/null; then
    print_error "❌ Không thể tạo tmux session '$TMUX_SESSION'"
    print_info "Checking if session was created despite error..."
    if tmux has-session -t "$TMUX_SESSION" 2>/dev/null; then
        print_info "✅ Session exists, continuing..."
    else
        exit 1
    fi
fi

# Wait a bit for the session to start
sleep 3

# Verify Node is running
if ! tmux has-session -t "$TMUX_SESSION" 2>/dev/null; then
    print_error "❌ Tmux session '$TMUX_SESSION' không tồn tại sau khi khởi động"
    print_error "   Có thể Node $NODE_ID đã crash ngay sau khi khởi động"
    print_info "   Kiểm tra log:"
    if [ -f "$LOG_FILE" ]; then
        print_info "   - Log file: $LOG_FILE"
        print_info "   - Last 20 lines:"
        tail -20 "$LOG_FILE" 2>/dev/null || true
    fi
    print_info "   - Hoặc kiểm tra tmux: tmux attach -t $TMUX_SESSION"
    exit 1
fi

print_info "⏳ Đợi Node $NODE_ID khởi động hoàn toàn (10 giây)..."
sleep 10

# Verify Node is still running
if ! tmux has-session -t "$TMUX_SESSION" 2>/dev/null; then
    print_error "❌ Node $NODE_ID đã dừng sau khi khởi động (có thể crash)"
    print_info "   Kiểm tra log:"
    if [ -f "$LOG_FILE" ]; then
        print_info "   - Log file: $LOG_FILE"
        print_info "   - Last 30 lines:"
        tail -30 "$LOG_FILE" 2>/dev/null || true
    fi
    print_info "   - Hoặc kiểm tra: tmux attach -t $TMUX_SESSION"
    exit 1
fi

print_info "✅ Rust Consensus Node $NODE_ID đã khởi động thành công!"
print_info "📺 Xem logs: tmux attach -t $TMUX_SESSION"
print_info "🛑 Dừng: tmux kill-session -t $TMUX_SESSION"

# Check if sockets are created
print_info "🔍 Kiểm tra sockets đã được tạo..."
if [ -S "/tmp/metanode-tx-$NODE_ID.sock" ]; then
    print_info "  ✅ Transaction socket: /tmp/metanode-tx-$NODE_ID.sock"
else
    print_warn "  ⚠️  Transaction socket chưa sẵn sàng"
fi

print_info "🎉 Node $NODE_ID đã sẵn sàng!"
echo ""
print_info "=========================================="
print_info "📊 Trạng thái Rust Consensus Node $NODE_ID:"
print_info "=========================================="
print_info "  - Tmux Session: $TMUX_SESSION ✅"
print_info "  - Config: $CONFIG_FILE ✅"
print_info "  - Port: $PORT ✅"
print_info "  - Mode: Sync-Only ✅"
print_info "  - Executor: disabled ✅"
print_info "  - Log: $LOG_FILE"
echo ""
print_info "📺 Commands:"
print_info "  - View logs: tmux attach -t $TMUX_SESSION"
print_info "  - Stop: tmux kill-session -t $TMUX_SESSION"
print_info "  - Check status: tmux has-session -t $TMUX_SESSION"
print_info "  - Check port: lsof -i :$PORT"
echo ""