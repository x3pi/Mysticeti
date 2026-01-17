#!/bin/bash

# Script để chạy full luồng: 1 Go Sub + 1 Go Master + 5 Rust Consensus Nodes
# - 4 Validator Nodes (node-0 đến node-3): Tham gia consensus và voting
# - 1 Sync-Only Node (node-4): Chỉ đồng bộ data, không tham gia validator ban đầu
#   Node-4 sẽ tự động chuyển thành validator nếu nằm trong committee khi chuyển epoch
# Mỗi lần chạy sẽ:
#   - Xóa dữ liệu cũ (sample và storage)
#   - Tạo committee mới (chỉ 4 validators đầu trong genesis.json)
#   - Khởi động tất cả nodes từ epoch 0
#
# Thứ tự khởi động:
#   Thứ tự khởi động (QUAN TRỌNG để tránh mất blocks):
#   1. Go Master Node (đầu tiên, để sẵn sàng nhận blocks từ Rust)
#   2. Go Sub Node (sau Go Master, với delay 15s để kết nối với Go Master)
#   3. Delay thêm 10s để đảm bảo Go Master và Go Sub đã hoàn toàn sẵn sàng
#   4. Rust Consensus Nodes (cuối cùng, sau khi Go Sub đã kết nối với Go Master)
#   
#   Lý do: Nếu Rust nodes chạy trước Go Sub, Go Master sẽ gửi blocks mà Go Sub chưa kết nối,
#   dẫn đến mất blocks và TxsProcessor bị stuck. Delay giúp đảm bảo Go Sub đã sẵn sàng nhận blocks.

set -e
set -o pipefail

# Full clean switches (safe defaults for local dev)
# - FULL_CLEAN_BUILD=1  : run cargo clean before cargo build --release
# - FULL_CLEAN_GO_MODCACHE=0 : DO NOT wipe Go module cache by default (slow; set to 1 if needed)
FULL_CLEAN_BUILD="${FULL_CLEAN_BUILD:-1}"
FULL_CLEAN_GO_MODCACHE="${FULL_CLEAN_GO_MODCACHE:-0}"

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
# METANODE_ROOT = /home/abc/chain-n/Mysticeti/metanode
# MYSTICETI_ROOT = /home/abc/chain-n/Mysticeti
# Go project = /home/abc/chain-n/mtn-simple-2025
GO_PROJECT_ROOT="$(cd "$METANODE_ROOT/../.." && pwd)/mtn-simple-2025"

# Verify paths
if [ ! -f "$METANODE_ROOT/Cargo.toml" ]; then
    echo "Error: Cannot find Cargo.toml at $METANODE_ROOT"
    echo "Expected path: $METANODE_ROOT/Cargo.toml"
    exit 1
fi

if [ ! -d "$GO_PROJECT_ROOT" ]; then
    echo "Error: Cannot find Go project at $GO_PROJECT_ROOT"
    echo "Please ensure mtn-simple-2025 is at the same level as Mysticeti directory"
    exit 1
fi

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

# Step 0: Check sudo permissions for LVM snapshot (if enabled) - SKIPPED
print_step "Bước 0: Kiểm tra quyền sudo cho lệnh snapshot... (SKIPPED)"

# SKIPPED: Check sudo permissions for LVM snapshot
print_info "ℹ️  Bỏ qua kiểm tra sudo cho LVM snapshot"

# Step 1: Clean up old data (CRITICAL: Must be done before starting any nodes)
print_step "Bước 1: Xóa dữ liệu cũ (QUAN TRỌNG: Phải xóa trước khi khởi động nodes)..."

# Clean old Unix sockets in /tmp (stale sockets can break connectivity even on localhost)
print_info "🧹 Xóa Unix sockets cũ trong /tmp (tránh dính socket stale)..."
rm -f /tmp/metanode-tx-*.sock 2>/dev/null || true
rm -f /tmp/executor*.sock 2>/dev/null || true
rm -f /tmp/rust-go.sock_* 2>/dev/null || true
rm -f /tmp/rust-go.sock_1 /tmp/rust-go.sock_2 2>/dev/null || true
print_info "  ✅ Đã cleanup sockets /tmp"

# Clean old LVM snapshots (xóa tất cả snapshot cũ trước khi khởi động mới)
print_info "🧹 Xóa LVM snapshots cũ (tránh conflict với data cũ)..."
LVM_SNAPSHOT_BASE_PATH="/mnt/lvm_public"
if [ -d "$LVM_SNAPSHOT_BASE_PATH" ]; then
    print_info "  - Xóa tất cả snapshots trong: $LVM_SNAPSHOT_BASE_PATH"

    # Xóa thư mục latest symlink trước
    if [ -L "$LVM_SNAPSHOT_BASE_PATH/latest" ]; then
        print_info "    🗑️  Xóa symlink latest..."
        rm -f "$LVM_SNAPSHOT_BASE_PATH/latest" 2>/dev/null || true
    fi

    # Xóa tất cả thư mục snapshot có pattern snap_id_*
    SNAPSHOT_DIRS=$(ls -d "$LVM_SNAPSHOT_BASE_PATH"/snap_id_* 2>/dev/null || true)
    if [ -n "$SNAPSHOT_DIRS" ]; then
        for snap_dir in $SNAPSHOT_DIRS; do
            if [ -d "$snap_dir" ]; then
                print_info "    🗑️  Xóa snapshot: $(basename "$snap_dir")"
                rm -rf "$snap_dir" 2>/dev/null || {
                    print_warn "      ⚠️  Không thể xóa $snap_dir (có thể đang được sử dụng)"
                }
            fi
        done
    else
        print_info "    ℹ️  Không có snapshot cũ nào để xóa"
    fi

    print_info "  ✅ Đã cleanup LVM snapshots cũ"
else
    print_info "  ℹ️  Thư mục LVM snapshot không tồn tại: $LVM_SNAPSHOT_BASE_PATH"
fi

# Clean Go sample data (bao gồm cả logs và tất cả dữ liệu)
# CRITICAL: Phải xóa HOÀN TOÀN để đảm bảo Go init genesis block mới
print_info "🧹 Xóa dữ liệu Go sample HOÀN TOÀN (bao gồm cả logs và database blocks)..."
if [ -d "$GO_PROJECT_ROOT/cmd/simple_chain/sample" ]; then
    print_info "  - Xóa: $GO_PROJECT_ROOT/cmd/simple_chain/sample"
    rm -rf "$GO_PROJECT_ROOT/cmd/simple_chain/sample"
    print_info "  ✅ Đã xóa sample directory"
else
    print_info "  ℹ️  Sample directory không tồn tại, bỏ qua"
fi

# Also clean Go logs directory if exists (logs cũ có thể gây conflict)
if [ -d "$GO_PROJECT_ROOT/cmd/simple_chain/logs" ]; then
    print_info "  - Xóa: $GO_PROJECT_ROOT/cmd/simple_chain/logs"
    rm -rf "$GO_PROJECT_ROOT/cmd/simple_chain/logs"
    print_info "  ✅ Đã xóa logs directory"
else
    print_info "  ℹ️  Logs directory không tồn tại, bỏ qua"
fi

# CRITICAL: Xóa cả database blocks nếu tồn tại (để đảm bảo Go init genesis mới)
# Note: Blocks database có thể tồn tại ngay cả sau khi xóa sample directory
# Phải xóa TRƯỚC khi tạo lại sample directory
print_info "🧹 Xóa database blocks cũ (nếu có) để đảm bảo Go init genesis mới..."
BLOCK_DB_PATHS=(
    "$GO_PROJECT_ROOT/cmd/simple_chain/sample/simple/data/data/blocks"
    "$GO_PROJECT_ROOT/cmd/simple_chain/sample/simple/data-write/data/blocks"
)
for block_db_path in "${BLOCK_DB_PATHS[@]}"; do
    if [ -d "$block_db_path" ]; then
        print_info "  - Xóa: $block_db_path"
        rm -rf "$block_db_path"
        print_info "  ✅ Đã xóa block database"
    fi
done

# CRITICAL: Sau khi tạo lại sample directory, đảm bảo blocks directory không tồn tại
# (có thể được tạo lại tự động, cần xóa lại)
print_info "🧹 Đảm bảo blocks directory không tồn tại sau khi tạo lại sample..."
for block_db_path in "${BLOCK_DB_PATHS[@]}"; do
    if [ -d "$block_db_path" ]; then
        print_info "  - Xóa lại: $block_db_path (đã được tạo lại tự động)"
        rm -rf "$block_db_path"
        print_info "  ✅ Đã xóa lại block database"
    fi
done

# Recreate sample directory structure (cần thiết cho Go nodes)
# CRITICAL: Tạo lại EMPTY directory để Go init genesis block mới
print_info "📁 Tạo lại cấu trúc thư mục sample RỖNG (để Go init genesis mới)..."
mkdir -p "$GO_PROJECT_ROOT/cmd/simple_chain/sample/simple/data/data/xapian_node"
mkdir -p "$GO_PROJECT_ROOT/cmd/simple_chain/sample/simple/data-write/data/xapian_node"
print_info "  ✅ Đã tạo lại cấu trúc thư mục sample (rỗng)"

# CRITICAL: Xóa lại blocks directory SAU KHI tạo lại sample (có thể được tạo lại tự động)
print_info "🧹 Xóa lại blocks directory (nếu có) để đảm bảo Go init genesis mới..."
for block_db_path in "${BLOCK_DB_PATHS[@]}"; do
    if [ -d "$block_db_path" ]; then
        print_info "  - Xóa lại: $block_db_path (có thể được tạo lại tự động)"
        rm -rf "$block_db_path"
        print_info "  ✅ Đã xóa lại block database"
    fi
done

# Final verification: Đảm bảo blocks directory không tồn tại
print_info "🔍 Kiểm tra cuối cùng: blocks directory không tồn tại..."
for block_db_path in "${BLOCK_DB_PATHS[@]}"; do
    if [ -d "$block_db_path" ]; then
        print_error "  ❌ Blocks directory vẫn tồn tại: $block_db_path"
        print_error "     Xóa thủ công và chạy lại script"
        exit 1
    else
        print_info "  ✅ Blocks directory không tồn tại: $block_db_path"
    fi
done
print_info "  💡 Go sẽ init genesis block mới với validators từ genesis.json"

# Clean Rust storage data
print_info "🧹 Xóa dữ liệu Rust storage..."
if [ -d "$METANODE_ROOT/config/storage" ]; then
    print_info "  - Xóa: $METANODE_ROOT/config/storage"
    rm -rf "$METANODE_ROOT/config/storage"
    print_info "  ✅ Đã xóa storage directory"
else
    print_info "  ℹ️  Storage directory không tồn tại, bỏ qua"
fi
mkdir -p "$METANODE_ROOT/config/storage"
print_info "  ✅ Đã tạo lại storage directory"

# Clean Rust logs
print_info "🧹 Xóa logs Rust..."
if [ -d "$METANODE_ROOT/logs" ]; then
    print_info "  - Xóa: $METANODE_ROOT/logs"
    rm -rf "$METANODE_ROOT/logs"
    print_info "  ✅ Đã xóa logs directory"
else
    print_info "  ℹ️  Logs directory không tồn tại, bỏ qua"
fi
mkdir -p "$METANODE_ROOT/logs"
print_info "  ✅ Đã tạo lại logs directory"

print_info "✅ Đã xóa sạch tất cả dữ liệu cũ (sample, logs, storage)"
print_info "   Bây giờ có thể khởi động nodes an toàn"

# Step 2: Stop any running nodes
print_step "Bước 2: Dừng các nodes đang chạy..."

cd "$METANODE_ROOT"

# Function to kill all processes using ports
kill_port_processes() {
    local port=$1
    local max_attempts=${2:-5}
    local attempt=1
    
    while [ $attempt -le $max_attempts ]; do
        PIDS=$(lsof -ti :$port 2>/dev/null || true)
        if [ -z "$PIDS" ]; then
            return 0  # Port is free
        fi
        
        for PID in $PIDS; do
            print_warn "Killing PID $PID đang dùng port $port (attempt $attempt/$max_attempts)..."
            kill -9 "$PID" 2>/dev/null || true
        done
        
        sleep 1
        attempt=$((attempt + 1))
    done
    
    # Final check
    PIDS=$(lsof -ti :$port 2>/dev/null || true)
    if [ -n "$PIDS" ]; then
        print_error "❌ Port $port vẫn bị chiếm bởi: $PIDS"
        return 1
    fi
    return 0
}

# Step 2.1: Kill all processes using ports FIRST (most aggressive)
print_info "🔴 Bước 2.1: Kill tất cả processes đang dùng ports 9000-9004..."
for port in 9000 9001 9002 9003 9004; do
    kill_port_processes $port 5 || true
done
sleep 2

# Step 2.2: Kill all processes by name (comprehensive)
print_info "🔴 Bước 2.2: Kill tất cả processes theo tên..."
pkill -9 -f "simple_chain" 2>/dev/null || true
pkill -9 -f "metanode" 2>/dev/null || true
pkill -9 -f "go run.*simple_chain" 2>/dev/null || true
pkill -9 -f "target/release/metanode" 2>/dev/null || true
# Kill all metanode processes (có thể có nhiều instances)
ps aux | grep -E "[m]etanode.*start" | awk '{print $2}' | xargs -r kill -9 2>/dev/null || true
ps aux | grep -E "[m]etanode" | grep -v grep | awk '{print $2}' | xargs -r kill -9 2>/dev/null || true
sleep 2

# Step 2.3: Stop tmux sessions
print_info "🔴 Bước 2.3: Dừng tmux sessions..."
tmux kill-session -t go-sub 2>/dev/null || true
tmux kill-session -t go-master 2>/dev/null || true
if [ -f "$METANODE_ROOT/scripts/node/stop_nodes.sh" ]; then
    bash "$METANODE_ROOT/scripts/node/stop_nodes.sh" || true
fi
sleep 2

# Step 2.4: Kill processes using ports AGAIN (in case tmux spawned new ones)
print_info "🔴 Bước 2.4: Kill lại processes đang dùng ports (sau khi dừng tmux)..."
for port in 9000 9001 9002 9003 9004; do
    kill_port_processes $port 3 || true
done
sleep 3

# Step 2.5: Final verification and cleanup
print_info "🔴 Bước 2.5: Kiểm tra và cleanup cuối cùng..."
all_ports_free=true
for port in 9000 9001 9002 9003 9004; do
    PIDS=$(lsof -ti :$port 2>/dev/null || true)
    if [ -n "$PIDS" ]; then
        print_error "❌ Port $port VẪN bị chiếm bởi PIDs: $PIDS"
        all_ports_free=false
        # Last attempt: kill with extreme prejudice
        for PID in $PIDS; do
            print_warn "   🔪 Force killing PID $PID..."
            kill -9 "$PID" 2>/dev/null || true
        done
    else
        print_info "✅ Port $port đã được giải phóng"
    fi
done

# If still not free, wait and try one more time
if [ "$all_ports_free" = false ]; then
    print_warn "⚠️  Một số ports vẫn bị chiếm, đợi 5 giây và thử lại lần cuối..."
    sleep 5
    for port in 9000 9001 9002 9003; do
        PIDS=$(lsof -ti :$port 2>/dev/null || true)
        if [ -n "$PIDS" ]; then
            print_error "❌❌ Port $port VẪN bị chiếm bởi: $PIDS"
            print_error "   Vui lòng kill thủ công: kill -9 $PIDS"
            print_error "   Hoặc kiểm tra: lsof -i :$port"
        fi
    done
fi

# Final check before proceeding
print_info "🔍 Kiểm tra cuối cùng trước khi tiếp tục..."
for port in 9000 9001 9002 9003 9004; do
    PIDS=$(lsof -ti :$port 2>/dev/null || true)
    if [ -n "$PIDS" ]; then
        print_error "❌❌❌ KHÔNG THỂ TIẾP TỤC: Port $port vẫn bị chiếm bởi: $PIDS"
        print_error "   Vui lòng kill thủ công và chạy lại script."
        exit 1
    fi
done

print_info "✅ Đã dừng tất cả nodes cũ và giải phóng ports"

# Step 3: Setup Move dependencies (nếu cần)
print_step "Bước 3: Kiểm tra và setup Move dependencies..."

SETUP_SCRIPT="$MYSTICETI_ROOT/scripts/setup_move_dependencies.sh"

# if [ -f "$SETUP_SCRIPT" ]; then
#     print_info "Đang kiểm tra Move dependencies..."
#     bash "$SETUP_SCRIPT" || {
#         print_warn "⚠️  Không thể setup Move dependencies tự động"
#         print_warn "   Bạn có thể chạy thủ công: bash $SETUP_SCRIPT"
#         print_warn "   Move dependencies đã được copy vào: $MYSTICETI_ROOT/external-crates/move/"
#     }
# else
#     print_warn "⚠️  Script setup Move dependencies không tìm thấy tại $SETUP_SCRIPT"
#     print_warn "   Move dependencies đã được copy vào: $MYSTICETI_ROOT/external-crates/move/"
# fi

# Step 4: Build Rust binary (luôn build lại để đảm bảo code mới nhất)
print_step "Bước 4: Build Rust binary và tạo committee mới..."

cd "$METANODE_ROOT" || exit 1

# Luôn build lại để đảm bảo code mới nhất được sử dụng
BINARY="$METANODE_ROOT/target/release/metanode"
print_info "Building metanode binary (this may take a few minutes)..."
print_info "💡 Tip: Nếu muốn skip build, hãy comment phần này trong script"
cd "$METANODE_ROOT" || exit 1

# Optional: force a full rebuild to avoid using stale incremental artifacts
if [ "$FULL_CLEAN_BUILD" = "1" ]; then
    print_info "🧹 FULL_CLEAN_BUILD=1 → chạy cargo clean để đảm bảo rebuild 100%..."
    # Xóa thư mục target/ hoàn toàn để tránh lỗi IO error
    if [ -d "$METANODE_ROOT/target" ]; then
        print_info "  - Xóa thư mục target/ hoàn toàn..."
        rm -rf "$METANODE_ROOT/target"
        print_info "  ✅ Đã xóa target/"
    fi
    # Chạy cargo clean để đảm bảo clean state
    cargo clean || true  # Ignore errors if target/ doesn't exist
fi

cargo build --release --bin metanode
if [ $? -ne 0 ]; then
    print_error "Build failed! Please check the error above."
    exit 1
fi
print_info "✅ Rust build completed"

# Verify binary exists
if [ ! -f "$BINARY" ]; then
    print_error "Binary không tồn tại sau khi build: $BINARY"
    exit 1
fi

# Remove old committee files first - NO LONGER NEEDED since nodes fetch from Go state
print_info "🗑️  Xóa committee files cũ (nodes sẽ fetch từ Go state)..."
cd "$METANODE_ROOT" || exit 1
rm -f "$METANODE_ROOT/config/committee.json"
# KHÔNG xóa committee_node_0.json nữa - giữ lại làm file chuẩn
rm -f "$METANODE_ROOT/config/committee_node_[1-9].json" 2>/dev/null || true
rm -f "$METANODE_ROOT/config/committee_node_[1-9][0-9].json" 2>/dev/null || true
rm -f "$METANODE_ROOT/config/node_*.toml"
rm -f "$METANODE_ROOT/config/node_*_protocol_key.json"
rm -f "$METANODE_ROOT/config/node_*_network_key.json"

# Generate keys and node configs, AND generate genesis.json for Go
print_info "🔑 Tạo keys và node configs cho 4 nodes..."
print_info "📄 Đồng thời tạo genesis.json cho Go từ keys của Rust"
print_info "💡 Committee data sẽ được fetch từ Go state qua Unix Domain Socket"
cd "$METANODE_ROOT" || exit 1

# Generate Rust keys and configs (5 nodes total: 4 validators + 1 sync-only)
"$BINARY" generate --nodes 5 --output config

# Configure node-4 as sync-only (không tham gia validator ban đầu)
print_info "🔄 Cấu hình node-4 là sync-only node..."
NODE_4_CONFIG="$METANODE_ROOT/config/node_4.toml"
if [ -f "$NODE_4_CONFIG" ]; then
    # Update initial_node_mode if it exists, otherwise add it
    if grep -q "^initial_node_mode" "$NODE_4_CONFIG"; then
        # Update existing value
        sed -i 's/^initial_node_mode = .*/initial_node_mode = "SyncOnly"/' "$NODE_4_CONFIG"
        print_info "  ✅ Đã cập nhật initial_node_mode = SyncOnly cho node-4"
    else
        # Add new configuration
        cat >> "$NODE_4_CONFIG" << EOF

# Sync-Only Node Configuration
# Node này chỉ đồng bộ data, không tham gia validator ban đầu
# Có thể tự động chuyển thành validator nếu nằm trong committee
initial_node_mode = "SyncOnly"
EOF
        print_info "  ✅ Đã thêm initial_node_mode = SyncOnly cho node-4"
    fi
else
    print_warn "  ⚠️  Không tìm thấy node_4.toml sau khi generate"
fi

# UPDATE committee.json với stake từ genesis.json (từ delegator_stakes)
print_info "🔄 Update committee.json với stake từ genesis.json..."
UPDATE_SCRIPT="$METANODE_ROOT/scripts/update_committee_from_genesis.py"
if [ -f "$UPDATE_SCRIPT" ]; then
    if python3 "$UPDATE_SCRIPT"; then
        print_info "✅ Đã update committee.json với stake từ delegator_stakes"
