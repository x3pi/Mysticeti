#!/bin/bash

# Script để TIẾP TỤC Mixed System VỚI Node 4 Go Layer từ nơi dừng lại
# - KHÔNG xóa dữ liệu Go/Rust
# - KHÔNG generate keys mới
# - KHÔNG reset genesis timestamp
# - Giữ nguyên toàn bộ trạng thái blockchain
# - Chỉ dừng processes cũ và khởi động lại
#
# Giống run_keep_keys_with_node4_go.sh nhưng:
#   ✅ Giữ nguyên data directories
#   ✅ Giữ nguyên Rust storage
#   ✅ Giữ nguyên config/keys
#   ✅ Chỉ restart processes

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

echo ""
echo -e "${GREEN}═══════════════════════════════════════════════════════════════${NC}"
echo -e "${GREEN}  🔄 RESUME MIXED SYSTEM WITH NODE 4 GO LAYER${NC}"
echo -e "${GREEN}     (Giữ nguyên dữ liệu, tiếp tục từ nơi dừng lại)${NC}"
echo -e "${GREEN}═══════════════════════════════════════════════════════════════${NC}"
echo ""

# ==============================================================================
# Step 1: Verify data/config exist
# ==============================================================================
print_step "Bước 1: Kiểm tra dữ liệu và config..."

# Check Rust configs
for id in 0 1 2 3 4; do
    if [ ! -f "$METANODE_ROOT/config/node_$id.toml" ]; then
        print_error "config/node_$id.toml không tìm thấy!"
        print_error "Cần chạy run_keep_keys_with_node4_go.sh lần đầu để tạo configs."
        exit 1
    fi
done

# Check Go configs
GO_CONFIG_NODE4="$GO_PROJECT_ROOT/cmd/simple_chain/config-master-node4.json"
if [ ! -f "$GO_CONFIG_NODE4" ]; then
    print_error "config-master-node4.json không tìm thấy!"
    exit 1
fi

# Check binary
BINARY="$METANODE_ROOT/target/release/metanode"
if [ ! -f "$BINARY" ]; then
    print_info "Binary chưa có, build lại..."
    cd "$METANODE_ROOT"
    cargo build --release --bin metanode
fi

print_info "✅ Tất cả configs và binary tồn tại"

# ==============================================================================
# Step 2: Stop ALL Running Nodes (KHÔNG xóa dữ liệu)
# ==============================================================================
print_step "Bước 2: Dừng tất cả nodes cũ..."

# Stop all tmux sessions
tmux kill-session -t go-master 2>/dev/null || true
tmux kill-session -t go-sub 2>/dev/null || true
tmux kill-session -t go-master-1 2>/dev/null || true
tmux kill-session -t go-sub-1 2>/dev/null || true
tmux kill-session -t go-master-4 2>/dev/null || true
tmux kill-session -t go-sub-4 2>/dev/null || true
for i in 0 1 2 3 4; do
    tmux kill-session -t "metanode-$i" 2>/dev/null || true
done
tmux kill-session -t metanode-1-sep 2>/dev/null || true

# Force Kill Processes
print_info "🔪 Force killing any lingering processes..."
pkill -9 -f "metanode start" 2>/dev/null || true
pkill -9 -f "metanode run" 2>/dev/null || true

# Kill ports
kill_port() {
    local port=$1
    local pids=$(lsof -ti :$port 2>/dev/null || true)
    if [ -n "$pids" ]; then
        print_warn "Killing process on port $port: $pids"
        kill -9 $pids 2>/dev/null || true
    fi
}
kill_port 9000; kill_port 9001; kill_port 9002; kill_port 9003; kill_port 9004
kill_port 9011
kill_port 10747; kill_port 10000; kill_port 6201; kill_port 9081
kill_port 10646; kill_port 10001; kill_port 6200

sleep 2

# ==============================================================================
# Step 3: Clean ONLY sockets (NOT data)
# ==============================================================================
print_step "Bước 3: Xóa sockets cũ (giữ nguyên dữ liệu)..."
rm -f /tmp/metanode-tx-*.sock /tmp/executor*.sock /tmp/rust-go.sock_* 2>/dev/null || true
rm -f /tmp/rust-go-node1.sock /tmp/executor1.sock /tmp/executor1-sep.sock 2>/dev/null || true
rm -f /tmp/rust-go-node1-master.sock /tmp/rust-go-standard-master.sock 2>/dev/null || true
rm -f /tmp/executor4.sock /tmp/rust-go-node4-master.sock 2>/dev/null || true
print_info "✅ Đã xóa sockets cũ"

# Ensure log dir exists
mkdir -p "$LOG_DIR"

# ==============================================================================
# Step 4: Start Standard Go Processes
# ==============================================================================
print_step "Bước 4: Khởi động Go processes (Standard + Node 1)..."
cd "$GO_PROJECT_ROOT/cmd/simple_chain"

# 4.1 Start Standard Go Master
print_info "🚀 Starting Standard Go Master (go-master)..."
tmux new-session -d -s go-master -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/simple/data/data/xapian_node' && go run . -config=config-master.json 2>&1 | tee \"$LOG_DIR/go-master.log\""
sleep 5

# 4.2 Start Standard Go Sub
print_info "🚀 Starting Standard Go Sub (go-sub)..."
tmux new-session -d -s go-sub -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/simple/data-write/data/xapian_node' && go run . -config=config-sub-write.json 2>&1 | tee \"$LOG_DIR/go-sub.log\""
sleep 5

# 4.3 Start Node 1 Go Master
print_info "🚀 Starting Node 1 Go Master (go-master-1)..."
tmux new-session -d -s go-master-1 -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/node1/data/data/xapian_node' && go run . -config=config-master-node1.json 2>&1 | tee \"$LOG_DIR/go-master-1.log\""
sleep 5

# 4.4 Start Node 1 Go Sub
print_info "🚀 Starting Node 1 Go Sub (go-sub-1)..."
tmux new-session -d -s go-sub-1 -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/node1/data-write/data/xapian_node' && go run . -config=config-sub-node1.json 2>&1 | tee \"$LOG_DIR/go-sub-1.log\""
sleep 5

print_info "⏳ Waiting for Go nodes to stabilize (10s)..."
sleep 10

# ==============================================================================
# Step 5: Start Rust Nodes (Standard: 0, 2, 3)
# ==============================================================================
print_step "Bước 5: Khởi động Rust Consensus Nodes (Standard)..."
cd "$METANODE_ROOT"

for id in 0 2 3; do
    print_info "🚀 Starting Rust Node $id (Standard)..."
    tmux new-session -d -s "metanode-$id" -c "$METANODE_ROOT" \
        "export RUST_LOG=info,consensus_core=debug; $BINARY start --config config/node_$id.toml 2>&1 | tee \"$LOG_DIR/metanode-$id.log\""
    sleep 1
done

# Start Node 1 (Separate Config)
print_info "🚀 Starting Rust Node 1 (Separate)..."
tmux new-session -d -s "metanode-1-sep" -c "$METANODE_ROOT" \
    "export RUST_LOG=info,consensus_core=debug; $BINARY start --config config/node_1.toml 2>&1 | tee \"$LOG_DIR/metanode-1-sep.log\""

print_info "⏳ Waiting for Rust nodes to start (5s)..."
sleep 5

# ==============================================================================
# Step 6: Start Node 4 Go Layer
# ==============================================================================
print_step "Bước 6: Khởi động Go Master và Sub cho Node 4..."
cd "$GO_PROJECT_ROOT/cmd/simple_chain"

print_info "🚀 Starting Node 4 Go Master (go-master-4)..."
tmux new-session -d -s go-master-4 -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/node4/data/data/xapian_node' && go run . -config=config-master-node4.json 2>&1 | tee \"$LOG_DIR/go-master-4.log\""
sleep 3

print_info "🚀 Starting Node 4 Go Sub (go-sub-4)..."
tmux new-session -d -s go-sub-4 -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/node4/data-write/data/xapian_node' && go run . -config=config-sub-node4.json 2>&1 | tee \"$LOG_DIR/go-sub-4.log\""

print_info "⏳ Đợi Go Master và Sub 4 khởi động (5s)..."
sleep 5

# ==============================================================================
# Step 7: Start Rust Metanode Node 4
# ==============================================================================
print_step "Bước 7: Khởi động Rust Metanode Node 4..."
cd "$METANODE_ROOT"

print_info "🚀 Starting Rust Node 4..."
tmux new-session -d -s metanode-4 -c "$METANODE_ROOT" \
    "export RUST_LOG=info,consensus_core=debug; $BINARY start --config config/node_4.toml 2>&1 | tee \"$LOG_DIR/metanode-4.log\""

print_info "⏳ Đợi Rust Node 4 khởi động (3s)..."
sleep 3

# ==============================================================================
# Summary
# ==============================================================================
echo ""
echo -e "${GREEN}═══════════════════════════════════════════════════════════════${NC}"
echo -e "${GREEN}  🎉 MIXED SYSTEM WITH NODE 4 GO LAYER - RESUMED!${NC}"
echo -e "${GREEN}     (Dữ liệu được giữ nguyên, tiếp tục từ nơi dừng lại)${NC}"
echo -e "${GREEN}═══════════════════════════════════════════════════════════════${NC}"
echo ""
print_info "📊 Standard System (Nodes 0, 2, 3):"
print_info "  - Rust Nodes: tmux attach -t metanode-{0,2,3}"
print_info "  - Go Master:  tmux attach -t go-master"
print_info "  - Go Sub:     tmux attach -t go-sub"
echo ""
print_info "📊 Node 1 Separate System:"
print_info "  - Rust: tmux attach -t metanode-1-sep"
print_info "  - Go:   tmux attach -t go-master-1 / go-sub-1"
echo ""
print_info "📊 Node 4 với Go Layer riêng:"
print_info "  - Rust:      tmux attach -t metanode-4"
print_info "  - Go Master: tmux attach -t go-master-4"
print_info "  - Go Sub:    tmux attach -t go-sub-4"
echo ""
print_info "📁 Log files: $LOG_DIR/*.log"
echo ""
