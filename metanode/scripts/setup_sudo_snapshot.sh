#!/bin/bash

# Script để tự động cấu hình quyền sudo cho lệnh lvm-snap-rsync
# Script này sẽ tạo file trong /etc/sudoers.d/ để cấu hình quyền NOPASSWD

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

print_info() {
    echo -e "${GREEN}ℹ️  $1${NC}"
}

print_warn() {
    echo -e "${YELLOW}⚠️  $1${NC}"
}

print_error() {
    echo -e "${RED}❌ $1${NC}"
}

print_success() {
    echo -e "${GREEN}✅ $1${NC}"
}

print_step() {
    echo -e "${BLUE}📋 $1${NC}"
}

# Get script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
METANODE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Get binary path from config
LVM_SNAPSHOT_BIN_PATH=""

# Check node_0.toml first
if [ -f "$METANODE_ROOT/config/node_0.toml" ]; then
    if grep -q "^enable_lvm_snapshot = true" "$METANODE_ROOT/config/node_0.toml" 2>/dev/null; then
        LVM_SNAPSHOT_BIN_PATH=$(grep "^lvm_snapshot_bin_path" "$METANODE_ROOT/config/node_0.toml" 2>/dev/null | sed 's/.*= *"\(.*\)".*/\1/' | sed "s/^ *//;s/ *$//")
    fi
fi

# If not found, use default
if [ -z "$LVM_SNAPSHOT_BIN_PATH" ]; then
    LVM_SNAPSHOT_BIN_PATH="$METANODE_ROOT/bin/lvm-snap-rsync"
fi

# Check if binary exists
if [ ! -f "$LVM_SNAPSHOT_BIN_PATH" ]; then
    print_error "File lvm-snap-rsync không tồn tại tại: $LVM_SNAPSHOT_BIN_PATH"
    exit 1
fi

# Get absolute path
LVM_SNAPSHOT_BIN_PATH=$(readlink -f "$LVM_SNAPSHOT_BIN_PATH" || echo "$LVM_SNAPSHOT_BIN_PATH")

print_step "Cấu hình quyền sudo cho lệnh snapshot..."
print_info "Binary path: $LVM_SNAPSHOT_BIN_PATH"
print_info "User: $(whoami)"

# Check if already configured
if sudo grep -q "NOPASSWD.*$(basename "$LVM_SNAPSHOT_BIN_PATH")" /etc/sudoers /etc/sudoers.d/* 2>/dev/null; then
    print_success "Quyền sudo đã được cấu hình!"
    exit 0
fi

# Create sudoers.d file
SUDOERS_FILE="/etc/sudoers.d/lvm-snap-rsync-$(whoami)"
SUDOERS_CONTENT="$(whoami) ALL=(ALL) NOPASSWD: $LVM_SNAPSHOT_BIN_PATH"

print_info "Tạo file sudoers: $SUDOERS_FILE"
echo "$SUDOERS_CONTENT" | sudo tee "$SUDOERS_FILE" > /dev/null

# Set correct permissions (sudoers files must be 0440)
sudo chmod 0440 "$SUDOERS_FILE"

# Validate sudoers syntax
if sudo visudo -c -f "$SUDOERS_FILE" >/dev/null 2>&1; then
    print_success "Quyền sudo đã được cấu hình thành công!"
    print_info "File: $SUDOERS_FILE"
    print_info "Nội dung: $SUDOERS_CONTENT"
    
    # Test the configuration
    print_info "Đang kiểm tra quyền sudo..."
    if sudo -n "$LVM_SNAPSHOT_BIN_PATH" --help >/dev/null 2>&1; then
        print_success "✅ Quyền sudo hoạt động đúng! Snapshot sẽ tự động chạy khi epoch transition."
    else
        print_warn "⚠️  Quyền sudo đã được cấu hình nhưng test thất bại. Vui lòng kiểm tra lại."
    fi
else
    print_error "Lỗi cú pháp trong file sudoers! Đang xóa file..."
    sudo rm -f "$SUDOERS_FILE"
    exit 1
fi

