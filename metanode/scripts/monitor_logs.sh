#!/bin/bash

# Simple script to monitor all 4 Go logs in split terminal view
# Uses tail -f to follow log files instead of attaching to sessions

# Colors
GREEN='\033[0;32m'
BLUE='\033[0;34m'
RED='\033[0;31m'
NC='\033[0m'

print_info() { echo -e "${GREEN}ℹ️  $1${NC}"; }
print_error() { echo -e "${RED}❌ $1${NC}"; }

MONITOR_SESSION="log-monitor"
LOG_DIR="/home/abc/nhat/consensus-chain/Mysticeti/metanode/logs"

# Check if log files exist
LOG_FILES=("go-master.log" "go-sub.log" "go-master-1.log" "go-sub-1.log")
missing_logs=()

for log_file in "${LOG_FILES[@]}"; do
    if [ ! -f "$LOG_DIR/$log_file" ]; then
        missing_logs+=("$log_file")
    fi
done

if [ ${#missing_logs[@]} -gt 0 ]; then
    print_error "Missing log files: ${missing_logs[*]}"
    print_info "Make sure the system is running first"
    exit 1
fi

# Kill existing monitor session if it exists
tmux kill-session -t "$MONITOR_SESSION" 2>/dev/null || true

print_info "Creating log monitor session..."

# Create new session
tmux new-session -d -s "$MONITOR_SESSION"

# Split into 4 panes
tmux split-window -h -t "$MONITOR_SESSION"
tmux split-window -v -t "$MONITOR_SESSION:0.0"  
tmux split-window -v -t "$MONITOR_SESSION:0.2"

# Setup log monitoring in each pane
tmux send-keys -t "$MONITOR_SESSION:0.0" "echo -e '\033[1;32m=== GO MASTER LOG ===\033[0m' && tail -f $LOG_DIR/go-master.log" Enter
tmux send-keys -t "$MONITOR_SESSION:0.1" "echo -e '\033[1;33m=== GO SUB LOG ===\033[0m' && tail -f $LOG_DIR/go-sub.log" Enter  
tmux send-keys -t "$MONITOR_SESSION:0.2" "echo -e '\033[1;34m=== GO MASTER-1 LOG ===\033[0m' && tail -f $LOG_DIR/go-master-1.log" Enter
tmux send-keys -t "$MONITOR_SESSION:0.3" "echo -e '\033[1;35m=== GO SUB-1 LOG ===\033[0m' && tail -f $LOG_DIR/go-sub-1.log" Enter

# Select first pane
tmux select-pane -t "$MONITOR_SESSION:0.0"

print_info "✅ Log monitor created with 4 panes:"
print_info "  Top-left:     go-master.log"
print_info "  Top-right:    go-sub.log"
print_info "  Bottom-left:  go-master-1.log" 
print_info "  Bottom-right: go-sub-1.log"
print_info ""
print_info "🔧 Controls:"
print_info "  Ctrl+B + Arrow: Switch panes"
print_info "  Ctrl+B + z:     Zoom pane" 
print_info "  Ctrl+C:         Stop tail in current pane"
print_info "  Ctrl+B + d:     Detach session"

# Attach to session
if [ -z "$TMUX" ]; then
    print_info "Attaching to log monitor..."
    tmux attach-session -t "$MONITOR_SESSION"
else
    print_info "Use: tmux attach -t $MONITOR_SESSION"
fi