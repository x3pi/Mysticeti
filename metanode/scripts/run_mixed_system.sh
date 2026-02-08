#!/bin/bash

# Script để chạy Mixed System:
# - Standard System: Nodes 0, 2, 3, 4 (Go Master/Sub standard, Rust Nodes standard)
#   - Node 4 chạy như Full Node (sync-only nếu không trong committee)
# - Special Node 1:  Node 1 Separate (Go Master/Sub separate, Rust Node separate ports)
#
# NodeMode được xác định tự động dựa trên committee membership:
# - Nếu node nằm trong committee -> Validator
# - Nếu node không trong committee -> SyncOnly (Full Node)
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
kill_port 9000; kill_port 9001; kill_port 9002; kill_port 9003; kill_port 9004
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
if [ -z "$KEEP_KEYS" ]; then
    # Clean old configs
    rm -f "$METANODE_ROOT/config/committee.json"
    rm -f "$METANODE_ROOT/config/node_*.toml"
    rm -f "$METANODE_ROOT/config/node_*_protocol_key.json"
    rm -f "$METANODE_ROOT/config/node_*_network_key.json"

    # Generate keys for 5 nodes (0, 1, 2, 3, 4)
    # Node 4 chạy như full node (mode được xác định bởi committee membership)
    print_info "Generating keys for 5 nodes..."
    "$BINARY" generate --nodes 5 --output config
else
    print_info "🔑 Skipping key generation (KEEP_KEYS set)..."
fi

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

# 4b. Update Socket Paths (Standard Nodes 0, 2, 3, 4)
# Updates 0, 2, 3, 4 to connect to Standard Go Master
for id in 0 2 3 4; do
    sed -i 's|executor_receive_socket_path = ".*"|executor_receive_socket_path = "/tmp/rust-go-standard-master.sock"|g' "config/node_$id.toml"
done

# 5. CRITICAL: Enable Executor Commit (Missing in standard Node 1 generation?)
# Node 1 acts as a Master (Validator), so it MUST receive blocks from Rust to Execute.
sed -i 's|executor_commit_enabled = false|executor_commit_enabled = true|g' config/node_1.toml
# Ensure read enabled too
sed -i 's|executor_read_enabled = false|executor_read_enabled = true|g' config/node_1.toml

# 6. CRITICAL: Disable LVM Snapshot for Node 0 (Go handles snapshots via rsync now)
print_info "🔧 Disabling Rust LVM Snapshot for Node 0 (Go uses rsync method)..."
sed -i 's|enable_lvm_snapshot = true|enable_lvm_snapshot = false|g' config/node_0.toml
# Remove obsolete snapshot binary path if present
sed -i '/lvm_snapshot_bin_path/d' config/node_0.toml

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

# Standard sockets are already configured in config-master.json and node_0.toml (via generation template or default)
# We assume node_0.toml points to go-master sockets by default generation logic or previous manual edits.


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
# Node 4 là full node - mode (Validator/SyncOnly) được xác định tự động bởi committee
for id in 0 2 3 4; do
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
# Step 5.5: Update add_validator_node4 config with new keys
# ==============================================================================
print_step "Bước 5.5: Update add_validator_node4 config..."

ADD_VALIDATOR_TOOL_DIR="$GO_PROJECT_ROOT/cmd/tool/add_validator_node4"
if [ -d "$ADD_VALIDATOR_TOOL_DIR" ]; then
    print_info "🔧 Updating add_validator_node4 config from new committee.json..."
    
    # Read chain_id from genesis
    CHAIN_ID=$(python3 -c "import json; print(json.load(open('$GENESIS_OUTPUT'))['config']['chainId'])" 2>/dev/null || echo "991")
    
    # Extract node-4 info from committee.json and update config
    python3 << PYTHON_EOF
import json
import re
import os

committee_path = "$METANODE_ROOT/config/committee.json"
tool_dir = "$ADD_VALIDATOR_TOOL_DIR"
chain_id = $CHAIN_ID

with open(committee_path, 'r') as f:
    committee = json.load(f)

# Find node-4 in authorities (by port 9004 or hostname)
node4 = None
for auth in committee['authorities']:
    hostname = auth.get('hostname', '')
    address = auth.get('address', '')
    if 'node-4' in hostname or 'node_4' in hostname or address.endswith(':9004'):
        node4 = auth
        break

if node4:
    print(f"   Found node-4 in committee.json: {node4.get('hostname')}")
    
    # Update main.go with new keys
    main_go_path = os.path.join(tool_dir, "main.go")
    if os.path.exists(main_go_path):
        with open(main_go_path, 'r') as f:
            content = f.read()
        
        # Replace node4Info struct values
        content = re.sub(r'Address:\s*"[^"]*"', f'Address:      "{node4.get("address", "/ip4/127.0.0.1/tcp/9004")}"', content)
        content = re.sub(r'Hostname:\s*"[^"]*"', f'Hostname:     "{node4.get("hostname", "node-4")}"', content)
        content = re.sub(r'AuthorityKey:\s*"[^"]*"', f'AuthorityKey: "{node4.get("authority_key", "")}"', content)
        content = re.sub(r'ProtocolKey:\s*"[^"]*"', f'ProtocolKey:  "{node4.get("protocol_key", "")}"', content)
        content = re.sub(r'NetworkKey:\s*"[^"]*"', f'NetworkKey:   "{node4.get("network_key", "")}"', content)
        
        with open(main_go_path, 'w') as f:
            f.write(content)
        print("   ✅ Updated main.go with node-4 keys")
    
    # Create config.json with a unique BLS key for node-4
    # HARDCODED VALID KEY AND ADDRESS to avoid SIGSEGV and dependency issues
    node4_bls_key = "6c8489f6f86fea58b26e34c8c37e13e5993651f09f5f96739d9febf65aded718"
    node4_eth_address = "a87c6FD018Da82a52158B0328D61BAc29b556e86".lower()
    
    config = {
        "private_key": node4_bls_key,
        "version": "0.0.1.0",
        "parent_address": "0x0000000000000000000000000000000000000000",
        "parent_connection_address": "127.0.0.1:4200",
        "parent_connection_type": "TCP_CONNECTION",
        "chain_id": chain_id
    }
    
    config_path = os.path.join(tool_dir, "config.json")
    with open(config_path, 'w') as f:
        json.dump(config, f, indent=2)
    print(f"   ✅ Created config.json with HARDCODED valid key")
    print(f"   Chain ID: {chain_id}")
    
    # CRITICAL: Add node-4 address to genesis.json alloc with balance
    try:
        # Add to genesis.json alloc
        genesis_path = "$GENESIS_OUTPUT"
        with open(genesis_path, 'r') as f:
            genesis = json.load(f)
        
        # Add node-4 address with balance
        node4_balance = "2000000000000000000000000000000"  # Same as other validators
        node4_bls_pub = "0x86d5de6f7c9c13cc0d959a553cc0e4853ba5faae45a28da9bddc8ef8e104eb5d3dece8dfaa24f11b4243ec27537e3184"
        
        # HARDCODED OLD ADDR TO REMOVE
        old_node4_addr = "c98223c939f0313d5b5dace9c3c3759af4de663a".lower()

        # Check if alloc exists or is list
        if 'alloc' not in genesis:
             genesis['alloc'] = []
             
        # Filter out OLD incorrect address FIRST if it exists
        genesis['alloc'] = [entry for entry in genesis['alloc'] if entry.get('address','').lower().replace('0x','') != old_node4_addr]
             
        # Check if new address present
        found = False
        for entry in genesis['alloc']:
             if entry['address'].lower().replace('0x','') == node4_eth_address:
                 entry['balance'] = node4_balance
                 entry['publicKeyBls'] = node4_bls_pub
                 found = True
                 break
        
        if not found:
             genesis['alloc'].append({
                 "address": "0x" + node4_eth_address,
                 "balance": node4_balance,
                 "pending_balance": "0",
                 "last_hash": "0x0000000000000000000000000000000000000000000000000000000000000000",
                 "device_key": "0x0000000000000000000000000000000000000000000000000000000000000000",
                 "publicKeyBls": node4_bls_pub
             })
        
        with open(genesis_path, 'w') as f:
            json.dump(genesis, f, indent=2)
        
        print(f"   ✅ Removed old node-4 ({old_node4_addr}) if existed")
        print(f"   ✅ Added/Updated node-4 address 0x{node4_eth_address} to genesis.json with balance")
    except Exception as e:
        print(f"   ⚠️  Could not add node-4 to genesis: {e}")
else:
    print("   ⚠️  node-4 not found in committee.json")
PYTHON_EOF

    # Rebuild the tool
    print_info "🔨 Rebuilding add_validator_node4..."
    cd "$ADD_VALIDATOR_TOOL_DIR"
    go build -o add_validator_node4 . 2>&1 && print_info "   ✅ Tool rebuilt successfully" || print_warn "   Build failed, manual rebuild needed"
    cd "$METANODE_ROOT"
fi

# ==============================================================================
# Step 6: Summary
# ==============================================================================

echo ""
print_info "=========================================="
print_info "🎉 MIXED SYSTEM STARTED SUCCESSFULLY!"
print_info "=========================================="
echo ""
print_info "📊 Standard System (Nodes 0, 2, 3, 4):"
print_info "  - Rust Node 0: tmux attach -t metanode-0"
print_info "  - Rust Node 2: tmux attach -t metanode-2"
print_info "  - Rust Node 3: tmux attach -t metanode-3"
print_info "  - Rust Node 4: tmux attach -t metanode-4 (Full Node)"
print_info "  - Go Master:   tmux attach -t go-master"
print_info "  - Go Sub:      tmux attach -t go-sub"
print_info ""
print_info "📊 Node 1 Separate System (Node 1):"
print_info "  - Rust: tmux attach -t metanode-1-sep"
print_info "  - Go:   tmux attach -t go-master-1 / go-sub-1"
echo ""
print_info "Log files in $LOG_DIR/*.log"
echo ""
print_info "📝 To register Node-4 as validator:"
print_info "  cd $GO_PROJECT_ROOT/cmd/tool/add_validator_node4"
print_info "  ./add_validator_node4 config.json"
echo ""
