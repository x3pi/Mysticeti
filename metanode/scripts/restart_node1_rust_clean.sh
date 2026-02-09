#!/bin/bash

set -e

echo "🛑 Stopping metanode-1-sep..."
tmux kill-session -t metanode-1-sep 2>/dev/null || true

sleep 2

echo "🗑️  Removing node_1 Rust storage (to regenerate genesis with fixed validator order)..."
rm -rf /home/abc/chain-n/Mysticeti/metanode/config/storage/node_1/

echo "✅ Node 1 storage cleaned!"
echo ""
echo "🚀 Restarting metanode-1-sep with fixed genesis generation..."

cd /home/abc/chain-n/Mysticeti/metanode

BINARY="/home/abc/chain-n/Mysticeti/metanode/target/release/metanode"
LOG_DIR="/home/abc/chain-n/Mysticeti/metanode/logs"

tmux new-session -d -s metanode-1-sep -c "/home/abc/chain-n/Mysticeti/metanode" \
    "export RUST_LOG=info,consensus_core=debug; $BINARY start --config config/node_1_separate.toml 2>&1 | tee \"$LOG_DIR/metanode-1-sep.log\""

echo "✅ Metanode-1-sep restarted!"
echo ""
echo "📋 Check logs:"
echo "  tail -f $LOG_DIR/metanode-1-sep.log"
echo ""
echo "🔍 Verify genesis blocks match network:"
echo "  Should see 'Generated 4 genesis blocks for epoch 1' with correct validator order"
