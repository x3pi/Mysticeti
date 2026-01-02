#!/bin/bash

# Script để chạy tất cả MetaNode consensus nodes trong tmux sessions

set -e

# Get script directory and change to project root
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$PROJECT_ROOT"

# Configuration
NODES=4
BINARY="./target/release/metanode"
CONFIG_DIR="config"
LOG_DIR="logs"

# Optional: reset epoch_timestamp_ms at startup (test-friendly).
# - If epoch_timestamp_ms is old, nodes will instantly propose epoch change on boot.
# - Resetting makes "epoch_duration_seconds" behave like a fresh timer per run.
# Set to 0 to disable: RESET_EPOCH_TIMESTAMP_MS=0 ./run_nodes.sh
RESET_EPOCH_TIMESTAMP_MS="${RESET_EPOCH_TIMESTAMP_MS:-0}"

# Create a per-run log directory so we never lose early startup / epoch-transition logs
RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_LOG_DIR="$LOG_DIR/run-$RUN_ID"

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
mkdir -p "$RUN_LOG_DIR"

# Keep a stable "latest" pointer for convenience (best-effort)
ln -sfn "$(basename "$RUN_LOG_DIR")" "$LOG_DIR/latest" 2>/dev/null || true

# Kill existing sessions
print_info "Cleaning up existing sessions..."
for i in $(seq 0 $((NODES-1))); do
    tmux kill-session -t "metanode-$i" 2>/dev/null && print_info "Killed existing session: metanode-$i" || true
done

sleep 1

# Optionally reset epoch_timestamp_ms in per-node committee files (and shared template, if present).
if [ "$RESET_EPOCH_TIMESTAMP_MS" = "1" ]; then
    print_info "Resetting epoch_timestamp_ms in per-node committee files (test-friendly)..."
    NOW_MS="$(python3 -c 'import time; print(int(time.time()*1000))')"

    for i in $(seq 0 $((NODES-1))); do
        committee_file="$CONFIG_DIR/committee_node_${i}.json"
        if [ -f "$committee_file" ]; then
            python3 - "$committee_file" "$NOW_MS" <<'PY'
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
        else
            print_warn "Missing per-node committee file: $committee_file (skipping reset)"
        fi
    done

    # Best-effort: also refresh shared committee template if it exists.
    if [ -f "$CONFIG_DIR/committee.json" ]; then
        python3 - "$CONFIG_DIR/committee.json" "$NOW_MS" <<'PY'
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
    fi

    print_info "epoch_timestamp_ms reset to NOW_MS=$NOW_MS"
else
    print_info "Keeping existing epoch_timestamp_ms (RESET_EPOCH_TIMESTAMP_MS=0)"
fi

# Start nodes
print_info "Starting $NODES nodes..."
for i in $(seq 0 $((NODES-1))); do
    config_file="$CONFIG_DIR/node_$i.toml"
    # Full log + an "epoch-only" log to grep quickly
    log_file="$RUN_LOG_DIR/node_$i.log"
    epoch_log_file="$RUN_LOG_DIR/node_$i.epoch.log"
    
    if [ ! -f "$config_file" ]; then
        print_error "Config file not found: $config_file"
        exit 1
    fi
    
    print_info "Starting node $i (config: $config_file)..."
    
    # Start node in tmux session with logging
    tmux new-session -d -s "metanode-$i" \
        "RUST_BACKTRACE=1 RUST_LOG=info,metanode=info,consensus_core=info stdbuf -oL -eL $BINARY start --config $config_file 2>&1 | stdbuf -oL -eL tee -a $log_file | stdbuf -oL -eL tee -a >(grep -a -i --line-buffered -E 'epoch|epoch_change|proposal_hash|quorum|transition|committee\\.json|fork' >> $epoch_log_file) >/dev/null"
    
    sleep 1
done

print_info "All nodes started!"
print_info "Logs for this run: $RUN_LOG_DIR"
print_info "Quick epoch logs:  $RUN_LOG_DIR/node_X.epoch.log"
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
echo "  tail -f $LOG_DIR/latest/node_0.log       # Follow node 0 logs (latest run)"
echo "  tail -f $LOG_DIR/latest/node_0.epoch.log # Follow epoch-only logs (latest run)"
echo "  tail -f $LOG_DIR/latest/node_1.log       # Follow node 1 logs"
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

