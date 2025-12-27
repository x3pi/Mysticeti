#!/bin/bash

# Script để chạy full luồng: 1 Go Sub + 1 Go Master + 4 Rust Consensus Nodes
# Mỗi lần chạy sẽ:
#   - Xóa dữ liệu cũ (sample và storage)
#   - Tạo committee mới
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
# Go project is at the same level as Mysticeti directory
# METANODE_ROOT = /home/abc/chain-new/Mysticeti/metanode
# Go project = /home/abc/chain-new/mtn-simple-2025
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

# Step 1: Clean up old data (CRITICAL: Must be done before starting any nodes)
print_step "Bước 1: Xóa dữ liệu cũ (QUAN TRỌNG: Phải xóa trước khi khởi động nodes)..."

# Clean old Unix sockets in /tmp (stale sockets can break connectivity even on localhost)
print_info "🧹 Xóa Unix sockets cũ trong /tmp (tránh dính socket stale)..."
rm -f /tmp/metanode-tx-*.sock 2>/dev/null || true
rm -f /tmp/executor*.sock 2>/dev/null || true
rm -f /tmp/rust-go.sock_* 2>/dev/null || true
rm -f /tmp/rust-go.sock_1 /tmp/rust-go.sock_2 2>/dev/null || true
print_info "  ✅ Đã cleanup sockets /tmp"

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
print_info "🔴 Bước 2.1: Kill tất cả processes đang dùng ports 9000-9003..."
for port in 9000 9001 9002 9003; do
    kill_port_processes $port 5 || true
done
sleep 2

# Step 2.2: Kill all processes by name (comprehensive)
print_info "🔴 Bước 2.2: Kill tất cả processes theo tên..."
pkill -9 -f "simple_chain" 2>/dev/null || true
pkill -9 -f "metanode" 2>/dev/null || true
pkill -9 -f "go run.*simple_chain" 2>/dev/null || true
pkill -9 -f "target/release/metanode" 2>/dev/null || true
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
for port in 9000 9001 9002 9003; do
    kill_port_processes $port 3 || true
done
sleep 3

# Step 2.5: Final verification and cleanup
print_info "🔴 Bước 2.5: Kiểm tra và cleanup cuối cùng..."
all_ports_free=true
for port in 9000 9001 9002 9003; do
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
for port in 9000 9001 9002 9003; do
    PIDS=$(lsof -ti :$port 2>/dev/null || true)
    if [ -n "$PIDS" ]; then
        print_error "❌❌❌ KHÔNG THỂ TIẾP TỤC: Port $port vẫn bị chiếm bởi: $PIDS"
        print_error "   Vui lòng kill thủ công và chạy lại script."
        exit 1
    fi
done

print_info "✅ Đã dừng tất cả nodes cũ và giải phóng ports"

# Step 3: Build Rust binary (luôn build lại để đảm bảo code mới nhất)
print_step "Bước 3: Build Rust binary và tạo committee mới..."

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

# Remove old committee files first
print_info "Xóa committee cũ..."
cd "$METANODE_ROOT" || exit 1
rm -f "$METANODE_ROOT/config/committee.json"
rm -f "$METANODE_ROOT/config/committee_node_*.json"
rm -f "$METANODE_ROOT/config/node_*.toml"
rm -f "$METANODE_ROOT/config/node_*_protocol_key.json"
rm -f "$METANODE_ROOT/config/node_*_network_key.json"

# Generate new committee for 4 nodes
print_info "Tạo committee mới cho 4 nodes (epoch 0)..."
cd "$METANODE_ROOT" || exit 1
"$BINARY" generate --nodes 4 --output config

# Verify committee files exist
if [ ! -f "$METANODE_ROOT/config/committee_node_0.json" ]; then
    print_error "Không thể tạo committee files!"
    exit 1
fi

print_info "✅ Đã tạo committee mới"

# Step 3.1: Sync committee vào genesis.json
print_step "Bước 3.1: Sync committee vào genesis.json..."

# Check if sync script exists
SYNC_SCRIPT="$(cd "$METANODE_ROOT/../.." && pwd)/sync_committee_to_genesis.py"
if [ ! -f "$SYNC_SCRIPT" ]; then
    print_warn "⚠️  Script sync_committee_to_genesis.py không tìm thấy tại $SYNC_SCRIPT"
    print_warn "   Bỏ qua bước sync vào genesis.json"
else
    # Use committee_node_0.json as source (all nodes have same committee initially)
    COMMITTEE_SOURCE="$METANODE_ROOT/config/committee_node_0.json"
    GENESIS_TARGET="$GO_PROJECT_ROOT/cmd/simple_chain/genesis.json"
    
    if [ ! -f "$COMMITTEE_SOURCE" ]; then
        print_error "Không tìm thấy committee file: $COMMITTEE_SOURCE"
        exit 1
    fi
    
    if [ ! -f "$GENESIS_TARGET" ]; then
        print_error "Không tìm thấy genesis.json: $GENESIS_TARGET"
        exit 1
    fi
    
    print_info "📝 Syncing committee từ $COMMITTEE_SOURCE vào $GENESIS_TARGET..."
    print_info "   💡 Điều này đảm bảo Go Master sẽ init genesis với validators mới từ Rust committee"
    python3 "$SYNC_SCRIPT" "$COMMITTEE_SOURCE" "$GENESIS_TARGET"
    
    if [ $? -eq 0 ]; then
        print_info "✅ Đã sync committee vào genesis.json"
        
        # Verify genesis.json có validators
        VALIDATOR_COUNT=$(grep -c '"address"' "$GENESIS_TARGET" 2>/dev/null || echo "0")
        if [ "$VALIDATOR_COUNT" -gt 0 ]; then
            print_info "  ✅ Genesis.json có $VALIDATOR_COUNT validators"
        else
            print_warn "  ⚠️  Genesis.json không có validators! Go sẽ không có validators để init genesis"
        fi
    else
        print_error "❌ Lỗi khi sync committee vào genesis.json"
        exit 1
    fi
    
    # CRITICAL: Xóa TẤT CẢ committee_node_*.json files vì tất cả nodes đều lấy từ Go state
    # Không cần sync vào committee_node_X.json files nữa vì tất cả nodes đều lấy từ Go qua Unix Domain Socket
    # Files này sẽ được tạo lại sau epoch transition để lưu epoch_timestamp_ms và last_global_exec_index
    print_info "🗑️  Xóa TẤT CẢ committee_node_*.json files vì tất cả nodes đều load từ Go state..."
    for i in 0 1 2 3; do
        COMMITTEE_NODE_FILE="$METANODE_ROOT/config/committee_node_${i}.json"
        if [ -f "$COMMITTEE_NODE_FILE" ]; then
            rm -f "$COMMITTEE_NODE_FILE"
            print_info "  ✅ Đã xóa committee_node_${i}.json"
        else
            print_info "  ℹ️  committee_node_${i}.json không tồn tại, bỏ qua"
        fi
    done
    print_info "  💡 Các file này sẽ được tạo lại sau epoch transition để lưu epoch_timestamp_ms và last_global_exec_index"
    print_info "  💡 Tất cả nodes (0, 1, 2, 3) đều lấy committee từ Go state qua Unix Domain Socket, không đọc từ file"
fi

# Step 4: Verify executor configuration for Node 0
print_step "Bước 4: Kiểm tra cấu hình executor cho Node 0..."

# Executor is now configured via executor_enabled field in node_0.toml
# No need to create separate enable_executor.toml file
if [ -f "$METANODE_ROOT/config/enable_executor.toml" ]; then
    print_warn "File enable_executor.toml đã không còn được sử dụng (đã chuyển sang executor_enabled trong node_X.toml)"
    print_info "Xóa file cũ..."
    rm -f "$METANODE_ROOT/config/enable_executor.toml"
fi

print_info "✅ Executor được cấu hình qua executor_enabled trong node_0.toml"

# Step 4.5: Regenerate Go protobuf (QUAN TRỌNG: Phải làm trước khi build Go)
print_step "Bước 4.5: Regenerate Go protobuf..."

PROTOC_SCRIPT="$GO_PROJECT_ROOT/pkg/proto/protoc.sh"
if [ -f "$PROTOC_SCRIPT" ]; then
    print_info "Regenerating Go protobuf từ $PROTOC_SCRIPT..."
    cd "$GO_PROJECT_ROOT/pkg/proto" || exit 1
    bash "$PROTOC_SCRIPT"
    if [ $? -eq 0 ]; then
        print_info "✅ Đã regenerate Go protobuf"
    else
        print_error "❌ Lỗi khi regenerate Go protobuf"
        exit 1
    fi
else
    print_warn "⚠️  Không tìm thấy protoc.sh tại $PROTOC_SCRIPT"
    print_warn "   Bỏ qua bước regenerate protobuf (có thể gây lỗi nếu protobuf chưa được cập nhật)"
fi

# Step 5: Start Go Master Node (đầu tiên)
print_step "Bước 5: Khởi động Go Master Node (đầu tiên)..."

cd "$GO_PROJECT_ROOT/cmd/simple_chain" || exit 1

# CRITICAL: Xóa blocks database NGAY TRƯỚC KHI khởi động Go Master
# (có thể được tạo lại trong quá trình chạy script)
print_info "🧹 Xóa blocks database NGAY TRƯỚC KHI khởi động Go Master..."
BLOCK_DB_PATHS_FINAL=(
    "$GO_PROJECT_ROOT/cmd/simple_chain/sample/simple/data/data/blocks"
    "$GO_PROJECT_ROOT/cmd/simple_chain/sample/simple/data-write/data/blocks"
)
for block_db_path in "${BLOCK_DB_PATHS_FINAL[@]}"; do
    if [ -d "$block_db_path" ]; then
        print_warn "  ⚠️  Blocks directory vẫn tồn tại: $block_db_path"
        print_info "  - Xóa: $block_db_path"
        rm -rf "$block_db_path"
        print_info "  ✅ Đã xóa block database"
    fi
done

# Final verification: Đảm bảo blocks directory không tồn tại
print_info "🔍 Final verification: Kiểm tra blocks directory không tồn tại..."
for block_db_path in "${BLOCK_DB_PATHS_FINAL[@]}"; do
    if [ -d "$block_db_path" ]; then
        print_error "  ❌❌ Blocks directory VẪN tồn tại: $block_db_path"
        print_error "     Vui lòng xóa thủ công: rm -rf $block_db_path"
        print_error "     Sau đó chạy lại script"
        exit 1
    else
        print_info "  ✅ Blocks directory không tồn tại: $block_db_path"
    fi
done
print_info "  ✅ Đảm bảo Go sẽ init genesis block mới"

# Start Go Master Node in tmux session using go run (like run.sh)
print_info "Khởi động Go Master Node (config-master.json) trong tmux session 'go-master'..."
print_info "Sử dụng 'go run' như script run.sh (không cần build binary)"
tmux kill-session -t go-master 2>/dev/null || true

# Set environment variables like run.sh
export GOTOOLCHAIN=go1.23.5
export XAPIAN_BASE_PATH='sample/simple/data/data/xapian_node'

# Clean Go cache first to ensure fresh build (avoid stale cached packages)
print_info "Cleaning Go cache để đảm bảo code mới được compile (go clean -cache -testcache)..."
cd "$GO_PROJECT_ROOT" || exit 1
go clean -cache -testcache >/dev/null 2>&1 || true
if [ "$FULL_CLEAN_GO_MODCACHE" = "1" ]; then
    print_warn "FULL_CLEAN_GO_MODCACHE=1 → xóa Go module cache (SẼ RẤT CHẬM vì phải tải lại deps)..."
    go clean -modcache >/dev/null 2>&1 || true
fi

# Start in tmux with go run
print_info "🚀 Khởi động Go Master Node (sẽ init genesis block mới với validators từ genesis.json)..."
tmux new-session -d -s go-master -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/simple/data/data/xapian_node' && go run . -config=config-master.json"

sleep 8  # Tăng delay để Go Master có thời gian init genesis block

# Verify Go Master Node is running
if tmux has-session -t go-master 2>/dev/null; then
    print_info "✅ Go Master Node đã khởi động (tmux session: go-master)"
else
    print_error "Không thể khởi động Go Master Node!"
    exit 1
fi

# CRITICAL: Verify Go Master đã init genesis block (check log)
print_info "🔍 Kiểm tra Go Master đã init genesis block..."
sleep 2  # Đợi thêm để Go init genesis
GENESIS_INIT_CHECK=$(tmux capture-pane -t go-master -p | grep -E "lastblock header 1|initGenesisBlock|Genesis" | head -1 || true)
if [ -n "$GENESIS_INIT_CHECK" ]; then
    print_info "  ✅ Go Master đã init genesis block (tìm thấy log: $GENESIS_INIT_CHECK)"
else
    print_warn "  ⚠️  Không thấy log init genesis block (có thể Go đang dùng block cũ)"
    print_warn "     Kiểm tra log: tmux attach -t go-master"
    print_warn "     Tìm log 'lastblock header 1' (init genesis) hoặc 'lastblock header 2' (dùng block cũ)"
fi

# Step 6: Start Go Sub Node (sau Go Master, với delay để kết nối)
print_step "Bước 6: Khởi động Go Sub Node (sau Go Master, delay để kết nối)..."

cd "$GO_PROJECT_ROOT/cmd/simple_chain" || exit 1

# Start Go Sub Node in tmux session using go run (like run.sh)
print_info "Khởi động Go Sub Node (config-sub-write.json) trong tmux session 'go-sub'..."
print_info "Sử dụng 'go run' như script run.sh (không cần build binary)"
tmux kill-session -t go-sub 2>/dev/null || true

# Set environment variables like run.sh
export GOTOOLCHAIN=go1.23.5
export XAPIAN_BASE_PATH='sample/simple/data-write/data/xapian_node'

# Start in tmux with go run (clean cache first to ensure fresh build)
print_info "Cleaning Go cache để đảm bảo code mới được compile (go clean -cache -testcache)..."
cd "$GO_PROJECT_ROOT" || exit 1
go clean -cache -testcache >/dev/null 2>&1 || true
if [ "$FULL_CLEAN_GO_MODCACHE" = "1" ]; then
    print_warn "FULL_CLEAN_GO_MODCACHE=1 → xóa Go module cache (SẼ RẤT CHẬM vì phải tải lại deps)..."
    go clean -modcache >/dev/null 2>&1 || true
fi

# Start in tmux with go run
tmux new-session -d -s go-sub -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/simple/data-write/data/xapian_node' && go run . -config=config-sub-write.json"

print_info "⏳ Đợi Go Sub Node kết nối với Go Master (15 giây)..."
sleep 15  # Tăng delay để đảm bảo Go Sub có thời gian kết nối với Go Master

# Verify Go Sub Node is running
if tmux has-session -t go-sub 2>/dev/null; then
    print_info "✅ Go Sub Node đã khởi động (tmux session: go-sub)"
else
    print_error "Không thể khởi động Go Sub Node!"
    exit 1
fi

# Thêm delay trước khi khởi động Rust nodes để đảm bảo Go Master và Go Sub đã sẵn sàng
print_info "⏳ Đợi Go Master và Go Sub hoàn toàn sẵn sàng trước khi khởi động Rust consensus (10 giây)..."
print_info "   💡 Điều này đảm bảo Go Sub đã kết nối với Go Master và sẵn sàng nhận blocks từ Go Master"
sleep 10

# Step 7: Start Rust consensus nodes (sau Go Sub, sau khi Go Sub đã kết nối với Go Master)
print_step "Bước 7: Khởi động 4 Rust consensus nodes (sau Go Sub, sau khi Go Sub đã kết nối với Go Master)..."

cd "$METANODE_ROOT" || exit 1

# Reset epoch timestamp to start from epoch 0
export RESET_EPOCH_TIMESTAMP_MS=1

if [ -f "$METANODE_ROOT/scripts/node/run_nodes.sh" ]; then
    print_info "Khởi động Rust nodes..."
    print_info "💡 Rust nodes sẽ bắt đầu tạo blocks, Go Sub đã sẵn sàng nhận blocks từ Go Master"
    cd "$METANODE_ROOT" || exit 1
    bash "$METANODE_ROOT/scripts/node/run_nodes.sh"
    sleep 5  # Đợi nodes khởi động
else
    print_error "Không tìm thấy script run_nodes.sh!"
    exit 1
fi

# Verify nodes are running
NODE_COUNT=$(ps aux | grep -c "[m]etanode.*start" || true)
if [ "$NODE_COUNT" -lt 4 ]; then
    print_warn "Có vẻ như không đủ 4 Rust nodes đang chạy (tìm thấy: $NODE_COUNT)"
else
    print_info "✅ Đã khởi động $NODE_COUNT Rust nodes"
fi

# Đợi thêm một chút để Rust nodes hoàn toàn sẵn sàng
print_info "⏳ Đợi Rust nodes sẵn sàng (5 giây)..."
sleep 5

# Step 8: Verify system
print_step "Bước 8: Kiểm tra hệ thống..."

sleep 5

# Check Rust nodes
RUST_NODES=$(ps aux | grep -c "[m]etanode.*start" || true)
print_info "Rust nodes đang chạy: $RUST_NODES/4"

# Check Go nodes
GO_SUB=$(tmux has-session -t go-sub 2>/dev/null && echo "1" || echo "0")
GO_MASTER=$(tmux has-session -t go-master 2>/dev/null && echo "1" || echo "0")
print_info "Go Sub Node: $([ "$GO_SUB" = "1" ] && echo "✅ Running" || echo "❌ Stopped")"
print_info "Go Master Node: $([ "$GO_MASTER" = "1" ] && echo "✅ Running" || echo "❌ Stopped")"

# Check sockets
if [ -S "/tmp/metanode-tx-0.sock" ]; then
    print_info "✅ Rust Node 0 transaction socket: /tmp/metanode-tx-0.sock"
else
    print_warn "⚠️  Rust Node 0 transaction socket chưa sẵn sàng"
fi

if [ -S "/tmp/executor0.sock" ]; then
    print_info "✅ Rust Node 0 executor socket: /tmp/executor0.sock"
else
    print_warn "⚠️  Rust Node 0 executor socket chưa sẵn sàng"
fi

# Summary
echo ""
print_info "=========================================="
print_info "🎉 Hệ thống đã được khởi động!"
print_info "=========================================="
echo ""
print_info "📊 Trạng thái:"
print_info "  - Rust Consensus Nodes: $RUST_NODES/4"
print_info "  - Go Sub Node: $([ "$GO_SUB" = "1" ] && echo "✅" || echo "❌")"
print_info "  - Go Master Node: $([ "$GO_MASTER" = "1" ] && echo "✅" || echo "❌")"
echo ""
print_info "📺 Xem logs:"
print_info "  - Rust Node 0: tmux attach -t metanode-0"
print_info "  - Rust Node 1: tmux attach -t metanode-1"
print_info "  - Rust Node 2: tmux attach -t metanode-2"
print_info "  - Rust Node 3: tmux attach -t metanode-3"
print_info "  - Go Sub: tmux attach -t go-sub"
print_info "  - Go Master: tmux attach -t go-master"
echo ""
print_info "🛑 Dừng hệ thống:"
print_info "  ./scripts/stop_full_system.sh"
echo ""

