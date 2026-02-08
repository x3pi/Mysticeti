#!/bin/bash
# kill_all.sh — Kill tất cả Go + Rust processes và clear data
set -e

echo "🛑 Stopping all processes..."

# 1. Gửi SIGTERM trước (graceful shutdown)
pkill -f simple_chain 2>/dev/null || true
pkill -f metanode 2>/dev/null || true
pkill -f "cmd.*config.*json" 2>/dev/null || true
# Kill go run processes (Go compiler spawns temp binaries)
pkill -f "go run" 2>/dev/null || true

# 2. Đợi processes dừng (tối đa 5 giây)
echo "⏳ Waiting for processes to stop (5s)..."
for i in $(seq 1 10); do
    if ! pgrep -f "simple_chain|metanode" > /dev/null 2>&1; then
        echo "   ✅ All processes stopped."
        break
    fi
    sleep 0.5
done

# 3. Force kill nếu còn sót
if pgrep -f "simple_chain|metanode" > /dev/null 2>&1; then
    echo "   ⚠️ Force killing remaining processes..."
    pkill -9 -f simple_chain 2>/dev/null || true
    pkill -9 -f metanode 2>/dev/null || true
    pkill -9 -f "cmd.*config.*json" 2>/dev/null || true
    pkill -9 -f "go run" 2>/dev/null || true
    sleep 1
fi

# 4. Force kill processes on common ports
echo "🔌 Releasing ports..."
fuser -k 10748/tcp 2>/dev/null || true
fuser -k 10649/tcp 2>/dev/null || true
# Snapshot server port
fuser -k 8700/tcp 2>/dev/null || true
# Peer RPC ports (nếu dùng)
fuser -k 9100/tcp 2>/dev/null || true
fuser -k 9101/tcp 2>/dev/null || true

# 5. Cleanup unix socket files
echo "🧹 Cleaning up socket files..."
rm -f /tmp/go_master_*.sock 2>/dev/null || true
rm -f /tmp/go_sub_*.sock 2>/dev/null || true
rm -f /tmp/*.sock 2>/dev/null || true

# 6. Clear data cũ (XÓA DỮ LIỆU!)
echo "🗑️ Clearing old data..."
rm -rf /home/abc/chain-n/mtn-simple-2025/cmd/simple_chain/sample/*/data
rm -rf /home/abc/chain-n/Mysticeti/metanode/data-*

# 7. Clear epoch backup files
rm -f /tmp/epoch_data_backup*.json

# 8. Clear snapshot data (nếu có)
rm -rf /home/abc/chain-n/mtn-simple-2025/cmd/simple_chain/snapshot_data 2>/dev/null || true
rm -rf /home/abc/chain-n/mtn-simple-2025/cmd/simple_chain/sample/*/data_snap 2>/dev/null || true

echo "✅ Done! All processes killed and data cleared."
echo "   Ready to restart fresh."