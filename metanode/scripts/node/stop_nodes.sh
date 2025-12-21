#!/bin/bash

# Script để dừng tất cả MetaNode consensus nodes

set -e

# Get script directory and change to project root
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$PROJECT_ROOT"

NODES=4

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

print_info() {
    echo -e "${GREEN}ℹ️  $1${NC}"
}

print_warn() {
    echo -e "${YELLOW}⚠️  $1${NC}"
}

print_info "Stopping all MetaNode consensus nodes..."

stopped=0
for i in $(seq 0 $((NODES-1))); do
    if tmux has-session -t "metanode-$i" 2>/dev/null; then
        tmux kill-session -t "metanode-$i" 2>/dev/null && {
            print_info "Stopped node $i"
            stopped=$((stopped + 1))
        } || print_warn "Failed to stop node $i"
    else
        print_warn "Node $i not running"
    fi
done

if [ $stopped -eq 0 ]; then
    print_warn "No nodes were running"
else
    print_info "Stopped $stopped node(s)"
fi

# Also kill any remaining metanode processes
pkill -f "metanode start" 2>/dev/null && print_info "Killed remaining processes" || true

print_info "All nodes stopped"

