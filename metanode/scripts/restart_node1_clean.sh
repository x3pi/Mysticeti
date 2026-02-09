#!/bin/bash

set -e

echo "🛑 Stopping node 1 processes..."
pkill -f "config-master-node1" || true
pkill -f "config-sub-node1" || true  
pkill -f "node_1_separate" || true

sleep 2

echo "🗑️  Cleaning node 1 databases..."
rm -rf /home/abc/chain-n/Mysticeti/metanode/config/storage/node_1/
rm -rf /home/abc/chain-n/mtn-simple-2025/cmd/simple_chain/sample/node1/data/
rm -rf /home/abc/chain-n/mtn-simple-2025/cmd/simple_chain/sample/node1/back_up/

echo "✅ Node 1 cleaned. Ready to restart with new binary!"
echo ""
echo "Next steps:"
echo "1. Restart Go Master node 1 in tmux session 'go-master-1'"
echo "2. Restart Go Sub node 1 in tmux session 'go-sub-1'"  
echo "3. Restart Rust metanode-1-sep in tmux session 'metanode-1-sep'"
