#!/bin/bash

# Script khởi động tất cả nodes theo thứ tự sử dụng individual scripts
# Tương tự run_full_system.sh nhưng sử dụng các scripts riêng biệt
# Hữu ích cho debugging và monitoring từng bước
# ⚠️  KHÔNG xóa dữ liệu cũ - giữ nguyên state hiện tại
#
# ✅ THỨ TỰ KHỞI ĐỘNG (QUAN TRỌNG):
# 1. go-master: Go Master Node (init genesis)
# 2. metanode-0/1/2/3/4: Rust Consensus Nodes
# 3. go-sub: Go Sub Node (cần Rust nodes để connect)
#
# ✅ TẤT CẢ NODES CHẠY TRONG TMUX SESSIONS:
# - go-master: Go Master Node
# - metanode-0: Rust Validator 0 (có executor)
# - metanode-1: Rust Validator 1
# - metanode-2: Rust Validator 2
# - metanode-3: Rust Validator 3
# - metanode-4: Rust Sync-Only Node
# - go-sub: Go Sub Node
#
# Cách chạy bằng tmux wrapper (khuyên dùng):
# ./start_mysticeti_in_tmux.sh
#
# Cách chạy thủ công:
# tmux new-session -d -s mysticeti-startup -c /home/abc/chain-n/Mysticeti/metanode/scripts
# tmux send-keys -t mysticeti-startup './run_all_individual.sh' C-m
# tmux attach -t mysticeti-startup
#
# Hoặc chạy trực tiếp:
# ./run_all_individual.sh

set -e
set -o pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Get script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

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

print_success() {
    echo -e "${GREEN}✅ $1${NC}"
}

echo ""
print_info "=========================================="
print_info "🚀 Khởi động Mysticeti System (Individual Scripts)"
print_info "=========================================="
echo ""

print_warn "⚠️  CHÚ Ý: Scripts này KHÔNG xóa dữ liệu cũ!"
print_warn "   - Giữ nguyên genesis.json và validator keys hiện có"
print_warn "   - Giữ nguyên sample data và blocks"
print_warn "   - Consensus sẽ tự động sync từ last commit index"
echo ""

# Step 1: Go Master Node
print_step "Bước 1: Kiểm tra/khởi động Go Master Node..."

# Check if Go Master is already running
if tmux has-session -t go-master 2>/dev/null; then
    print_info "✅ Go Master Node đã đang chạy - bỏ qua khởi động"
    print_info "💡 Nếu muốn restart, hãy kill session trước: tmux kill-session -t go-master"
else
    print_info "🚀 Go Master Node chưa chạy - khởi động..."
    if bash "$SCRIPT_DIR/run_go_master.sh"; then
        print_success "Go Master Node đã khởi động thành công"
    else
        print_error "Lỗi khi khởi động Go Master Node"
        exit 1
    fi
fi

echo ""

# Step 2: Skip Go Sub for now (will start after Rust nodes)

# Step 3: Wait for Go Master to be ready (Go Sub will start after Rust nodes)
print_step "Bước 3: Đợi Go Master sẵn sàng..."
print_info "⏳ Đợi 15 giây để Go Master hoàn toàn sẵn sàng..."
sleep 15

# Verify Go Master is still running
print_info "🔍 Checking Go Master tmux session..."
if tmux has-session -t go-master 2>/dev/null; then
    print_info "✅ Go Master tmux session is running"
else
    print_error "❌ Go Master tmux session not found!"
    exit 1
fi

print_success "Go Master đã sẵn sàng (Go Sub sẽ khởi động sau Rust nodes)"
echo ""

# Step 4: Rust Consensus Nodes
print_step "Bước 4: Khởi động Rust Consensus Nodes..."
print_info "🔍 Starting Rust node initialization..."

# Node 0 (with executor)
print_info "🚀 Kiểm tra/khởi động Node 0 (Validator + Executor)..."

# Check if Node 0 is already running
if tmux has-session -t metanode-0 2>/dev/null; then
    print_info "✅ Node 0 đã đang chạy - bỏ qua khởi động"
    print_info "💡 Nếu muốn restart, hãy kill session trước: tmux kill-session -t metanode-0"
else
    print_info "🚀 Node 0 chưa chạy - khởi động..."

    # Temporarily disable strict error checking for node startup
    set +e
    set +o pipefail

    # Kill existing session (just in case)
    tmux kill-session -t metanode-0 2>/dev/null || true
    sleep 1

    # Start Node 0 directly
    print_info "Creating tmux session 'metanode-0'..."
    cd "$METANODE_ROOT"
    if tmux new-session -d -s metanode-0 -c "$METANODE_ROOT" "$METANODE_ROOT/target/release/metanode start --config config/node_0.toml 2>&1 | tee $METANODE_ROOT/logs/latest/node_0.log" 2>/dev/null; then
        print_success "Node 0 đã khởi động thành công"
    else
        print_error "Lỗi khi khởi động Node 0"
        print_info "🔍 Checking if session was created despite error..."
        if tmux has-session -t metanode-0 2>/dev/null; then
            print_info "✅ Session exists, Node 0 may be running"
        else
            print_info "❌ Session not found"
        fi
    fi

    # Re-enable strict error checking
    set -e
    set -o pipefail
fi

echo ""
sleep 3  # Wait between node startups

# Node 1
print_info "🚀 Khởi động Node 1 (Validator)..."

# Temporarily disable strict error checking for node startup
set +e
set +o pipefail

# Kill existing session
tmux kill-session -t metanode-1 2>/dev/null || true
sleep 1

# Start Node 1 directly
print_info "Creating tmux session 'metanode-1'..."
cd "$METANODE_ROOT"
if tmux new-session -d -s metanode-1 -c "$METANODE_ROOT" "$METANODE_ROOT/target/release/metanode start --config config/node_1.toml 2>&1 | tee $METANODE_ROOT/logs/latest/node_1.log" 2>/dev/null; then
    print_success "Node 1 đã khởi động thành công"
else
    print_error "Lỗi khi khởi động Node 1"
    print_info "🔍 Checking if session was created despite error..."
    if tmux has-session -t metanode-1 2>/dev/null; then
        print_info "✅ Session exists, Node 1 may be running"
    else
        print_info "❌ Session not found"
    fi
fi

# Re-enable strict error checking
set -e
set -o pipefail

echo ""
sleep 3  # Wait between node startups

# Node 2
print_info "🚀 Khởi động Node 2 (Validator)..."

# Temporarily disable strict error checking for node startup
set +e
set +o pipefail

# Kill existing session
tmux kill-session -t metanode-2 2>/dev/null || true
sleep 1

# Start Node 2 directly
print_info "Creating tmux session 'metanode-2'..."
cd "$METANODE_ROOT"
if tmux new-session -d -s metanode-2 -c "$METANODE_ROOT" "$METANODE_ROOT/target/release/metanode start --config config/node_2.toml 2>&1 | tee $METANODE_ROOT/logs/latest/node_2.log" 2>/dev/null; then
    print_success "Node 2 đã khởi động thành công"
else
    print_error "Lỗi khi khởi động Node 2"
    print_info "🔍 Checking if session was created despite error..."
    if tmux has-session -t metanode-2 2>/dev/null; then
        print_info "✅ Session exists, Node 2 may be running"
    else
        print_info "❌ Session not found"
    fi
fi

# Re-enable strict error checking
set -e
set -o pipefail

echo ""
sleep 3  # Wait between node startups

# Node 3
print_info "🚀 Khởi động Node 3 (Validator)..."

# Temporarily disable strict error checking for node startup
set +e
set +o pipefail

# Kill existing session
tmux kill-session -t metanode-3 2>/dev/null || true
sleep 1

# Start Node 3 directly
print_info "Creating tmux session 'metanode-3'..."
cd "$METANODE_ROOT"
if tmux new-session -d -s metanode-3 -c "$METANODE_ROOT" "$METANODE_ROOT/target/release/metanode start --config config/node_3.toml 2>&1 | tee $METANODE_ROOT/logs/latest/node_3.log" 2>/dev/null; then
    print_success "Node 3 đã khởi động thành công"
else
    print_error "Lỗi khi khởi động Node 3"
    print_info "🔍 Checking if session was created despite error..."
    if tmux has-session -t metanode-3 2>/dev/null; then
        print_info "✅ Session exists, Node 3 may be running"
    else
        print_info "❌ Session not found"
    fi
fi

# Re-enable strict error checking
set -e
set -o pipefail

echo ""
sleep 3  # Wait between node startups

# Node 4 (Sync-Only)
print_info "🚀 Khởi động Node 4 (Sync-Only)..."

# Temporarily disable strict error checking for node startup
set +e
set +o pipefail

# Kill existing session
tmux kill-session -t metanode-4 2>/dev/null || true
sleep 1

# Start Node 4 directly
print_info "Creating tmux session 'metanode-4'..."
cd "$METANODE_ROOT"
if tmux new-session -d -s metanode-4 -c "$METANODE_ROOT" "$METANODE_ROOT/target/release/metanode start --config config/node_4.toml 2>&1 | tee $METANODE_ROOT/logs/latest/node_4.log" 2>/dev/null; then
    print_success "Node 4 đã khởi động thành công"
else
    print_error "Lỗi khi khởi động Node 4"
    print_info "🔍 Checking if session was created despite error..."
    if tmux has-session -t metanode-4 2>/dev/null; then
        print_info "✅ Session exists, Node 4 may be running"
    else
        print_info "❌ Session not found"
    fi
fi

# Re-enable strict error checking
set -e
set -o pipefail

echo ""
print_info "🔍 Rust node initialization completed, moving to final verification..."

# Step 4.5: Start Go Sub Node (now that Rust nodes are running)
print_step "Bước 4.5: Khởi động Go Sub Node (sau Rust nodes)..."

# Check if Go Sub is already running
if tmux has-session -t go-sub 2>/dev/null; then
    print_info "✅ Go Sub Node đã đang chạy - bỏ qua khởi động"
    print_info "💡 Nếu muốn restart, hãy kill session trước: tmux kill-session -t go-sub"
else
    print_info "🚀 Go Sub Node chưa chạy - khởi động..."
    if bash "$SCRIPT_DIR/run_go_sub.sh"; then
        print_success "Go Sub Node đã khởi động thành công"
else
    print_warn "⚠️  Go Sub script exited with error, but checking if it's actually running..."
    # Check if Go Sub tmux session exists despite script error
    if tmux has-session -t go-sub 2>/dev/null; then
        print_info "✅ Go Sub tmux session exists - khởi động thành công!"
    else
        print_error "❌ Go Sub tmux session not found"
        print_warn "⚠️  Go Sub failed nhưng system vẫn có thể hoạt động với Go Master"
    fi
fi
fi

echo ""

# Step 5: Final verification
print_step "Bước 5: Kiểm tra hệ thống..."

# Count running nodes
RUST_NODES=$(ps aux | grep metanode | grep -v grep | wc -l || true)
GO_MASTER=$(tmux has-session -t go-master 2>/dev/null && echo "1" || echo "0")
GO_SUB=$(tmux has-session -t go-sub 2>/dev/null && echo "1" || echo "0")

echo ""
print_info "=========================================="
print_success "🎉 Hệ thống đã được khởi động!"
print_info "=========================================="
echo ""
print_info "📊 Trạng thái cuối cùng:"
print_info "  - Go Master Node: $([ "$GO_MASTER" = "1" ] && echo "✅ Running" || echo "❌ Stopped")"
print_info "  - Go Sub Node: $([ "$GO_SUB" = "1" ] && echo "✅ Running" || echo "❌ Stopped")"
print_info "  - Rust Consensus Nodes: $RUST_NODES/5 $([ "$RUST_NODES" -eq 5 ] && echo "✅" || echo "⚠️")"
echo ""
print_info "📺 Xem logs:"
print_info "  - Go Master: tmux attach -t go-master"
print_info "  - Go Sub: tmux attach -t go-sub"
print_info "  - Node 0: tmux attach -t metanode-0"
print_info "  - Node 1: tmux attach -t metanode-1"
print_info "  - Node 2: tmux attach -t metanode-2"
print_info "  - Node 3: tmux attach -t metanode-3"
print_info "  - Node 4: tmux attach -t metanode-4"
echo ""
print_info "🛑 Dừng hệ thống:"
print_info "  bash $SCRIPT_DIR/stop_full_system.sh"
echo ""
print_info "📝 Scripts individual:"
print_info "  - run_go_master.sh, run_go_sub.sh"
print_info "  - run_node_0.sh, run_node_1.sh, run_node_2.sh, run_node_3.sh, run_node_4.sh"
echo ""

# Final check
if [ "$GO_MASTER" = "1" ] && [ "$GO_SUB" = "1" ] && [ "$RUST_NODES" -eq 5 ]; then
    print_success "🎉 TẤT CẢ NODES ĐÃ KHỞI ĐỘNG THÀNH CÔNG!"
    echo ""
    print_info "💡 Hệ thống sẽ bắt đầu tạo blocks và consensus rounds"
    print_info "   Monitor logs để xem hoạt động của hệ thống"
else
    print_warn "⚠️  Một số nodes có thể chưa sẵn sàng"
    print_warn "   Kiểm tra logs và khởi động lại nếu cần"
fi