#!/bin/bash

# Script để chạy tất cả MetaNode consensus nodes trong tmux sessions

set -e

# Configuration
NODES=4
BINARY="./target/release/metanode"
CONFIG_DIR="config"
LOG_DIR="logs"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Function to print colored messages
print_info() {
    echo -e "${GREEN}ℹ️  $1${NC}"
}

print_warn() {
    echo -e "${YELLOW}⚠️  $1${NC}"
}

print_error() {
    echo -e "${RED}❌ $1${NC}"
}

# Check if binary exists
if [ ! -f "$BINARY" ]; then
    print_error "Binary not found: $BINARY"
    print_info "Please build first: cargo build --release --bin metanode"
    exit 1
fi

# Check if config directory exists
if [ ! -d "$CONFIG_DIR" ]; then
    print_error "Config directory not found: $CONFIG_DIR"
    print_info "Please generate configs first: $BINARY generate --nodes $NODES --output $CONFIG_DIR"
    exit 1
fi

# Create log directory
mkdir -p "$LOG_DIR"

# Kill existing sessions
print_info "Cleaning up existing sessions..."
for i in $(seq 0 $((NODES-1))); do
    tmux kill-session -t "metanode-$i" 2>/dev/null && print_info "Killed existing session: metanode-$i" || true
done

sleep 1

# Start nodes
print_info "Starting $NODES nodes..."
for i in $(seq 0 $((NODES-1))); do
    config_file="$CONFIG_DIR/node_$i.toml"
    log_file="$LOG_DIR/node_$i.log"
    
    if [ ! -f "$config_file" ]; then
        print_error "Config file not found: $config_file"
        exit 1
    fi
    
    print_info "Starting node $i (config: $config_file)..."
    
    # Start node in tmux session with logging
    tmux new-session -d -s "metanode-$i" \
        "RUST_LOG=info,metanode=info,consensus_core=info stdbuf -oL -eL $BINARY start --config $config_file 2>&1 | tee $log_file"
    
    sleep 1
done

print_info "All nodes started!"
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "📊 Node Management Commands:"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "View logs:"
echo "  tmux attach -t metanode-0  # View node 0 (Ctrl+B, D to detach)"
echo "  tmux attach -t metanode-1  # View node 1"
echo "  tmux attach -t metanode-2  # View node 2"
echo "  tmux attach -t metanode-3  # View node 3"
echo ""
echo "View log files:"
echo "  tail -f $LOG_DIR/node_0.log  # Follow node 0 logs"
echo "  tail -f $LOG_DIR/node_1.log  # Follow node 1 logs"
echo ""
echo "List all sessions:"
echo "  tmux list-sessions"
echo ""
echo "Stop all nodes:"
echo "  ./stop_nodes.sh"
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "🌐 Network Information:"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
for i in $(seq 0 $((NODES-1))); do
    port=$((9000 + i))
    metrics_port=$((9100 + i))
    echo "  Node $i:"
    echo "    Consensus: 127.0.0.1:$port"
    echo "    Metrics:   http://localhost:$metrics_port/metrics"
done
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

