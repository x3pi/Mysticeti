#!/bin/bash
# Script để kill tất cả processes liên quan đến metanode và simple_chain

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

print_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

print_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

print_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

print_step() {
    echo -e "\n${GREEN}========================================${NC}"
    echo -e "${GREEN}$1${NC}"
    echo -e "${GREEN}========================================${NC}\n"
}

cd "$PROJECT_ROOT"

print_step "Kill tất cả processes liên quan đến hệ thống"

# Function to kill all processes using a port
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
        print_error "Port $port vẫn bị chiếm bởi: $PIDS"
        return 1
    fi
    return 0
}

# Step 1: Kill processes using ports
print_info "Bước 1: Kill processes đang dùng ports 9000-9003..."
for port in 9000 9001 9002 9003; do
    kill_port_processes $port 5 || true
done
sleep 2

# Step 2: Kill processes by name
print_info "Bước 2: Kill processes theo tên..."
pkill -9 -f "simple_chain" 2>/dev/null || true
pkill -9 -f "metanode" 2>/dev/null || true
pkill -9 -f "go run.*simple_chain" 2>/dev/null || true
pkill -9 -f "target/release/metanode" 2>/dev/null || true
sleep 2

# Step 3: Kill tmux sessions
print_info "Bước 3: Dừng tmux sessions..."
tmux kill-session -t go-sub 2>/dev/null || true
tmux kill-session -t go-master 2>/dev/null || true
if [ -f "$PROJECT_ROOT/scripts/node/stop_nodes.sh" ]; then
    bash "$PROJECT_ROOT/scripts/node/stop_nodes.sh" || true
fi
sleep 2

# Step 4: Kill processes using ports again
print_info "Bước 4: Kill lại processes đang dùng ports..."
for port in 9000 9001 9002 9003; do
    kill_port_processes $port 3 || true
done
sleep 2

# Step 5: Final verification
print_info "Bước 5: Kiểm tra cuối cùng..."
all_ports_free=true
for port in 9000 9001 9002 9003; do
    PIDS=$(lsof -ti :$port 2>/dev/null || true)
    if [ -n "$PIDS" ]; then
        print_error "❌ Port $port vẫn bị chiếm bởi: $PIDS"
        all_ports_free=false
    else
        print_info "✅ Port $port đã được giải phóng"
    fi
done

if [ "$all_ports_free" = true ]; then
    print_info "✅ Tất cả ports đã được giải phóng!"
else
    print_warn "⚠️  Một số ports vẫn bị chiếm. Vui lòng kill thủ công."
    exit 1
fi

