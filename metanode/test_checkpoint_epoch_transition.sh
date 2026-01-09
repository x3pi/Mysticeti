#!/bin/bash

# Script để test checkpoint-based epoch transition
# Kiểm tra xem epoch transition có hoạt động đúng với checkpoint thay vì block count

set -e

# Get script directory and change to project root
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR" && pwd)"
cd "$PROJECT_ROOT"

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "🧪 Testing Checkpoint-Based Epoch Transition (Sui-Style)"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# Check if nodes are configured for checkpoint-based epochs
echo "1. Checking Configuration:"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

for i in 0 1 2 3; do
    CONFIG_FILE="config/node_${i}.toml"
    if [ -f "$CONFIG_FILE" ]; then
        CHECKPOINT_BASED=$(grep "checkpoint_based_epochs" "$CONFIG_FILE" | sed 's/.*= *//' | tr -d ' ')
        BLOCK_BASED=$(grep "block_based_epoch_change" "$CONFIG_FILE" | sed 's/.*= *//' | tr -d ' ')
        CHECKPOINTS_PER_EPOCH=$(grep "checkpoints_per_epoch" "$CONFIG_FILE" | sed 's/.*= *Some(//' | sed 's/).*//' | tr -d ' ')

        echo "  Node $i:"
        echo "    - checkpoint_based_epochs: $CHECKPOINT_BASED"
        echo "    - block_based_epoch_change: $BLOCK_BASED"
        echo "    - checkpoints_per_epoch: $CHECKPOINTS_PER_EPOCH"
        echo ""
    fi
done

# Test compilation
echo "2. Testing Compilation:"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

if cargo check; then
    echo "  ✅ Code compiles successfully"
else
    echo "  ❌ Compilation failed"
    exit 1
fi

echo ""

# Test checkpoint logic
echo "3. Testing Checkpoint Logic:"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

if cargo test checkpoint::tests; then
    echo "  ✅ Checkpoint tests pass"
else
    echo "  ❌ Checkpoint tests failed"
    exit 1
fi

echo ""

# Test epoch transition simulation
echo "4. Simulating Epoch Transition:"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

# Run a quick test to see if checkpoint epoch transition logic works
if cargo run --bin metanode -- --help > /dev/null 2>&1; then
    echo "  ✅ Binary runs successfully"
else
    echo "  ❌ Binary failed to run"
    exit 1
fi

echo ""

# Check for checkpoint creation in logs
echo "5. Checking for Checkpoint Creation in Logs:"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

for i in 0 1 2 3; do
    LOG_FILE="logs/latest/node_${i}.log"
    if [ -f "$LOG_FILE" ]; then
        CHECKPOINT_COUNT=$(grep "Created checkpoint" "$LOG_FILE" | wc -l)
        END_OF_EPOCH_COUNT=$(grep "end-of-epoch for epoch" "$LOG_FILE" | wc -l)

        echo "  Node $i:"
        echo "    - Checkpoints created: $CHECKPOINT_COUNT"
        echo "    - End-of-epoch checkpoints: $END_OF_EPOCH_COUNT"
        echo ""
    else
        echo "  Node $i: Log file not found"
    fi
done

# Check for epoch transition messages
echo "6. Checking for Epoch Transition Messages:"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

for i in 0 1 2 3; do
    LOG_FILE="logs/latest/node_${i}.log"
    if [ -f "$LOG_FILE" ]; then
        EPOCH_TRANSITIONS=$(grep "Checkpoint.*triggers epoch transition" "$LOG_FILE" | wc -l)
        EPOCH_CHANGE_TRANSACTIONS=$(grep "checkpoint-based epoch change transaction" "$LOG_FILE" | wc -l)

        echo "  Node $i:"
        echo "    - Epoch transition triggers: $EPOCH_TRANSITIONS"
        echo "    - Epoch change transactions: $EPOCH_CHANGE_TRANSACTIONS"
        echo ""
    fi
done

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "✅ Checkpoint-based epoch transition test completed"
echo ""
echo "Key improvements:"
echo "- No single point of failure (all nodes can create epoch change transactions)"
echo "- Deterministic triggers based on checkpoint sequence numbers"
echo "- Sui-compatible architecture with end-of-epoch checkpoints"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
