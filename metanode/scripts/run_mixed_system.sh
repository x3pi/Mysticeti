#!/bin/bash

# Script để chạy Mixed System:
# - Standard System: Nodes 0, 2, 3, 4 (Go Master/Sub standard, Rust Nodes standard)
# - Special Node 1:  Node 1 Separate (Go Master/Sub separate, Rust Node separate ports)
#
# Mục đích: Test khả năng tương tác giữa node chạy port riêng (Node 1) với hệ thống chuẩn.

set -e
set -o pipefail

# Full clean switches
FULL_CLEAN_BUILD="${FULL_CLEAN_BUILD:-1}"
FULL_CLEAN_GO_MODCACHE="${FULL_CLEAN_GO_MODCACHE:-0}"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Paths
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
METANODE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
MYSTICETI_ROOT="$(cd "$METANODE_ROOT/.." && pwd)"
GO_PROJECT_ROOT="$(cd "$METANODE_ROOT/../.." && pwd)/mtn-simple-2025"
LOG_DIR="$METANODE_ROOT/logs"

print_info() { echo -e "${GREEN}ℹ️  $1${NC}"; }
print_warn() { echo -e "${YELLOW}⚠️  $1${NC}"; }
print_error() { echo -e "${RED}❌ $1${NC}"; }
print_step() { echo -e "${BLUE}📋 $1${NC}"; }

if [ ! -d "$GO_PROJECT_ROOT" ]; then
    print_error "Cannot find Go project at $GO_PROJECT_ROOT"
    exit 1
fi

# ==============================================================================
# Step 1: Clean Data (BOTH Standard AND Separate Node 1)
# ==============================================================================
print_step "Bước 1: Cleanup dữ liệu toàn bộ hệ thống..."

# 1.1 Clean Standard Data (Logic from run_full_system.sh)
print_info "🧹 Cleaning Standard Data..."
mkdir -p "$LOG_DIR"
rm -f /tmp/metanode-tx-*.sock /tmp/executor*.sock /tmp/rust-go.sock_* 2>/dev/null || true
rm -rf "$GO_PROJECT_ROOT/cmd/simple_chain/sample/simple"
rm -rf "$METANODE_ROOT/config/storage"
mkdir -p "$METANODE_ROOT/config/storage"
mkdir -p "$GO_PROJECT_ROOT/cmd/simple_chain/sample/simple/data/data/xapian_node"
mkdir -p "$GO_PROJECT_ROOT/cmd/simple_chain/sample/simple/data-write/data/xapian_node"

# 1.2 Clean Node 1 Separate Data
print_info "🧹 Cleaning Node 1 Data..."
rm -f /tmp/rust-go-node1.sock /tmp/executor1.sock 2>/dev/null || true
rm -rf "$GO_PROJECT_ROOT/cmd/simple_chain/sample/node1"
rm -rf "$METANODE_ROOT/config/storage/node_1"
mkdir -p "$GO_PROJECT_ROOT/cmd/simple_chain/sample/node1/data/data/xapian_node"
mkdir -p "$GO_PROJECT_ROOT/cmd/simple_chain/sample/node1/data-write/data/xapian_node"
mkdir -p "$METANODE_ROOT/config/storage" # storage root is same parent

# ==============================================================================
# Step 2: Stop ALL Running Nodes
# ==============================================================================
print_step "Bước 2: Dừng tất cả nodes cũ..."

# 2.1 Stop Standard Sessions
tmux kill-session -t go-master 2>/dev/null || true
tmux kill-session -t go-sub 2>/dev/null || true
for i in 0 1 2 3 4; do
    tmux kill-session -t "metanode-$i" 2>/dev/null || true
done

# 2.2 Stop Separate Node 1 Sessions
tmux kill-session -t go-master-1 2>/dev/null || true
tmux kill-session -t go-sub-1 2>/dev/null || true
tmux kill-session -t metanode-1-sep 2>/dev/null || true

# 2.3 Force Kill Ports and Processes
kill_port() {
    local port=$1
    local pids=$(lsof -ti :$port 2>/dev/null || true)
    if [ -n "$pids" ]; then
        print_warn "Killing process on port $port: $pids"
        kill -9 $pids 2>/dev/null || true
    fi
}
# Kill by process name (safety net)
print_info "🔪 Force killing any lingering metanode processes..."
pkill -9 -f "metanode start" 2>/dev/null || true
pkill -9 -f "metanode run" 2>/dev/null || true

# Standard ports
# NO CHANGE NEEDED for Go Code here, as Go communicates via Sockets.
# However, I should kill port 9001 explicitly in cleanup.
kill_port 9000; kill_port 9001; kill_port 9002; kill_port 9003; # kill_port 9004
# Node 1 Separate ports
kill_port 9011
kill_port 10747; kill_port 10000; kill_port 6201; kill_port 9081
kill_port 10646; kill_port 10001; kill_port 6200

# ==============================================================================
# Step 3: Global Setup & Build
# ==============================================================================
print_step "Bước 3: Setup & Build..."
cd "$METANODE_ROOT"

# Ensure aggressive cleanup of ALL possible storage locations
print_info "🧹 Deep cleaning storage..."
rm -rf "$METANODE_ROOT/config/storage"
rm -rf "$METANODE_ROOT/storage" # Fallback if config points here
mkdir -p "$METANODE_ROOT/config/storage"

# 3.1 Build Rust
if [ "$FULL_CLEAN_BUILD" = "1" ] || [ ! -f "$METANODE_ROOT/target/release/metanode" ]; then
    print_info "Building Rust binary..."
    cargo build --release --bin metanode
fi
BINARY="$METANODE_ROOT/target/release/metanode"

# 3.2 Update Committee & Genesis (Standard Logic)
# Clean old configs
rm -f "$METANODE_ROOT/config/committee.json"
rm -f "$METANODE_ROOT/config/node_*.toml"
rm -f "$METANODE_ROOT/config/node_*_protocol_key.json"
rm -f "$METANODE_ROOT/config/node_*_network_key.json"

# Generate keys for 4 nodes (0, 1, 2, 3) + 1 (node 4 not used in this script but standard generates 5 usually? Script says --nodes 4 which means 4 nodes: 0,1,2,3)
# The original script said --nodes 4.
print_info "Generating keys for 4 nodes..."
"$BINARY" generate --nodes 4 --output config

# DO NOT Restore old backup. Instead, PATCH node_1.toml directly.
# This ensures it has the exact same structure/params as standard nodes.

# Patch node_1.toml for custom ports/sockets/paths
# 1. Update Network Port (9001 -> 9011)
sed -i 's/127.0.0.1:9001/127.0.0.1:9011/g' config/node_1.toml
# 2. Update Metrics Port (9101 -> 9111) - heuristic replacement
sed -i 's/metrics_port = 9101/metrics_port = 9111/g' config/node_1.toml
# 3. Update Storage Path (Already correct as config/storage/node_1, unless we want to change it? Keep standard)
# sed -i 's|config/storage/node_1"|config/storage/node_1_separate"|g' config/node_1_separate.toml

# 4. Update Socket Paths (Node 1)
# Fix SEND socket (Rust listens here) - should be unique
sed -i 's|executor_send_socket_path = ".*"|executor_send_socket_path = "/tmp/executor1-sep.sock"|g' config/node_1.toml
# Fix RECEIVE socket (Rust connects to Go here) - MUST match Go Master 1
sed -i 's|executor_receive_socket_path = ".*"|executor_receive_socket_path = "/tmp/rust-go-node1-master.sock"|g' config/node_1.toml

# 4b. Update Socket Paths (Standard Nodes 0, 2, 3)
# Updates 0, 2, 3 to connect to Standard Go Master
for id in 0 2 3; do
    sed -i 's|executor_receive_socket_path = ".*"|executor_receive_socket_path = "/tmp/rust-go-standard-master.sock"|g' "config/node_$id.toml"
done

# 5. CRITICAL: Enable Executor Commit (Missing in standard Node 1 generation?)
# Node 1 acts as a Master (Validator), so it MUST receive blocks from Rust to Execute.
sed -i 's|executor_commit_enabled = false|executor_commit_enabled = true|g' config/node_1.toml
# Ensure read enabled too
sed -i 's|executor_read_enabled = false|executor_read_enabled = true|g' config/node_1.toml

# 6. CRITICAL: Enable LVM Snapshot for Node 0 (Primary node creates epoch snapshots)
print_info "🔧 Enabling LVM Snapshot for Node 0..."
sed -i 's|enable_lvm_snapshot = false|enable_lvm_snapshot = true|g' config/node_0.toml
# Add snapshot binary path if not present
if ! grep -q "lvm_snapshot_bin_path" config/node_0.toml; then
    sed -i '/enable_lvm_snapshot = true/a lvm_snapshot_bin_path = "/home/abc/chain-n/Mysticeti/lvm-manager/target/release/lvm-snap-rsync"' config/node_0.toml
else
    sed -i 's|lvm_snapshot_bin_path = .*|lvm_snapshot_bin_path = "/home/abc/chain-n/Mysticeti/lvm-manager/target/release/lvm-snap-rsync"|g' config/node_0.toml
fi

if [ -f "scripts/update_committee_from_genesis.py" ]; then
    if python3 "scripts/update_committee_from_genesis.py"; then
        print_info "✅ Updated committee from genesis"
    else
        print_warn "⚠️  Failed to update committee from genesis (likely validator count mismatch), continuing..."
    fi
fi

# CRITICAL: Patch committee.json to match Node 1 Separate Port (9011)
# Standard generation produces 9001. We must update it so ALL nodes expect Node 1 at 9011.
# This ensures Genesis Block hash calculation is consistent/deterministic across all nodes.
print_info "🔧 Patching committee.json for Node 1 port 9011..."
sed -i 's|/ip4/127.0.0.1/tcp/9001|/ip4/127.0.0.1/tcp/9011|g' config/committee.json


# Generate/Update Genesis.json (Standard)
print_info "Updating genesis.json..."
GENESIS_OUTPUT="$GO_PROJECT_ROOT/cmd/simple_chain/genesis.json"

# FORCE PATCH Genesis BEFORE sync (or after, but purely string replacement is safest to ensure consistency)
# We want to ensure that if genesis exists, it ALSO has 9011 for Node 1 BEFORE Go starts.
# Although the python script might overwrite it, let's try to patch it AFTER the python script or ensure the python script picks it up.
# Actually, the best place is after the python script runs, to overwrite whatever it did.

if [ -f "$GENESIS_OUTPUT" ] && grep -q '"alloc"' "$GENESIS_OUTPUT"; then
    python3 "scripts/sync_committee_to_genesis.py" "config/committee.json" "$GENESIS_OUTPUT"
else
    bash "scripts/generate_genesis_from_rust_keys.sh" "config" "$GENESIS_OUTPUT"
fi

# CRITICAL: Force Patch Genesis to 9011 for Node 1
# The python script might not validly update the address if it only syncs keys/stakes.
# We explicitly replace port 9001 -> 9011 for Node 1's entry if it exists.
# We target the specific string "/ip4/127.0.0.1/tcp/9001" to be safe.
print_info "🔧 Force patching genesis.json for Node 1 port 9011..."
sed -i 's|/ip4/127.0.0.1/tcp/9001|/ip4/127.0.0.1/tcp/9011|g' "$GENESIS_OUTPUT"
# CRITICAL: Sync epoch_timestamp_ms from genesis.json BACK to committee.json
# The sync_committee_to_genesis.py already sets epoch_timestamp_ms in genesis.json
# We must ensure committee.json has the IDENTICAL timestamp for genesis hash consistency
print_info "🔧 Syncing epoch_timestamp_ms from genesis to committee..."
python3 -c "
import json
genesis_path = '$GENESIS_OUTPUT'
committee_path = 'config/committee.json'
with open(genesis_path, 'r') as f:
    genesis = json.load(f)
with open(committee_path, 'r') as f:
    committee = json.load(f)
# Sync timestamp from genesis to committee
if 'config' in genesis and 'epoch_timestamp_ms' in genesis['config']:
    committee['epoch_timestamp_ms'] = genesis['config']['epoch_timestamp_ms']
    with open(committee_path, 'w') as f:
        json.dump(committee, f, indent=2)
    print(f'   ✅ Synced epoch_timestamp_ms to committee.json: {committee[\"epoch_timestamp_ms\"]}')
"

# ==============================================================================
# Step 4: Start Go Processes
# ==============================================================================
print_step "Bước 4: Start Go Processes (Standard + Node 1)..."
cd "$GO_PROJECT_ROOT/cmd/simple_chain"

# CRITICAL: Create shared epoch backup file BEFORE Go Masters start
# This ensures ALL Go Masters use the SAME epoch_timestamp_ms from genesis.json
# instead of creating their own timestamp based on startup time
print_info "🔧 Creating shared epoch backup file with genesis timestamp..."
python3 -c "
import json
genesis_path = '$GENESIS_OUTPUT'
backup_path = '/tmp/epoch_data_backup.json'
with open(genesis_path, 'r') as f:
    genesis = json.load(f)
epoch_timestamp_ms = genesis.get('config', {}).get('epoch_timestamp_ms', 0)
if epoch_timestamp_ms > 0:
    epoch_data = {
        'current_epoch': 0,
        'epoch_start_timestamp_ms': epoch_timestamp_ms,
        'epoch_start_timestamps': {'0': epoch_timestamp_ms}
    }
    with open(backup_path, 'w') as f:
        json.dump(epoch_data, f)
    print(f'   ✅ Created {backup_path} with epoch_timestamp_ms={epoch_timestamp_ms}')
else:
    print(f'   ❌ No epoch_timestamp_ms found in genesis, Go Masters will use current time!')
"



# 4.1 Start Standard Go Master
print_info "🚀 Starting Standard Go Master (go-master)..."
tmux new-session -d -s go-master -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/simple/data/data/xapian_node' && go run . -config=config-master.json 2>&1 | tee \"$LOG_DIR/go-master.log\""
sleep 5

# 4.2 Start Standard Go Sub
print_info "🚀 Starting Standard Go Sub (go-sub)..."
tmux new-session -d -s go-sub -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/simple/data-write/data/xapian_node' && go run . -config=config-sub-write.json 2>&1 | tee \"$LOG_DIR/go-sub.log\""
sleep 5

# 4.3 Start Node 1 Go Master
# 4.3b Patch Go Master 1 Config to match Rust Socket
# Rust sends execution/commits to executor_send_socket_path ("/tmp/executor1-sep.sock")
# Go must listen/connect to this same path.
print_info "🔧 Patching config-master-node1.json socket to match Rust..."
sed -i 's|"/tmp/rust-go.sock_1"|"/tmp/executor1-sep.sock"|g' "$GO_PROJECT_ROOT/cmd/simple_chain/config-master-node1.json"

print_info "🚀 Starting Node 1 Go Master (go-master-1)..."
tmux new-session -d -s go-master-1 -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/node1/data/data/xapian_node' && go run . -config=config-master-node1.json 2>&1 | tee \"$LOG_DIR/go-master-1.log\""
sleep 5

# 4.4 Start Node 1 Go Sub
print_info "🚀 Starting Node 1 Go Sub (go-sub-1)..."
tmux new-session -d -s go-sub-1 -c "$GO_PROJECT_ROOT/cmd/simple_chain" \
    "export GOTOOLCHAIN=go1.23.5 && export XAPIAN_BASE_PATH='sample/node1/data-write/data/xapian_node' && go run . -config=config-sub-node1.json 2>&1 | tee \"$LOG_DIR/go-sub-1.log\""
sleep 5

print_info "⏳ Waiting for Go nodes to stabilize (10s)..."
sleep 10

# ==============================================================================
# Step 5: Start Rust Nodes
# ==============================================================================
print_step "Bước 5: Start Rust Consensus Nodes..."
cd "$METANODE_ROOT"

# 5.1 Start Standard Nodes (0, 2, 3, 4)
for id in 0 2 3; do
    print_info "🚀 Starting Rust Node $id (Standard)..."
    tmux new-session -d -s "metanode-$id" -c "$METANODE_ROOT" \
        "export RUST_LOG=info,consensus_core=debug; $BINARY start --config config/node_$id.toml 2>&1 | tee \"$LOG_DIR/metanode-$id.log\""
    sleep 1
done

# 5.2 Start Node 1 (Separate Config)
print_info "🚀 Starting Rust Node 1 (Separate)..."
# Uses node_1.toml (patched)
tmux new-session -d -s "metanode-1-sep" -c "$METANODE_ROOT" \
    "export RUST_LOG=info,consensus_core=debug; $BINARY start --config config/node_1.toml 2>&1 | tee \"$LOG_DIR/metanode-1-sep.log\""

print_info "⏳ Waiting for Rust nodes to start..."
sleep 5

# ==============================================================================
# Step 6: Summary
# ==============================================================================
echo ""
print_info "=========================================="
print_info "🎉 MIXED SYSTEM STARTED SUCCESSFULLY!"
print_info "=========================================="
echo ""
print_info "📊 Standard System (Nodes 0, 2, 3, 4):"
print_info "  - Rust: tmux attach -t metanode-0 (etc)"
print_info "  - Go:   tmux attach -t go-master / go-sub"
print_info ""
print_info "📊 Node 1 Separate System (Node 1):"
print_info "  - Rust: tmux attach -t metanode-1-sep"
print_info "  - Go:   tmux attach -t go-master-1 / go-sub-1"
echo ""
print_info "Log files in $LOG_DIR/*.log"
