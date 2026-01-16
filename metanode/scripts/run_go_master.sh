#!/bin/bash

# Script khởi động Go Master Node riêng biệt
# - Go Master Node: Thực thi transactions và quản lý state
# - Sử dụng config-master.json
# - Chạy trong tmux session 'go-master'
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

if [ ! -f "$GO_PROJECT_ROOT/cmd/simple_chain/config-master.json" ]; then
    print_error "Cannot find config-master.json at $GO_PROJECT_ROOT/cmd/simple_chain/config-master.json"
    exit 1
fi

# Step 1: Kill any existing Go Master process
print_step "Bước 1: Dừng Go Master Node đang chạy (nếu có)..."

# Kill tmux session
tmux kill-session -t go-master 2>/dev/null || true

# Kill processes using go run with config-master.json
pkill -f "go run.*config-master.json" 2>/dev/null || true

# Kill processes using simple_chain with master config
pkill -f "simple_chain.*config-master.json" 2>/dev/null || true

sleep 2

# Step 2: Clean up old data (sample directory) - SKIP for individual startup
print_step "Bước 2: Kiểm tra dữ liệu (KHÔNG xóa khi khởi động riêng)..."

# ⚠️  KHÔNG xóa dữ liệu cũ khi khởi động node riêng lẻ
# Chỉ kiểm tra sample directory có tồn tại không
if [ -d "$GO_PROJECT_ROOT/cmd/simple_chain/sample" ]; then
    print_info "✅ Sample directory đã tồn tại - giữ nguyên dữ liệu cũ"
    print_info "💡 Nếu muốn xóa dữ liệu cũ, hãy dùng: rm -rf $GO_PROJECT_ROOT/cmd/simple_chain/sample"
else
    print_warn "⚠️  Sample directory không tồn tại"
    print_info "📁 Tạo cấu trúc thư mục sample cơ bản..."
    mkdir -p "$GO_PROJECT_ROOT/cmd/simple_chain/sample/simple/data/data/xapian_node"
    print_info "  ✅ Đã tạo cấu trúc thư mục sample cơ bản"
fi

# Step 3: Clean Go cache
print_step "Bước 3: Clean Go cache..."

cd "$GO_PROJECT_ROOT" || exit 1
print_info "Cleaning Go cache để đảm bảo code mới được compile..."
go clean -cache -testcache >/dev/null 2>&1 || true

# Step 4: Start Go Master Node
print_step "Bước 4: Khởi động Go Master Node..."

cd "$GO_PROJECT_ROOT/cmd/simple_chain" || exit 1

# Set environment variables
export GOTOOLCHAIN=go1.23.5
export XAPIAN_BASE_PATH='sample/simple/data/data/xapian_node'

# Start Go Master in tmux session (for reliability and monitoring)
print_info "Starting Go Master in tmux session 'go-master'..."
cd "$GO_PROJECT_ROOT/cmd/simple_chain"

# Kill existing session if any
tmux kill-session -t go-master 2>/dev/null || true
sleep 1

# Create tmux session with Go Master
if ! tmux new-session -d -s go-master -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/simple/data/data/xapian_node' && go run . -config=config-master.json 2>&1 | tee /tmp/go-master.log"; then
    print_error "❌ Không thể tạo tmux session 'go-master'"
    exit 1
fi

# Wait a bit for the session to start
sleep 3

# Verify Go Master tmux session is running
if ! tmux has-session -t go-master 2>/dev/null; then
    print_error "❌ Tmux session 'go-master' không tồn tại sau khi khởi động"
    print_info "   Kiểm tra log:"
    if [ -f "/tmp/go-master.log" ]; then
        print_info "   - Log file: /tmp/go-master.log"
        print_info "   - Last 20 lines:"
        tail -20 /tmp/go-master.log 2>/dev/null || true
    fi
    exit 1
fi

print_info "✅ Go Master started in tmux session 'go-master'"

print_info "✅ Go Master Node đã khởi động thành công!"
print_info "📺 Xem logs: tmux attach -t go-master"
print_info "🛑 Dừng: tmux kill-session -t go-master"

# Wait for initialization
print_info "⏳ Đợi Go Master init genesis block và register validators..."
sleep 10

# Verify Go Master đã init genesis và có validators
print_info "🔍 Kiểm tra Go Master đã init genesis..."
VALIDATOR_CHECK=false
for i in {1..20}; do
    if grep -qE "Found [1-9][0-9]* validators in stake state DB|Found [1-9] validators in stake state DB" /tmp/go-master.log 2>/dev/null; then
        VALIDATOR_CHECK=true
        print_info "  ✅ Go Master đã init genesis và register validators (sau $i giây)"
        break
    fi
    if [ $i -lt 20 ]; then
        sleep 1
    fi
done

if [ "$VALIDATOR_CHECK" = false ]; then
    print_warn "  ⚠️  Không xác nhận được Go Master đã init validators (có thể vẫn đang init)"
fi

print_info "🎉 Go Master Node đã sẵn sàng!"
echo ""
print_info "=========================================="
print_info "📊 Trạng thái Go Master Node:"
print_info "=========================================="
print_info "  - Tmux Session: go-master ✅"
print_info "  - Config: config-master.json ✅"
print_info "  - Log: /tmp/go-master.log"
echo ""
print_info "📺 Commands:"
print_info "  - View logs: tmux attach -t go-master"
print_info "  - Stop: tmux kill-session -t go-master"
print_info "  - Check status: tmux has-session -t go-master"
echo ""