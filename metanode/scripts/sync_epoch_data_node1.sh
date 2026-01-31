#!/bin/bash

set -e

echo "🔄 Syncing epoch data from main network to Node 1..."
echo ""

# Source: Go Master main network (has all epoch data)
SOURCE_BACKUP="/home/abc/chain-n/mtn-simple-2025/cmd/simple_chain/sample/simple/back_up/backup_db"

# Destination: Go Master node 1 (missing epoch data)
DEST_BACKUP="/home/abc/chain-n/mtn-simple-2025/cmd/simple_chain/sample/node1/back_up/backup_db"

if [ ! -d "$SOURCE_BACKUP" ]; then
    echo "❌ Source backup not found: $SOURCE_BACKUP"
    exit 1
fi

echo "📋 Source: $SOURCE_BACKUP"
echo "📋 Destination: $DEST_BACKUP"
echo ""

# Create backup directory if not exists
mkdir -p "$(dirname "$DEST_BACKUP")"

# Copy epoch metadata database
echo "📦 Copying epoch boundary data..."
cp -r "$SOURCE_BACKUP" "$DEST_BACKUP"

echo "✅ Epoch data synced successfully!"
echo ""
echo "📊 Verifying..."
if [ -d "$DEST_BACKUP" ]; then
    echo "✅ $DEST_BACKUP exists"
    ls -lh "$DEST_BACKUP" | head -10
else
    echo "❌ Copy failed!"
    exit 1
fi

echo ""
echo "🚀 Next: Restart Node 1 Go Master and Rust metanode"
echo "  1. tmux kill-session -t go-master-1"
echo "  2. tmux kill-session -t metanode-1-sep"
echo "  3. Restart them - they will now have correct epoch data"
