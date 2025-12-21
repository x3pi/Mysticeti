#!/bin/bash

# ============================================================================
# Script: start_node.sh
# Mục đích: Khởi động một MetaNode consensus node cụ thể
# ============================================================================
#
# HƯỚNG DẪN SỬ DỤNG:
# -------------------
# 1. Start một node:
#    ./start_node.sh <node_id> [options]
#
#    Ví dụ:
#    ./start_node.sh 0                    # Start node 0
#    ./start_node.sh 1 --follow-logs      # Start node 1 và theo dõi logs
#    ./start_node.sh 2 --no-reset-epoch   # Start node 2, không reset epoch timestamp
#
# 2. Options:
#    --follow-logs      : Theo dõi logs ngay sau khi start (Ctrl+C để dừng)
#    --no-reset-epoch  : Không reset epoch_timestamp_ms (mặc định: reset nếu RESET_EPOCH_TIMESTAMP_MS=1)
#
# 3. Xem logs sau khi start:
#    tail -f logs/latest/node_<node_id>.log
#    tail -f logs/latest/node_<node_id>.epoch.log  # Chỉ epoch-related logs
#
# QUY TRÌNH RECOVERY:
# -------------------
# Khi node được start, nó sẽ thực hiện các bước sau:
#
# 1. **Load Configuration**
#    - Đọc config từ config/node_<id>.toml
#    - Đọc committee từ config/committee_node_<id>.json
#    - Xác định epoch hiện tại từ committee.json
#
# 2. **Epoch Detection**
#    - Node đọc epoch từ committee.json
#    - Nếu network đã chuyển epoch, node sẽ load epoch mới
#    - Node KHÔNG cần process từng epoch một, nó sẽ nhảy thẳng vào epoch hiện tại
#
# 3. **Database Recovery**
#    - Node recover từ DB của epoch hiện tại: storage/node_X/epochs/epoch_N/consensus_db
#    - Load DAG state từ RocksDB
#    - Recover committed state
#    - Recover block commit statuses
#    - Recover commit observer state
#    - Replay unsent commits (nếu có)
#
# 4. **Sync với Network**
#    - Node sync missing blocks từ peers
#    - Catch up với current round
#    - Đuổi kịp consensus state của epoch hiện tại
#
# NẾU NODE MUỘN NHIỀU EPOCH:
# ---------------------------
# **Node sẽ NHẢY CÓC vào epoch hiện tại, KHÔNG đuổi kịp từng epoch:**
#
# ✅ **Cách hoạt động:**
#    - Node đọc epoch hiện tại từ committee.json (đã được update bởi các nodes khác)
#    - Node khởi động với epoch hiện tại ngay lập tức
#    - Node sync blocks của epoch hiện tại từ peers
#    - Node KHÔNG cần process commits của các epoch cũ
#
# ❌ **Node KHÔNG đuổi kịp từng epoch:**
#    - Node không process epoch 6 nếu đã ở epoch 7
#    - Node không replay commits của epoch cũ
#    - Node chỉ sync blocks của epoch hiện tại
#
# 📝 **Ví dụ cụ thể:**
#    - Node dừng ở epoch 5, commit index 1000
#    - Network đã chuyển sang epoch 7, commit index 5000
#    - Khi restart, node sẽ:
#       1. Đọc epoch=7 từ committee.json
#       2. Khởi động với epoch 7 (bỏ qua epoch 6)
#       3. Load DB: storage/node_X/epochs/epoch_7/consensus_db
#       4. Sync blocks của epoch 7 từ peers
#       5. Catch up với current round của epoch 7
#       6. KHÔNG process commits của epoch 5 hoặc epoch 6
#
# THỜI GIAN RECOVERY:
# --------------------
# - **Ít commits (<1000)**: ~5-10 giây
# - **Nhiều commits (100K-1M)**: ~20-40 giây
# - **Rất nhiều commits (>1M)**: ~40-60 giây
#
# Logs để theo dõi recovery:
#   - "Recovering committed state from C..."
#   - "Recovering block commit statuses..."
#   - "Recovering commit observer in the range [1..=N]"
#   - "Recovering N unsent commits"
#   - "Executing commit #N (ordered): ..."
#   - "Consensus authority started, took X.Xs"
#
# LƯU Ý QUAN TRỌNG:
# -----------------
# 1. **Committee.json phải đúng**: Node cần có committee.json đúng với network hiện tại
#    - Nếu committee.json cũ, node sẽ không thể sync được
#    - Committee.json được update tự động khi epoch transition
#
# 2. **Epoch timestamp**: 
#    - Mặc định script sẽ reset epoch_timestamp_ms (nếu RESET_EPOCH_TIMESTAMP_MS=1)
#    - Dùng --no-reset-epoch để giữ nguyên timestamp
#    - Tất cả nodes phải dùng cùng epoch_timestamp_ms
#
# 3. **Database path**: 
#    - Mỗi epoch có DB riêng: storage/node_X/epochs/epoch_N/consensus_db
#    - Node chỉ recover từ DB của epoch hiện tại
#    - DB của epoch cũ được giữ lại (không xóa)
#
# 4. **Network sync**: 
#    - Node sẽ tự động sync blocks từ peers
#    - Cần đảm bảo network connectivity
#    - Node sẽ catch up với current round
#
# ============================================================================

set -e

# Configuration
BINARY="./target/release/metanode"
CONFIG_DIR="config"
LOG_DIR="logs"

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
    print_error "Usage: $0 <node_id> [options]"
    echo ""
    echo "Options:"
    echo "  --follow-logs    Follow logs after starting (default: false)"
    echo "  --no-reset-epoch Don't reset epoch timestamp (default: reset if RESET_EPOCH_TIMESTAMP_MS=1)"
    echo ""
    echo "Examples:"
    echo "  $0 0              # Start node 0"
    echo "  $0 1 --follow-logs # Start node 1 and follow logs"
    exit 1
fi

NODE_ID=$1
FOLLOW_LOGS=false
RESET_EPOCH="${RESET_EPOCH_TIMESTAMP_MS:-1}"

# Parse additional arguments
shift
while [[ $# -gt 0 ]]; do
    case $1 in
        --follow-logs)
            FOLLOW_LOGS=true
            shift
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

TMUX_SESSION="metanode-$NODE_ID"
CONFIG_FILE="$CONFIG_DIR/node_${NODE_ID}.toml"
COMMITTEE_FILE="$CONFIG_DIR/committee_node_${NODE_ID}.json"

# Check if binary exists
if [ ! -f "$BINARY" ]; then
    print_error "Binary not found: $BINARY"
    print_info "Please build first: cargo build --release --bin metanode"
    exit 1
fi

# Check if config file exists
if [ ! -f "$CONFIG_FILE" ]; then
    print_error "Config file not found: $CONFIG_FILE"
    print_info "Please generate configs first: $BINARY generate --nodes <num_nodes> --output $CONFIG_DIR"
    exit 1
fi

# Check if node is already running
if tmux has-session -t "$TMUX_SESSION" 2>/dev/null; then
    print_warn "Node $NODE_ID is already running in tmux session: $TMUX_SESSION"
    print_info "To view it: tmux attach -t $TMUX_SESSION"
    print_info "To kill it first: ./kill_node.sh $NODE_ID"
    exit 1
fi

# Get or create log directory
if [ -L "$LOG_DIR/latest" ]; then
    RUN_LOG_DIR="$LOG_DIR/$(readlink "$LOG_DIR/latest")"
else
    # Create new run directory
    RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)"
    RUN_LOG_DIR="$LOG_DIR/run-$RUN_ID"
    mkdir -p "$RUN_LOG_DIR"
    ln -sfn "$(basename "$RUN_LOG_DIR")" "$LOG_DIR/latest" 2>/dev/null || true
fi

LOG_FILE="$RUN_LOG_DIR/node_${NODE_ID}.log"
EPOCH_LOG_FILE="$RUN_LOG_DIR/node_${NODE_ID}.epoch.log"

# Optionally reset epoch_timestamp_ms
if [ "$RESET_EPOCH" = "1" ] && [ -f "$COMMITTEE_FILE" ]; then
    print_info "Resetting epoch_timestamp_ms in $COMMITTEE_FILE..."
    NOW_MS="$(python3 -c 'import time; print(int(time.time()*1000))')"
    
    python3 - "$COMMITTEE_FILE" "$NOW_MS" <<'PY'
import json, sys
path = sys.argv[1]
now_ms = int(sys.argv[2])
with open(path, "r", encoding="utf-8") as f:
    data = json.load(f)
data["epoch_timestamp_ms"] = now_ms
tmp = path + ".tmp"
with open(tmp, "w", encoding="utf-8") as f:
    json.dump(data, f, indent=2, sort_keys=False)
    f.write("\n")
import os
os.replace(tmp, path)
PY
    
    print_success "epoch_timestamp_ms reset to $NOW_MS"
fi

print_info "Starting node $NODE_ID..."
print_info "Config: $CONFIG_FILE"
print_info "Logs: $LOG_FILE"

# Start node in tmux session with logging
tmux new-session -d -s "$TMUX_SESSION" \
    "RUST_BACKTRACE=1 RUST_LOG=info,metanode=info,consensus_core=info stdbuf -oL -eL $BINARY start --config $CONFIG_FILE 2>&1 | stdbuf -oL -eL tee -a $LOG_FILE | stdbuf -oL -eL tee -a >(grep -a -i --line-buffered -E 'epoch|epoch_change|proposal_hash|quorum|transition|committee\\.json|fork|recover|recovery|Executing commit' >> $EPOCH_LOG_FILE) >/dev/null"

# Wait a moment to ensure node started
sleep 2

# Check if node started successfully
if tmux has-session -t "$TMUX_SESSION" 2>/dev/null; then
    print_success "Node $NODE_ID started successfully!"
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "📊 Node $NODE_ID Information:"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""
    echo "View in tmux:"
    echo -e "  ${BLUE}tmux attach -t $TMUX_SESSION${NC}"
    echo "  (Press Ctrl+B, then D to detach)"
    echo ""
    echo "View logs:"
    echo -e "  ${BLUE}tail -f $LOG_FILE${NC}"
    echo -e "  ${BLUE}tail -f $EPOCH_LOG_FILE${NC}  # Epoch-related logs only"
    echo ""
    echo "Monitor recovery:"
    echo -e "  ${BLUE}tail -f $LOG_FILE | grep -i 'recover\|recovery\|Executing commit'${NC}"
    echo ""
    
    # Get network info from config
    if grep -q "network_address" "$CONFIG_FILE"; then
        NETWORK_ADDR=$(grep "network_address" "$CONFIG_FILE" | cut -d'"' -f2)
        METRICS_PORT=$(grep "metrics_port" "$CONFIG_FILE" | cut -d'=' -f2 | tr -d ' ')
        RPC_PORT=$((METRICS_PORT + 1000))
        
        echo "Network:"
        echo "  Consensus: $NETWORK_ADDR"
        echo "  Metrics:   http://localhost:$METRICS_PORT/metrics"
        echo "  RPC:       http://localhost:$RPC_PORT"
        echo ""
    fi
    
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""
    print_info "📋 Recovery Process:"
    echo "   - Node sẽ load epoch hiện tại từ committee.json"
    echo "   - Nếu network đã chuyển epoch, node sẽ NHẢY CÓC vào epoch hiện tại"
    echo "   - Node KHÔNG cần đuổi kịp từng epoch, chỉ sync epoch hiện tại"
    echo "   - Recovery time: 40-60 giây nếu có nhiều commits (>1M)"
    echo ""
    print_info "🔍 Watch for recovery messages:"
    echo "   - 'Recovering committed state from C...'"
    echo "   - 'Recovering commit observer in the range [1..=N]'"
    echo "   - 'Executing commit #N (ordered): ...'"
    echo "   - 'Consensus authority started, took X.Xs'"
    echo ""
    
    if [ "$FOLLOW_LOGS" = true ]; then
        print_info "Following logs (Ctrl+C to stop)..."
        echo ""
        tail -f "$LOG_FILE"
    fi
else
    print_error "Failed to start node $NODE_ID!"
    print_info "Check logs: $LOG_FILE"
    exit 1
fi

