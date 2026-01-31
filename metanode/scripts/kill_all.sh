# 1. Stop tất cả
pkill -f simple_chain
pkill -9 -f metanode

# 2. Clear data cũ (XÓA DỮ LIỆU!)
rm -rf /home/abc/chain-n/mtn-simple-2025/cmd/simple_chain/sample/*/data
rm -rf /home/abc/chain-n/Mysticeti/metanode/data-*

# 3. Clear epoch backup files
rm -f /tmp/epoch_data_backup*.json

# 4. Restart fresh với scripts bình thường