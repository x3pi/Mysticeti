#!/bin/bash
# stop_all.sh — Graceful stop tất cả Go + Rust processes, GIỮ NGUYÊN data
# Dùng script này trước khi chạy resume_keep_keys_with_node4_go.sh
set -e

echo "🛑 Stopping all processes (giữ nguyên dữ liệu)..."

# 1. Gửi SIGTERM trước (graceful shutdown - cho Go flush LevelDB)
echo "📤 Sending SIGTERM for graceful LevelDB flush..."
pkill -f simple_chain 2>/dev/null || true
pkill -f "metanode start" 2>/dev/null || true
pkill -f "metanode run" 2>/dev/null || true
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
    pkill -9 -f "metanode start" 2>/dev/null || true
    pkill -9 -f "metanode run" 2>/dev/null || true
    pkill -9 -f "go run" 2>/dev/null || true
    sleep 1
fi

# 4. Release ports
echo "🔌 Releasing ports..."
fuser -k 10748/tcp 2>/dev/null || true
fuser -k 10649/tcp 2>/dev/null || true
fuser -k 8700/tcp 2>/dev/null || true
fuser -k 9100/tcp 2>/dev/null || true
fuser -k 9101/tcp 2>/dev/null || true

# 5. Kill tmux sessions (processes đã chết, chỉ cleanup tmux)
echo "🗑️ Cleaning up tmux sessions..."
tmux kill-session -t go-master 2>/dev/null || true
tmux kill-session -t go-sub 2>/dev/null || true
tmux kill-session -t go-master-1 2>/dev/null || true
tmux kill-session -t go-sub-1 2>/dev/null || true
tmux kill-session -t go-master-4 2>/dev/null || true
tmux kill-session -t go-sub-4 2>/dev/null || true
for i in 0 1 2 3 4; do
    tmux kill-session -t "metanode-$i" 2>/dev/null || true
done
tmux kill-session -t metanode-1-sep 2>/dev/null || true

# 6. Cleanup unix socket files
echo "🧹 Cleaning up socket files..."
rm -f /tmp/go_master_*.sock 2>/dev/null || true
rm -f /tmp/go_sub_*.sock 2>/dev/null || true
rm -f /tmp/metanode-tx-*.sock /tmp/executor*.sock /tmp/rust-go.sock_* 2>/dev/null || true
rm -f /tmp/rust-go-node1.sock /tmp/rust-go-node1-master.sock /tmp/rust-go-standard-master.sock 2>/dev/null || true
rm -f /tmp/executor4.sock /tmp/rust-go-node4-master.sock 2>/dev/null || true

echo ""
echo "✅ Done! All processes stopped."
echo "   📁 Data directories preserved — ready for resume."
echo "   👉 Run: bash resume_keep_keys_with_node4_go.sh"
