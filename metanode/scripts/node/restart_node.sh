#!/bin/bash

# ============================================================================
# Script: restart_node.sh
# Mục đích: Restart một MetaNode consensus node và tiếp tục chạy nơi dừng lại
# ============================================================================
#
# HƯỚNG DẪN SỬ DỤNG:
# -------------------
# 1. Restart một node:
#    ./restart_node.sh <node_id> [options]
#
#    Ví dụ:
#    ./restart_node.sh 0                    # Restart node 0
#    ./restart_node.sh 1 --follow-logs     # Restart node 1 và theo dõi logs
#    ./restart_node.sh 2 --wait 5          # Restart node 2, đợi 5 giây giữa kill và start
#
# 2. Options:
#    --follow-logs      : Theo dõi logs ngay sau khi start (Ctrl+C để dừng)
#    --wait <seconds>   : Đợi N giây giữa kill và start (mặc định: 2 giây)
#    --no-reset-epoch  : Không reset epoch_timestamp_ms khi start
#
# TIẾP TỤC CHẠY NƠI DỪNG LẠI:
# -----------------------------
# Khi restart, node sẽ tiếp tục chạy từ nơi dừng lại tùy thuộc vào tình huống:
#
# 1. **CÙNG EPOCH, MUỘN COMMITS** (Tiếp tục đúng nghĩa):
#    - Node ở epoch 7, commit index 5
#    - Network ở epoch 7, commit index 1000
#    - Khi restart:
#      ✅ Node sẽ ĐUỔI KỊP bằng cách sync commits từ peers
#      ✅ Node process commits từ 5 → 1000
#      ✅ Node tiếp tục chạy từ commit 5 và đuổi kịp đến 1000
#      ✅ Đây là "tiếp tục chạy nơi dừng lại" đúng nghĩa
#
# 2. **KHÁC EPOCH** (Nhảy cóc, không tiếp tục):
#    - Node dừng ở epoch 5, commit index 1000
#    - Network đã chuyển sang epoch 7, commit index 5000
#    - Khi restart:
#      ⚠️ Node sẽ NHẢY CÓC vào epoch 7 (bỏ qua epoch 6)
#      ⚠️ Node KHÔNG tiếp tục từ epoch 5
#      ⚠️ Node sync blocks của epoch 7 từ đầu
#      ⚠️ Đây KHÔNG phải "tiếp tục" mà là "bắt đầu lại" ở epoch mới
#
# 3. **CÙNG EPOCH, KHÔNG MUỘN** (Tiếp tục bình thường):
#    - Node ở epoch 7, commit index 1000
#    - Network ở epoch 7, commit index 1000
#    - Khi restart:
#      ✅ Node recover từ DB và tiếp tục chạy bình thường
#      ✅ Node không cần sync gì cả
#      ✅ Đây là "tiếp tục chạy nơi dừng lại" hoàn hảo
#
# KHUYẾN NGHỊ:
# ------------
# - Dùng script này khi bạn muốn restart node và xem recovery process
# - Nếu node cùng epoch, node sẽ tiếp tục từ commit index cũ
# - Nếu node khác epoch, node sẽ nhảy vào epoch mới (không tiếp tục epoch cũ)
# - Dùng --follow-logs để xem quá trình recovery/catch-up
#
# ============================================================================

set -e

# Get script directory and change to project root
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$PROJECT_ROOT"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m' # No Color

print_info() {
    echo -e "${GREEN}ℹ️  $1${NC}"
}

print_warn() {
    echo -e "${YELLOW}⚠️  $1${NC}"
}

print_error() {
    echo -e "${RED}❌ $1${NC}"
}

print_success() {
    echo -e "${GREEN}✅ $1${NC}"
}

print_step() {
    echo -e "${CYAN}📌 $1${NC}"
}

# Check if node_id is provided
if [ -z "$1" ]; then
    print_error "Usage: $0 <node_id> [options]"
    echo ""
    echo "Options:"
    echo "  --follow-logs      Follow logs after starting (default: false)"
    echo "  --wait <seconds>   Wait N seconds between kill and start (default: 2)"
    echo "  --no-reset-epoch  Don't reset epoch timestamp when starting"
    echo ""
    echo "Examples:"
    echo "  $0 0                    # Restart node 0"
    echo "  $0 1 --follow-logs     # Restart node 1 and follow logs"
    echo "  $0 2 --wait 5          # Restart node 2, wait 5 seconds"
    exit 1
fi

NODE_ID=$1
FOLLOW_LOGS=false
WAIT_SECONDS=2
RESET_EPOCH="${RESET_EPOCH_TIMESTAMP_MS:-1}"

# Parse additional arguments
shift
while [[ $# -gt 0 ]]; do
    case $1 in
        --follow-logs)
            FOLLOW_LOGS=true
            shift
            ;;
        --wait)
            if [ -z "$2" ] || ! [[ "$2" =~ ^[0-9]+$ ]]; then
                print_error " --wait requires a number (seconds)"
                exit 1
            fi
            WAIT_SECONDS=$2
            shift 2
            ;;
        --no-reset-epoch)
            RESET_EPOCH=0
            shift
            ;;
        *)
            print_error "Unknown option: $1"
            exit 1
            ;;
    esac
done

# Validate node_id is a number
if ! [[ "$NODE_ID" =~ ^[0-9]+$ ]]; then
    print_error "Node ID must be a number (0, 1, 2, ...)"
    exit 1
fi

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "🔄 Restarting Node $NODE_ID"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# Step 1: Kill node
print_step "Step 1: Killing node $NODE_ID..."
if ./kill_node.sh "$NODE_ID" 2>/dev/null; then
    print_success "Node $NODE_ID killed successfully"
else
    print_warn "Node $NODE_ID may not have been running (this is OK)"
fi

# Step 2: Wait
if [ "$WAIT_SECONDS" -gt 0 ]; then
    print_step "Step 2: Waiting $WAIT_SECONDS seconds before restart..."
    sleep "$WAIT_SECONDS"
fi

# Step 3: Start node
print_step "Step 3: Starting node $NODE_ID..."

# Build start command
START_CMD="./start_node.sh $NODE_ID"
if [ "$RESET_EPOCH" = "0" ]; then
    START_CMD="$START_CMD --no-reset-epoch"
fi

# Start node (without follow-logs for now, we'll handle it separately)
if [ "$FOLLOW_LOGS" = true ]; then
    # Start node and follow logs
    eval "$START_CMD --follow-logs"
else
    # Start node normally
    eval "$START_CMD"
    
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "✅ Node $NODE_ID restarted successfully!"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""
    print_info "💡 Node sẽ tiếp tục chạy từ nơi dừng lại:"
    echo "   - Nếu cùng epoch: node sẽ đuổi kịp commits (nếu muộn) hoặc tiếp tục bình thường"
    echo "   - Nếu khác epoch: node sẽ nhảy vào epoch hiện tại (không tiếp tục epoch cũ)"
    echo ""
    print_info "📊 Để theo dõi recovery/catch-up:"
    echo -e "   ${BLUE}tail -f logs/latest/node_${NODE_ID}.log | grep -i 'recover\|recovery\|Executing commit\|CommitSyncer'${NC}"
    echo ""
    print_info "🔍 Để xem logs đầy đủ:"
    echo -e "   ${BLUE}tail -f logs/latest/node_${NODE_ID}.log${NC}"
    echo ""
    print_warn "⚠️  LƯU Ý VỀ EPOCH TRANSITION:"
    echo "   - Nếu epoch không chuyển đổi sau khi restart, có thể do:"
    echo "     1. Không đủ quorum (2f+1) - cần ít nhất 3/4 nodes online"
    echo "     2. Time-based check chưa đủ thời gian (epoch_duration_seconds)"
    echo "     3. Clock drift - NTP sync fail"
    echo "   - Để kiểm tra epoch status:"
    echo -e "     ${BLUE}./check_epoch_status.sh $NODE_ID${NC}"
    echo -e "     ${BLUE}tail -f logs/latest/node_${NODE_ID}.log | grep -i epoch${NC}"
    echo ""
fi

