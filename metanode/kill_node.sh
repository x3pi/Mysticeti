#!/bin/bash

# ============================================================================
# Script: kill_node.sh
# Mục đích: Kill một MetaNode consensus node cụ thể
# ============================================================================
#
# HƯỚNG DẪN SỬ DỤNG:
# -------------------
# 1. Kill một node:
#    ./kill_node.sh <node_id>
#
#    Ví dụ:
#    ./kill_node.sh 0    # Kill node 0
#    ./kill_node.sh 1    # Kill node 1
#
# 2. Sau khi kill, để restart node:
#    ./start_node.sh <node_id>
#
# 3. Để xem logs trước khi restart:
#    tail -f logs/latest/node_<node_id>.log
#
# RECOVERY KHI RESTART:
# ---------------------
# Khi node được restart (bằng start_node.sh), nó sẽ:
#
# 1. **Load epoch hiện tại từ committee.json**
#    - Node sẽ đọc epoch hiện tại từ config/committee_node_<id>.json
#    - Nếu network đã chuyển sang epoch mới, node sẽ load epoch mới
#    - Node KHÔNG cần process từng epoch một, nó sẽ nhảy thẳng vào epoch hiện tại
#
# 2. **Recovery từ database**
#    - Node sẽ recover từ DB của epoch hiện tại: storage/node_X/epochs/epoch_N/consensus_db
#    - Load DAG state, commit observer state, block commit statuses
#    - Replay unsent commits (nếu có)
#    - Thời gian: 40-60 giây nếu có nhiều commits (>1M commits)
#
# 3. **Sync với network**
#    - Node sẽ sync missing blocks từ các peers
#    - Catch up với current round của epoch hiện tại
#    - Đuổi kịp consensus state
#
# NẾU NODE CÙNG EPOCH NHƯNG MUỘN COMMITS:
# ----------------------------------------
# **Node sẽ ĐUỔI KỊP bằng cách sync commits từ peers:**
#
# ✅ **Cách hoạt động:**
#    - Node detect lag: commit index của node < commit index của network
#    - CommitSyncer tự động sync missing commits từ peers
#    - Node process các commits từ index hiện tại → index của network
#    - Node sẽ catch up hoàn toàn với network
#
# 📝 **Ví dụ cụ thể:**
#    - Node ở epoch 7, commit index 5
#    - Network ở epoch 7, commit index 1000
#    - Khi restart, node sẽ:
#       1. Load epoch 7 từ committee.json (đúng epoch)
#       2. Recover từ DB: storage/node_X/epochs/epoch_7/consensus_db
#       3. Detect lag: commit 5 < 1000
#       4. CommitSyncer sync commits từ peers (commit 6 → 1000)
#       5. Process các commits theo thứ tự: 6, 7, 8, ..., 1000
#       6. Catch up hoàn toàn với network
#
# ⚡ **Cơ chế Sync:**
#    - CommitSyncer tự động chạy mỗi 2 giây để check lag
#    - Parallel fetching: sync nhiều commits cùng lúc
#    - Batch processing: sync theo batch (mặc định 100 commits/batch)
#    - Node sẽ process commits tuần tự để đảm bảo thứ tự
#
# ⏱️ **Thời gian catch up:**
#    - Phụ thuộc vào số commits cần sync
#    - Với 995 commits (1000 - 5): ~10-30 giây
#    - Với nhiều commits (>10K): có thể mất vài phút
#
# NẾU NODE MUỘN NHIỀU EPOCH:
# ---------------------------
# **Node sẽ NHẢY CÓC vào epoch hiện tại, KHÔNG đuổi kịp từng epoch:**
#
# - Node đọc epoch hiện tại từ committee.json (đã được update bởi các nodes khác)
# - Node khởi động với epoch hiện tại ngay lập tức
# - Node sync blocks của epoch hiện tại từ peers
# - Node KHÔNG cần process commits của các epoch cũ
#
# Ví dụ:
#   - Node dừng ở epoch 5
#   - Network đã chuyển sang epoch 7
#   - Khi restart, node sẽ:
#     1. Đọc epoch=7 từ committee.json
#     2. Khởi động với epoch 7 (bỏ qua epoch 6)
#     3. Sync blocks của epoch 7 từ peers
#     4. Catch up với current round của epoch 7
#
# LƯU Ý:
# ------
# - Node cần có committee.json đúng với network hiện tại
# - Nếu committee.json cũ, node sẽ không thể sync được
# - Node sẽ tự động sync blocks từ peers khi restart
# - Recovery time phụ thuộc vào số lượng commits cần recover
#
# ============================================================================

set -e

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
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

# Check if node_id is provided
if [ -z "$1" ]; then
    print_error "Usage: $0 <node_id>"
    echo ""
    echo "Example:"
    echo "  $0 0    # Kill node 0"
    echo "  $0 1    # Kill node 1"
    exit 1
fi

NODE_ID=$1

# Validate node_id is a number
if ! [[ "$NODE_ID" =~ ^[0-9]+$ ]]; then
    print_error "Node ID must be a number (0, 1, 2, ...)"
    exit 1
fi

TMUX_SESSION="metanode-$NODE_ID"

print_info "Killing node $NODE_ID..."

# Check if tmux session exists
if tmux has-session -t "$TMUX_SESSION" 2>/dev/null; then
    print_info "Found tmux session: $TMUX_SESSION"
    
    # Kill tmux session
    if tmux kill-session -t "$TMUX_SESSION" 2>/dev/null; then
        print_success "Killed tmux session: $TMUX_SESSION"
    else
        print_error "Failed to kill tmux session: $TMUX_SESSION"
        exit 1
    fi
else
    print_warn "Tmux session not found: $TMUX_SESSION"
fi

# Also kill any remaining metanode processes for this node
# Look for processes with config file for this node
CONFIG_FILE="config/node_${NODE_ID}.toml"
if pgrep -f "metanode.*start.*${CONFIG_FILE}" > /dev/null; then
    print_info "Killing remaining metanode processes for node $NODE_ID..."
    pkill -f "metanode.*start.*${CONFIG_FILE}" && {
        print_success "Killed remaining processes"
    } || print_warn "No processes found to kill"
fi

# Wait a bit to ensure process is fully stopped
sleep 1

# Verify node is stopped
if tmux has-session -t "$TMUX_SESSION" 2>/dev/null; then
    print_error "Node $NODE_ID is still running!"
    exit 1
else
    print_success "Node $NODE_ID has been stopped successfully"
    echo ""
    print_info "To restart this node, run:"
    echo -e "  ${BLUE}./start_node.sh $NODE_ID${NC}"
    echo ""
    print_info "To view logs before restart:"
    echo -e "  ${BLUE}tail -f logs/latest/node_${NODE_ID}.log${NC}"
fi

