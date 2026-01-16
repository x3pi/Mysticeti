#!/bin/bash

# Script khởi động Go Sub Node riêng biệt
# - Go Sub Node: Nhận blocks từ Go Master và xử lý write operations
# - Sử dụng config-sub-write.json
# - Chạy trong tmux session 'go-sub'
# - CẦN Go Master Node đã chạy trước
# - KHÔNG xóa dữ liệu cũ - giữ nguyên state hiện tại

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

# Verify paths
if [ ! -d "$GO_PROJECT_ROOT" ]; then
    print_error "Cannot find Go project at $GO_PROJECT_ROOT"
    print_error "Please ensure mtn-simple-2025 is at the same level as Mysticeti directory"
    exit 1
fi

if [ ! -f "$GO_PROJECT_ROOT/cmd/simple_chain/config-sub-write.json" ]; then
    print_error "Cannot find config-sub-write.json at $GO_PROJECT_ROOT/cmd/simple_chain/config-sub-write.json"
    exit 1
fi

# Step 1: Check if Go Master is running
print_step "Bước 1: Kiểm tra Go Master Node đã chạy..."

if ! ps aux | grep -q "[s]imple_chain.*config-master"; then
    print_error "❌ Go Master Node chưa chạy!"
    print_error "   Vui lòng khởi động Go Master Node trước:"
    print_error "   bash $SCRIPT_DIR/run_go_master.sh"
    exit 1
fi

print_info "✅ Go Master Node đang chạy"

# Step 2: Kill any existing Go Sub process
print_step "Bước 2: Dừng Go Sub Node đang chạy (nếu có)..."

# Kill tmux session
tmux kill-session -t go-sub 2>/dev/null || true

# Kill processes using go run with config-sub-write.json
pkill -f "go run.*config-sub-write.json" 2>/dev/null || true

# Kill processes using simple_chain with sub config
pkill -f "simple_chain.*config-sub-write.json" 2>/dev/null || true

sleep 2

# Step 3: Check data (KHÔNG xóa dữ liệu cũ của Go Sub Node)
print_step "Bước 3: Kiểm tra dữ liệu của Go Sub Node..."

# ⚠️  KHÔNG xóa dữ liệu cũ khi khởi động node riêng lẻ
# Chỉ kiểm tra data-write directory có tồn tại không
if [ -d "$GO_PROJECT_ROOT/cmd/simple_chain/sample/simple/data-write" ]; then
    print_info "✅ Data-write directory đã tồn tại - giữ nguyên dữ liệu cũ"
    print_info "💡 Nếu muốn xóa dữ liệu cũ, hãy dùng: rm -rf $GO_PROJECT_ROOT/cmd/simple_chain/sample/simple/data-write"
else
    print_warn "⚠️  Data-write directory không tồn tại"
    print_info "📁 Tạo cấu trúc thư mục data-write cơ bản..."
    mkdir -p "$GO_PROJECT_ROOT/cmd/simple_chain/sample/simple/data-write/data/xapian_node"
    print_info "  ✅ Đã tạo cấu trúc thư mục data-write cơ bản"
fi

# Step 4: Clean Go cache
print_step "Bước 4: Clean Go cache..."

cd "$GO_PROJECT_ROOT" || exit 1
print_info "Cleaning Go cache để đảm bảo code mới được compile..."
go clean -cache -testcache >/dev/null 2>&1 || true

# Step 5: Start Go Sub Node
print_step "Bước 5: Khởi động Go Sub Node..."

cd "$GO_PROJECT_ROOT/cmd/simple_chain" || exit 1

# Set environment variables
export GOTOOLCHAIN=go1.23.5
export XAPIAN_BASE_PATH='sample/simple/data-write/data/xapian_node'

print_info "🚀 Khởi động Go Sub Node (config-sub-write.json) trong tmux session 'go-sub'..."
print_info "Sử dụng 'go run' (không cần build binary)"

# Verify genesis.json exists
if [ ! -f "$GO_PROJECT_ROOT/cmd/simple_chain/genesis.json" ]; then
    print_error "❌ Không tìm thấy genesis.json tại $GO_PROJECT_ROOT/cmd/simple_chain/genesis.json"
    print_error "   Vui lòng đảm bảo Go Master đã tạo genesis.json"
    exit 1
fi

# Start Go Sub in tmux session (for reliability and monitoring)
print_info "Starting Go Sub in tmux session 'go-sub'..."
cd "$GO_PROJECT_ROOT/cmd/simple_chain"

# Kill existing session if any
tmux kill-session -t go-sub 2>/dev/null || true
sleep 1

# Create tmux session with Go Sub
if ! tmux new-session -d -s go-sub -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "cd '$GO_PROJECT_ROOT/cmd/simple_chain' && export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/simple/data-write/data/xapian_node' && go run . -config=config-sub-write.json 2>&1 | tee /tmp/go-sub.log"; then
    print_error "❌ Không thể tạo tmux session 'go-sub'"
    exit 1
fi

# Wait a bit for the session to start
sleep 3

# Verify Go Sub is running (either tmux session or process)
if tmux has-session -t go-sub 2>/dev/null; then
    print_info "✅ Go Sub tmux session is running"
elif ps aux | grep -q "[s]imple_chain.*config-sub-write"; then
    print_info "✅ Go Sub process is running (tmux session may have crashed)"
else
    print_error "❌ Go Sub không chạy được"
    print_info "   Kiểm tra log:"
    if [ -f "/tmp/go-sub.log" ]; then
        print_info "   - Log file: /tmp/go-sub.log"
        print_info "   - Last 20 lines:"
        tail -20 /tmp/go-sub.log 2>/dev/null || true
    fi
    exit 1
fi

# Wait a bit for Go Sub to initialize
print_info "⏳ Đợi Go Sub Node khởi động hoàn toàn (5 giây)..."
sleep 5

print_info "✅ Go Sub started in tmux session 'go-sub'"

print_info "⏳ Đợi Go Sub Node kết nối với Go Master (15 giây)..."
sleep 15

# Check Go Sub status after delay
if ps -p $GO_SUB_PID > /dev/null 2>&1; then
    print_info "✅ Go Sub Node vẫn đang chạy"
elif tmux has-session -t go-sub 2>/dev/null; then
    print_warn "⚠️  Go Sub process đã dừng nhưng tmux session còn tồn tại"
    print_info "   Có thể process bị restart hoặc crash nhẹ"
else
    print_error "❌ Go Sub Node đã dừng hoàn toàn"
    print_info "   Kiểm tra log:"
    if [ -f "/tmp/go-sub.log" ]; then
        print_info "   - Log file: /tmp/go-sub.log"
        print_info "   - Last 30 lines:"
        tail -30 /tmp/go-sub.log 2>/dev/null || true
    fi
    exit 1
fi

print_info "✅ Go Sub Node đã khởi động thành công!"
print_info "📺 Xem logs: tmux attach -t go-sub"
print_info "🛑 Dừng: tmux kill-session -t go-sub"

# Wait for connection to Go Master (don't fail if it doesn't connect immediately)
print_info "🔍 Kiểm tra Go Sub Node đã kết nối với Go Master..."
CONNECTION_CHECK=false
for i in {1..15}; do
    # Check if Go Sub is still running first
    if ! ps -p $GO_SUB_PID > /dev/null 2>&1 && ! tmux has-session -t go-sub 2>/dev/null; then
        print_warn "  ⚠️  Go Sub đã dừng trong quá trình kiểm tra kết nối"
        break
    fi

    if grep -qE "TCP kết nối thành công|KẾT NỐI ĐẾN MASTER HOÀN TẤT" /tmp/go-sub.log 2>/dev/null; then
        CONNECTION_CHECK=true
        print_info "  ✅ Go Sub Node đã kết nối với Go Master (sau $i giây)"
        break
    fi
    if [ $i -lt 15 ]; then
        sleep 1
    fi
done

if [ "$CONNECTION_CHECK" = false ]; then
    print_warn "  ⚠️  Không xác nhận được kết nối với Go Master ngay lập tức, nhưng sẽ tiếp tục..."
fi

# Note: Go Sub will try to connect to Rust nodes later when they start up
print_info "💡 Go Sub sẽ tự động kết nối với Go Master và Rust nodes khi chúng sẵn sàng"

print_info "🎉 Go Sub Node đã sẵn sàng!"
echo ""
print_info "=========================================="
print_info "📊 Trạng thái Go Sub Node:"
print_info "=========================================="
print_info "  - Tmux Session: go-sub ✅"
print_info "  - Config: config-sub-write.json ✅"
print_info "  - Connected to Go Master: $([ "$CONNECTION_CHECK" = true ] && echo "✅" || echo "⚠️")"
print_info "  - Log: /tmp/go-sub.log"
echo ""
print_info "📺 Commands:"
print_info "  - View logs: tmux attach -t go-sub"
print_info "  - Stop: tmux kill-session -t go-sub"
print_info "  - Check status: tmux has-session -t go-sub"
echo ""