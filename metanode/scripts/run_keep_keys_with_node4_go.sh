#!/bin/bash

# Script để chạy Mixed System VỚI Node 4 có Go layer riêng
# - Standard System: Nodes 0, 2, 3 (Go Master/Sub standard, Rust Nodes standard)
# - Node 1: Go Master/Sub riêng, Rust Node riêng
# - Node 4: Go Master riêng, executor_commit_enabled = true
#
# Đây là extension của run_mixed_system.sh với Node 4 có Go layer riêng

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
echo -e "${GREEN}  🚀 MIXED SYSTEM WITH NODE 4 GO LAYER${NC}"
echo -e "${GREEN}═══════════════════════════════════════════════════════════════${NC}"
echo ""

# ==============================================================================
# Step 0: Clean ALL Go node data (KEEP config/keys, DELETE data)
# ==============================================================================
print_step "Bước 0: Xóa tất cả dữ liệu Go nodes (giữ config/keys)..."

# Clean ALL Go node data directories
print_info "🗑️  Xóa dữ liệu Go Standard (simple)..."
rm -rf "$GO_PROJECT_ROOT/cmd/simple_chain/sample/simple/data" 2>/dev/null || true
rm -rf "$GO_PROJECT_ROOT/cmd/simple_chain/sample/simple/data-write" 2>/dev/null || true
rm -rf "$GO_PROJECT_ROOT/cmd/simple_chain/sample/simple/back_up" 2>/dev/null || true

print_info "🗑️  Xóa dữ liệu Go Node 1..."
rm -rf "$GO_PROJECT_ROOT/cmd/simple_chain/sample/node1/data" 2>/dev/null || true
rm -rf "$GO_PROJECT_ROOT/cmd/simple_chain/sample/node1/data-write" 2>/dev/null || true
rm -rf "$GO_PROJECT_ROOT/cmd/simple_chain/sample/node1/back_up" 2>/dev/null || true

print_info "🗑️  Xóa dữ liệu Go Node 4..."
rm -rf "$GO_PROJECT_ROOT/cmd/simple_chain/sample/node4/data" 2>/dev/null || true
rm -rf "$GO_PROJECT_ROOT/cmd/simple_chain/sample/node4/data-write" 2>/dev/null || true
rm -rf "$GO_PROJECT_ROOT/cmd/simple_chain/sample/node4/back_up" 2>/dev/null || true
rm -rf "$GO_PROJECT_ROOT/cmd/simple_chain/sample/node4/back_up_write" 2>/dev/null || true

# Clean Rust storage data (but not keys)
print_info "🗑️  Xóa dữ liệu Rust storage..."
rm -rf "$METANODE_ROOT/config/storage" 2>/dev/null || true
mkdir -p "$METANODE_ROOT/config/storage"

# Recreate required directories
print_info "📁 Tạo lại thư mục dữ liệu..."
mkdir -p "$GO_PROJECT_ROOT/cmd/simple_chain/sample/simple/data/data/xapian_node"
mkdir -p "$GO_PROJECT_ROOT/cmd/simple_chain/sample/simple/data-write/data/xapian_node"
mkdir -p "$GO_PROJECT_ROOT/cmd/simple_chain/sample/node1/data/data/xapian_node"
mkdir -p "$GO_PROJECT_ROOT/cmd/simple_chain/sample/node1/data-write/data/xapian_node"
mkdir -p "$GO_PROJECT_ROOT/cmd/simple_chain/sample/node4/data/data/xapian_node"
mkdir -p "$GO_PROJECT_ROOT/cmd/simple_chain/sample/node4/data-write/data/xapian_node"

print_info "✅ Đã xóa tất cả dữ liệu Go/Rust, giữ nguyên config/keys"

# ==============================================================================
# Step 1: FORCE RESET Genesis Timestamp to Current Time
# ==============================================================================
print_step "Bước 0.5: Force reset genesis timestamp to current time..."

GENESIS_PATH="$GO_PROJECT_ROOT/cmd/simple_chain/genesis.json"
if [ -f "$GENESIS_PATH" ]; then
    python3 << 'PYTHON_EOF'
import json
import time

genesis_path = "/home/abc/chain-n/mtn-simple-2025/cmd/simple_chain/genesis.json"
current_ms = int(time.time() * 1000)

try:
    with open(genesis_path, 'r') as f:
        genesis = json.load(f)
    
    if 'config' not in genesis:
        genesis['config'] = {}
    
    old_ts = genesis['config'].get('epoch_timestamp_ms', 0)
    genesis['config']['epoch_timestamp_ms'] = current_ms
    
    with open(genesis_path, 'w') as f:
        json.dump(genesis, f, indent=2)
    
    print(f"   ✅ Reset epoch_timestamp_ms: {old_ts} -> {current_ms}")
except Exception as e:
    print(f"   ⚠️  Could not reset timestamp: {e}")
PYTHON_EOF
    print_info "✅ Genesis timestamp reset to current time"
else
    print_info "⚠️ genesis.json not found, will be created fresh"
fi

# Also reset backup file
rm -f /tmp/epoch_data_backup.json /tmp/epoch_data_backup_*.json 2>/dev/null || true
print_info "✅ Removed old epoch backup files"

# ==============================================================================
# Step 1: Run the main mixed system script with KEEP_KEYS
# ==============================================================================
print_step "Bước 1: Chạy Mixed System (giữ keys)..."
export KEEP_KEYS=1
"$SCRIPT_DIR/run_mixed_system.sh"

# Wait for system to stabilize
print_info "⏳ Đợi hệ thống ổn định (5s)..."
sleep 5

# ==============================================================================
# Step 2: Stop Node 4 Rust to reconfigure
# ==============================================================================
print_step "Bước 2: Dừng Node 4 để cấu hình lại..."
tmux kill-session -t metanode-4 2>/dev/null || true
sleep 2

# ==============================================================================
# Step 3: Update Node 4 config for dedicated Go layer
# ==============================================================================
print_step "Bước 3: Cập nhật cấu hình Node 4..."

NODE4_CONFIG="$METANODE_ROOT/config/node_4.toml"

if [ -f "$NODE4_CONFIG" ]; then
    # Enable executor_read and executor_commit
    sed -i 's|executor_read_enabled = false|executor_read_enabled = true|g' "$NODE4_CONFIG"
    sed -i 's|executor_commit_enabled = false|executor_commit_enabled = true|g' "$NODE4_CONFIG"
    
    # Update socket paths for Node 4 dedicated Go Master
    sed -i 's|executor_send_socket_path = ".*"|executor_send_socket_path = "/tmp/executor4.sock"|g' "$NODE4_CONFIG"
    sed -i 's|executor_receive_socket_path = ".*"|executor_receive_socket_path = "/tmp/rust-go-node4-master.sock"|g' "$NODE4_CONFIG"
    sed -i 's|peer_go_master_sockets = \[.*\]|peer_go_master_sockets = ["/tmp/rust-go-node4-master.sock"]|g' "$NODE4_CONFIG"
    
    print_info "✅ node_4.toml đã cập nhật:"
    grep -E "(executor_read_enabled|executor_commit_enabled|executor_send_socket_path|executor_receive_socket_path)" "$NODE4_CONFIG" | while read line; do
        echo "   $line"
    done
else
    print_error "node_4.toml không tìm thấy!"
    exit 1
fi

# ==============================================================================
# Step 4: Clean old Node 4 sockets and data directories
# ==============================================================================
print_step "Bước 4: Xóa dữ liệu cũ và chuẩn bị thư mục Node 4..."

rm -f /tmp/executor4.sock /tmp/rust-go-node4-master.sock 2>/dev/null || true

# CRITICAL: Clean old Node 4 Go data to prevent stale data issues
# Node 4 was at block 12633 but network reset to block ~0
print_info "🗑️  Xóa dữ liệu cũ của Go Node 4..."
rm -rf "$GO_PROJECT_ROOT/cmd/simple_chain/sample/node4/data" 2>/dev/null || true
rm -rf "$GO_PROJECT_ROOT/cmd/simple_chain/sample/node4/data-write" 2>/dev/null || true
rm -rf "$GO_PROJECT_ROOT/cmd/simple_chain/sample/node4/back_up" 2>/dev/null || true
rm -rf "$GO_PROJECT_ROOT/cmd/simple_chain/sample/node4/back_up_write" 2>/dev/null || true

# Recreate directories
mkdir -p "$GO_PROJECT_ROOT/cmd/simple_chain/sample/node4/data/data/xapian_node"
mkdir -p "$GO_PROJECT_ROOT/cmd/simple_chain/sample/node4/data-write/data/xapian_node"
mkdir -p "$GO_PROJECT_ROOT/cmd/simple_chain/sample/node4/back_up"
mkdir -p "$GO_PROJECT_ROOT/cmd/simple_chain/sample/node4/back_up_write"
print_info "✅ Đã tạo lại thư mục dữ liệu Node 4"

# ==============================================================================
# Step 5: Check Go config for Node 4
# ==============================================================================
print_step "Bước 5: Kiểm tra Go config cho Node 4..."

GO_CONFIG_NODE4="$GO_PROJECT_ROOT/cmd/simple_chain/config-master-node4.json"

if [ ! -f "$GO_CONFIG_NODE4" ]; then
    print_error "config-master-node4.json không tìm thấy!"
    print_error "Vui lòng tạo file $GO_CONFIG_NODE4 trước"
    exit 1
fi

print_info "✅ Found Go config: $GO_CONFIG_NODE4"

# ==============================================================================
# Step 6: Start Go Master and Sub for Node 4
# ==============================================================================
print_step "Bước 6: Khởi động Go Master và Sub cho Node 4..."

cd "$GO_PROJECT_ROOT/cmd/simple_chain"

# Kill any existing go-master-4 and go-sub-4 sessions
tmux kill-session -t go-master-4 2>/dev/null || true
tmux kill-session -t go-sub-4 2>/dev/null || true

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
BINARY="$METANODE_ROOT/target/release/metanode"

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
echo -e "${GREEN}  🎉 MIXED SYSTEM WITH NODE 4 GO LAYER - STARTED!${NC}"
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
print_info "  - executor_read_enabled:   TRUE"
print_info "  - executor_commit_enabled: TRUE"
echo ""
print_info "📁 Log files: $LOG_DIR/*.log"
echo ""
print_info "📝 Để đăng ký Node 4 làm validator:"
print_info "  cd $GO_PROJECT_ROOT/cmd/tool/register_validator"
print_info "  go run ."
echo ""
