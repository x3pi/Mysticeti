#!/bin/bash

# Script để khởi động Node 4 với Go execution layer riêng
# Node 4 chạy ở chế độ SyncOnly với executor_read_enabled và executor_commit_enabled = true
#
# Script này:
# 1. Tạo thư mục dữ liệu cho Node 4
# 2. Cập nhật node_4.toml với executor enabled
# 3. Khởi động Go Master cho Node 4
# 4. Khởi động Rust Metanode Node 4

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

# ==============================================================================
# Step 1: Cleanup and Prepare Node 4 Data Directories
# ==============================================================================
print_step "Bước 1: Chuẩn bị thư mục dữ liệu Node 4..."

# Kill existing sessions
tmux kill-session -t go-master-4 2>/dev/null || true
tmux kill-session -t metanode-4 2>/dev/null || true

# Kill port 9004 if in use
kill_port() {
    local port=$1
    local pids=$(lsof -ti :$port 2>/dev/null || true)
    if [ -n "$pids" ]; then
        print_warn "Killing process on port $port: $pids"
        kill -9 $pids 2>/dev/null || true
    fi
}
kill_port 9004
kill_port 10748

# Clean old sockets
rm -f /tmp/executor4.sock /tmp/rust-go-node4-master.sock 2>/dev/null || true

# Create Node 4 data directories
print_info "🗂️  Creating Node 4 data directories..."
mkdir -p "$GO_PROJECT_ROOT/cmd/simple_chain/sample/node4/data/data/xapian_node"
mkdir -p "$GO_PROJECT_ROOT/cmd/simple_chain/sample/node4/data-write/data/xapian_node"
mkdir -p "$GO_PROJECT_ROOT/cmd/simple_chain/sample/node4/back_up"
mkdir -p "$METANODE_ROOT/config/storage/node_4"
mkdir -p "$LOG_DIR"

# ==============================================================================
# Step 2: Update node_4.toml Configuration
# ==============================================================================
print_step "Bước 2: Cập nhật cấu hình node_4.toml..."

NODE4_CONFIG="$METANODE_ROOT/config/node_4.toml"

if [ -f "$NODE4_CONFIG" ]; then
    # Enable executor_read and executor_commit
    sed -i 's|executor_read_enabled = false|executor_read_enabled = true|g' "$NODE4_CONFIG"
    sed -i 's|executor_commit_enabled = false|executor_commit_enabled = true|g' "$NODE4_CONFIG"
    
    # Update socket paths for Node 4 dedicated Go Master
    sed -i 's|executor_send_socket_path = ".*"|executor_send_socket_path = "/tmp/executor4.sock"|g' "$NODE4_CONFIG"
    sed -i 's|executor_receive_socket_path = ".*"|executor_receive_socket_path = "/tmp/rust-go-node4-master.sock"|g' "$NODE4_CONFIG"
    
    # Verify changes
    print_info "✅ node_4.toml configuration updated:"
    grep -E "(executor_read_enabled|executor_commit_enabled|executor_send_socket_path|executor_receive_socket_path)" "$NODE4_CONFIG" | while read line; do
        echo "   $line"
    done
else
    print_error "node_4.toml not found at $NODE4_CONFIG"
    exit 1
fi

# ==============================================================================
# Step 3: Check/Create Go Config for Node 4
# ==============================================================================
print_step "Bước 3: Kiểm tra Go config cho Node 4..."

GO_CONFIG_NODE4="$GO_PROJECT_ROOT/cmd/simple_chain/config-master-node4.json"

if [ ! -f "$GO_CONFIG_NODE4" ]; then
    print_error "config-master-node4.json not found!"
    print_info "Please create $GO_CONFIG_NODE4 first"
    exit 1
fi

print_info "✅ Found Go config: $GO_CONFIG_NODE4"

# ==============================================================================
# Step 4: Build Rust Binary (if needed)
# ==============================================================================
print_step "Bước 4: Kiểm tra Rust binary..."

BINARY="$METANODE_ROOT/target/release/metanode"
if [ ! -f "$BINARY" ]; then
    print_info "Building Rust binary..."
    cd "$METANODE_ROOT"
    cargo build --release --bin metanode
fi
print_info "✅ Rust binary ready: $BINARY"

# ==============================================================================
# Step 5: Start Go Master for Node 4
# ==============================================================================
print_step "Bước 5: Khởi động Go Master cho Node 4..."

cd "$GO_PROJECT_ROOT/cmd/simple_chain"

print_info "🚀 Starting Node 4 Go Master (go-master-4)..."
tmux new-session -d -s go-master-4 -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/node4/data/data/xapian_node' && go run . -config=config-master-node4.json 2>&1 | tee \"$LOG_DIR/go-master-4.log\""

print_info "⏳ Waiting for Go Master 4 to start (5s)..."
sleep 5

# ==============================================================================
# Step 6: Start Rust Metanode Node 4
# ==============================================================================
print_step "Bước 6: Khởi động Rust Metanode Node 4..."

cd "$METANODE_ROOT"

print_info "🚀 Starting Rust Node 4..."
tmux new-session -d -s metanode-4 -c "$METANODE_ROOT" \
    "export RUST_LOG=info,consensus_core=debug; $BINARY start --config config/node_4.toml 2>&1 | tee \"$LOG_DIR/metanode-4.log\""

print_info "⏳ Waiting for Rust Node 4 to start (3s)..."
sleep 3

# ==============================================================================
# Summary
# ==============================================================================
echo ""
print_info "=========================================="
print_info "🎉 NODE 4 WITH GO LAYER STARTED!"
print_info "=========================================="
echo ""
print_info "📊 Node 4 Configuration:"
print_info "  - Executor Read:   ENABLED"
print_info "  - Executor Commit: ENABLED"
print_info "  - Initial Mode:    SyncOnly (chờ được thêm vào committee)"
echo ""
print_info "📺 Attach to sessions:"
print_info "  - Rust Node 4: tmux attach -t metanode-4"
print_info "  - Go Master 4: tmux attach -t go-master-4"
echo ""
print_info "📁 Log files:"
print_info "  - $LOG_DIR/metanode-4.log"
print_info "  - $LOG_DIR/go-master-4.log"
echo ""
print_info "📝 Để đăng ký Node 4 làm validator:"
print_info "  cd $GO_PROJECT_ROOT/cmd/tool/register_validator"
print_info "  go run ."
echo ""
