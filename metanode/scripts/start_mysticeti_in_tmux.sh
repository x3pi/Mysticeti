#!/bin/bash

# Script để khởi động Mysticeti system trong tmux session
# Tạo tmux session và chạy run_all_individual.sh trong đó
# Có thể attach vào để monitor progress

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

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

# Get script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SESSION_NAME="mysticeti-startup"

echo ""
print_info "=========================================="
print_info "🚀 Khởi động Mysticeti trong tmux"
print_info "=========================================="
echo ""

# Check if tmux session already exists
if tmux has-session -t "$SESSION_NAME" 2>/dev/null; then
    print_warn "⚠️  Tmux session '$SESSION_NAME' đã tồn tại"
    print_info "🔄 Kill session cũ..."
    tmux kill-session -t "$SESSION_NAME"
    sleep 2
fi

print_step "Tạo tmux session '$SESSION_NAME'..."

# Create tmux session and run the startup script
if tmux new-session -d -s "$SESSION_NAME" -c "$SCRIPT_DIR"; then
    print_success "✅ Tmux session '$SESSION_NAME' đã được tạo"

    # Send the command to run the startup script
    print_info "📤 Gửi lệnh khởi động vào tmux session..."
    tmux send-keys -t "$SESSION_NAME" './run_all_individual.sh' C-m

    # Wait a bit for the script to start
    sleep 3

    print_success "🎉 Script đã bắt đầu chạy trong tmux!"
    echo ""
    print_info "📺 Cách monitor:"
    print_info "  - Attach: tmux attach -t $SESSION_NAME"
    print_info "  - View: tmux attach -t $SESSION_NAME"
    print_info "  - Detach: Ctrl+B, D"
    echo ""
    print_info "🛑 Dừng session:"
    print_info "  - tmux kill-session -t $SESSION_NAME"
    echo ""
    print_info "📊 Kiểm tra trạng thái:"
    print_info "  - tmux list-sessions"
    print_info "  - ps aux | grep simple_chain"
    print_info "  - ps aux | grep metanode"

    # Auto attach option
    echo ""
    read -p "🔗 Có muốn attach vào tmux session ngay bây giờ? (y/N): " -n 1 -r
    echo ""
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        print_info "🔗 Attaching to tmux session..."
        tmux attach -t "$SESSION_NAME"
    else
        print_info "💡 Bạn có thể attach sau bằng: tmux attach -t $SESSION_NAME"
    fi

else
    print_error "❌ Không thể tạo tmux session '$SESSION_NAME'"
    exit 1
fi