# 1. Stop tất cả Go và Rust processes
pkill -f simple_chain
pkill -9 -f metanode
# Kill 'cmd' binary from 'go run .' 
pkill -9 -f "cmd.*config.*json"
# Force kill any process on common ports
fuser -k 10748/tcp 2>/dev/null || true
fuser -k 10649/tcp 2>/dev/null || true

# 2. Clear data cũ (XÓA DỮ LIỆU!)
rm -rf /home/abc/chain-n/mtn-simple-2025/cmd/simple_chain/sample/*/data
rm -rf /home/abc/chain-n/Mysticeti/metanode/data-*

# 3. Clear epoch backup files
rm -f /tmp/epoch_data_backup*.json

# 4. Restart fresh với scripts bình thường