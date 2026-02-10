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

# Helper: Wait for a Unix socket file to exist (indicates Go process is ready)
wait_for_socket() {
    local socket=$1
    local name=$2
    local timeout=${3:-120}
    local start=$(date +%s)
    while true; do
        if [ -S "$socket" ]; then
            local elapsed=$(( $(date +%s) - start ))
            print_info "✅ $name ready: $socket (${elapsed}s)"
            return 0
        fi
        local elapsed=$(( $(date +%s) - start ))
        if [ $elapsed -ge $timeout ]; then
            print_info "⚠️ Timeout waiting for $name: $socket (${timeout}s). Continuing anyway..."
            return 1
        fi
        sleep 1
    done
}

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

# ⚠️ CRITICAL: Send SIGTERM FIRST to trigger graceful LevelDB flush
# tmux kill-session sends SIGHUP which Go doesn't handle → LevelDB data lost!
# Must SIGTERM before killing tmux so Go's App.Stop() → storageManager.CloseAll() runs.
print_info "📤 Sending SIGTERM to Go processes for graceful LevelDB flush..."
pkill -f "simple_chain" 2>/dev/null || true
pkill -f "metanode start" 2>/dev/null || true
pkill -f "metanode run" 2>/dev/null || true

print_info "⏳ Waiting 5s for LevelDB to flush WAL to disk..."
sleep 5  # Give Go time to run App.Stop() → CloseAll() → LevelDB Close()

# NOW kill tmux sessions (processes should already be dead from SIGTERM)
print_info "🗑️ Cleaning up tmux sessions..."
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

# Force kill any stragglers
print_info "🔪 Force killing any lingering processes..."
pkill -9 -f "simple_chain" 2>/dev/null || true
pkill -9 -f "metanode start" 2>/dev/null || true
pkill -9 -f "metanode run" 2>/dev/null || true

# Kill ports
kill_port() {
    local port=$1
    local pids=$(lsof -ti :$port 2>/dev/null || true)
    if [ -n "$pids" ]; then
        print_warn "Gracefully stopping process on port $port: $pids"
        kill $pids 2>/dev/null || true
        sleep 1
        # Force kill if still running
        local remaining=$(lsof -ti :$port 2>/dev/null || true)
        if [ -n "$remaining" ]; then
            kill -9 $remaining 2>/dev/null || true
        fi
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
mkdir -p "$LOG_DIR/node_0" "$LOG_DIR/node_1" "$LOG_DIR/node_2" "$LOG_DIR/node_3" "$LOG_DIR/node_4"

# ==============================================================================
# Step 4: Start Standard Go Processes
# ==============================================================================
print_step "Bước 4: Khởi động Go processes (Standard + Node 1)..."
cd "$GO_PROJECT_ROOT/cmd/simple_chain"

# 4.1 Start Standard Go Master
print_info "🚀 Starting Standard Go Master (go-master)..."
tmux new-session -d -s go-master -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/simple/data/data/xapian_node' && go run . -config=config-master.json >> \"$LOG_DIR/node_0/go-master-stdout.log\" 2>&1"

# 4.2 Start Standard Go Sub
print_info "🚀 Starting Standard Go Sub (go-sub)..."
tmux new-session -d -s go-sub -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/simple/data-write/data/xapian_node' && go run . -config=config-sub-write.json >> \"$LOG_DIR/node_0/go-sub-stdout.log\" 2>&1"

# 4.3 Start Node 1 Go Master
print_info "🚀 Starting Node 1 Go Master (go-master-1)..."
tmux new-session -d -s go-master-1 -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/node1/data/data/xapian_node' && go run . -config=config-master-node1.json >> \"$LOG_DIR/node_1/go-master-stdout.log\" 2>&1"

# 4.4 Start Node 1 Go Sub
print_info "🚀 Starting Node 1 Go Sub (go-sub-1)..."
tmux new-session -d -s go-sub-1 -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/node1/data-write/data/xapian_node' && go run . -config=config-sub-node1.json >> \"$LOG_DIR/node_1/go-sub-stdout.log\" 2>&1"

print_info "⏳ Waiting for Go Master executor sockets to be ready..."
wait_for_socket "/tmp/rust-go-standard-master.sock" "Go Master 0" 120
wait_for_socket "/tmp/rust-go-node1-master.sock" "Go Master 1" 120

# ==============================================================================
# Step 5: Start Rust Nodes (Standard: 0, 2, 3)
# ==============================================================================
print_step "Bước 5: Khởi động Rust Consensus Nodes (Standard)..."
cd "$METANODE_ROOT"

for id in 0 2 3; do
    print_info "🚀 Starting Rust Node $id (Standard)..."
    tmux new-session -d -s "metanode-$id" -c "$METANODE_ROOT" \
        "export RUST_LOG=info,consensus_core=debug; $BINARY start --config config/node_$id.toml >> \"$LOG_DIR/node_$id/rust.log\" 2>&1"
done

# Start Node 1 (Separate Config)
print_info "🚀 Starting Rust Node 1 (Separate)..."
tmux new-session -d -s "metanode-1-sep" -c "$METANODE_ROOT" \
    "export RUST_LOG=info,consensus_core=debug; $BINARY start --config config/node_1.toml >> \"$LOG_DIR/node_1/rust.log\" 2>&1"

print_info "⏳ Waiting for Rust nodes to start (5s)..."
sleep 5

# ==============================================================================
# Step 6: Start Node 4 Go Layer
# ==============================================================================
print_step "Bước 6: Khởi động Go Master và Sub cho Node 4..."
cd "$GO_PROJECT_ROOT/cmd/simple_chain"

print_info "🚀 Starting Node 4 Go Master (go-master-4)..."
tmux new-session -d -s go-master-4 -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/node4/data/data/xapian_node' && go run . -config=config-master-node4.json >> \"$LOG_DIR/node_4/go-master-stdout.log\" 2>&1"

print_info "🚀 Starting Node 4 Go Sub (go-sub-4)..."
tmux new-session -d -s go-sub-4 -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/node4/data-write/data/xapian_node' && go run . -config=config-sub-node4.json >> \"$LOG_DIR/node_4/go-sub-stdout.log\" 2>&1"

print_info "⏳ Waiting for Node 4 Go Master executor socket..."
wait_for_socket "/tmp/rust-go-node4-master.sock" "Go Master 4" 120

# ==============================================================================
# Step 7: Start Rust Metanode Node 4
# ==============================================================================
print_step "Bước 7: Khởi động Rust Metanode Node 4..."
cd "$METANODE_ROOT"

print_info "🚀 Starting Rust Node 4..."
tmux new-session -d -s metanode-4 -c "$METANODE_ROOT" \
    "export RUST_LOG=info,consensus_core=debug; $BINARY start --config config/node_4.toml >> \"$LOG_DIR/node_4/rust.log\" 2>&1"

print_info "⏳ Đợi Rust Node 4 khởi động (3s)..."
sleep 3

# ==============================================================================
# Step 8: Verify System Health
# ==============================================================================
print_step "Bước 8: Kiểm tra sức khỏe hệ thống..."

verify_system_health() {
    local all_ok=true
    echo ""
    echo -e "${BLUE}─── Internal Connection Readiness Check ───${NC}"

    # Check tmux sessions are alive
    local sessions=("metanode-0" "metanode-2" "metanode-3" "metanode-4" "metanode-1-sep" "go-master" "go-sub" "go-master-1" "go-sub-1" "go-master-4" "go-sub-4")
    local alive=0
    local dead=0
    for s in "${sessions[@]}"; do
        if tmux has-session -t "$s" 2>/dev/null; then
            alive=$((alive + 1))
        else
            dead=$((dead + 1))
            echo -e "  ${RED}✗ tmux session missing: $s${NC}"
            all_ok=false
        fi
    done
    echo -e "  ${GREEN}✓ tmux sessions: $alive alive, $dead missing${NC}"

    # Check executor sockets exist
    local sockets=("/tmp/rust-go-standard-master.sock" "/tmp/rust-go-node4-master.sock")
    for sock in "${sockets[@]}"; do
        if [ -S "$sock" ]; then
            echo -e "  ${GREEN}✓ Socket ready: $sock${NC}"
        else
            echo -e "  ${RED}✗ Socket missing: $sock${NC}"
            all_ok=false
        fi
    done

    # Check [READY] signals in logs (wait up to 30s for them to appear)
    echo -e "  ${YELLOW}⏳ Waiting for [READY] signals (up to 30s)...${NC}"
    local ready_timeout=30
    local ready_found=0
    local ready_targets=3  # Go Master, Go Sub, Rust
    for i in $(seq 1 $ready_timeout); do
        ready_found=0
        # Go Master [READY]
        if grep -q "\[READY\].*fully operational" "$LOG_DIR"/node_0/go-master-stdout.log 2>/dev/null || \
           grep -qr "\[READY\].*fully operational" "$GO_SIMPLE_ROOT"/sample/simple/data/data/logs/ 2>/dev/null; then
            ready_found=$((ready_found + 1))
        fi
        # Go Sub [READY]
        if grep -q "\[READY\].*Go Sub synced" "$LOG_DIR"/node_0/go-sub-stdout.log 2>/dev/null || \
           grep -qr "\[READY\].*Go Sub synced" "$GO_SIMPLE_ROOT"/sample/simple/data-write/data/logs/ 2>/dev/null; then
            ready_found=$((ready_found + 1))
        fi
        # Rust [READY]
        if grep -q "\[READY\].*Rust executor" "$LOG_DIR"/node_0/rust.log 2>/dev/null; then
            ready_found=$((ready_found + 1))
        fi

        if [ "$ready_found" -ge "$ready_targets" ]; then
            break
        fi
        sleep 1
    done

    if [ "$ready_found" -ge "$ready_targets" ]; then
        echo -e "  ${GREEN}✓ All [READY] signals received ($ready_found/$ready_targets)${NC}"
    else
        echo -e "  ${YELLOW}⚠ Only $ready_found/$ready_targets [READY] signals found (some components may still be starting)${NC}"
        all_ok=false
    fi

    echo -e "${BLUE}────────────────────────────────────────────${NC}"
    if $all_ok; then
        echo -e "  ${GREEN}🟢 System health: ALL OK${NC}"
    else
        echo -e "  ${YELLOW}🟡 System health: PARTIAL (check warnings above)${NC}"
    fi
    echo ""
}

# Run health check in background (non-blocking), display results after summary
verify_system_health &
HEALTH_PID=$!

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
print_info "📁 Log files: $LOG_DIR/node_N/"
print_info "🔍 Check readiness: grep '[READY]' $LOG_DIR/node_*/go-*-stdout.log $LOG_DIR/node_*/rust.log"
echo ""

# Wait for health check to complete
wait $HEALTH_PID 2>/dev/null

