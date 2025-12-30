#!/bin/bash

# Script để setup Move dependencies từ Sui repository
# Các Move crates cần thiết:
#   - move-binary-format
#   - move-core-types
#   - move-vm-config

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

print_step() {
    echo -e "${BLUE}📋 $1${NC}"
}

# Get script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Mysticeti root is one level up from scripts/
MYSTICETI_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SUI_DIR="$MYSTICETI_ROOT/sui"
MOVE_CRATES_DIR="$SUI_DIR/external-crates/move/crates"

print_step "Setup Move Dependencies từ Sui Repository"

# Check if sui directory exists and has content
if [ -d "$SUI_DIR" ] && [ "$(ls -A $SUI_DIR 2>/dev/null)" ]; then
    print_info "Thư mục sui đã tồn tại và có nội dung"
    
    # Check if Move crates already exist
    if [ -d "$MOVE_CRATES_DIR/move-binary-format" ] && \
       [ -d "$MOVE_CRATES_DIR/move-core-types" ] && \
       [ -d "$MOVE_CRATES_DIR/move-vm-config" ]; then
        print_info "✅ Các Move crates đã tồn tại:"
        print_info "   - move-binary-format"
        print_info "   - move-core-types"
        print_info "   - move-vm-config"
        print_info "   Không cần setup lại"
        exit 0
    fi
fi

# Check if git is available
if ! command -v git &> /dev/null; then
    print_error "Git không được cài đặt. Vui lòng cài đặt git trước."
    exit 1
fi

print_step "Bước 1: Clone Sui repository (nếu chưa có)..."

if [ ! -d "$SUI_DIR" ] || [ ! "$(ls -A $SUI_DIR 2>/dev/null)" ]; then
    print_info "Thư mục sui không tồn tại hoặc rỗng, đang clone Sui repository..."
    print_warn "⚠️  Quá trình này có thể mất vài phút vì Sui repository rất lớn"
    
    cd "$MYSTICETI_ROOT"
    
    # Clone only the necessary parts using sparse checkout
    print_info "Đang clone Sui repository với sparse checkout (chỉ lấy Move crates)..."
    
    if [ ! -d "$SUI_DIR" ]; then
        mkdir -p "$SUI_DIR"
        cd "$SUI_DIR"
        git init
        git remote add origin https://github.com/MystenLabs/sui.git
        git config core.sparseCheckout true
        
        # Configure sparse checkout to only get Move crates
        echo "external-crates/move/crates/move-binary-format/*" > .git/info/sparse-checkout
        echo "external-crates/move/crates/move-core-types/*" >> .git/info/sparse-checkout
        echo "external-crates/move/crates/move-vm-config/*" >> .git/info/sparse-checkout
        
        print_info "Đang pull từ Sui repository (main branch)..."
        git pull --depth=1 origin main
        
        if [ $? -ne 0 ]; then
            print_error "Lỗi khi clone Sui repository"
            print_info "Thử cách khác: clone toàn bộ repository..."
            cd "$MYSTICETI_ROOT"
            rm -rf "$SUI_DIR"
            git clone --depth=1 https://github.com/MystenLabs/sui.git "$SUI_DIR"
        fi
    else
        cd "$SUI_DIR"
        if [ -d ".git" ]; then
            print_info "Đang pull updates từ Sui repository..."
            git pull origin main || true
        else
            print_error "Thư mục sui tồn tại nhưng không phải git repository"
            print_info "Xóa và clone lại..."
            cd "$MYSTICETI_ROOT"
            rm -rf "$SUI_DIR"
            git clone --depth=1 https://github.com/MystenLabs/sui.git "$SUI_DIR"
        fi
    fi
else
    print_info "Thư mục sui đã tồn tại, đang kiểm tra Move crates..."
fi

print_step "Bước 2: Kiểm tra Move crates..."

REQUIRED_CRATES=(
    "move-binary-format"
    "move-core-types"
    "move-vm-config"
)

MISSING_CRATES=()

for crate in "${REQUIRED_CRATES[@]}"; do
    CRATE_PATH="$MOVE_CRATES_DIR/$crate"
    if [ ! -d "$CRATE_PATH" ] || [ ! -f "$CRATE_PATH/Cargo.toml" ]; then
        MISSING_CRATES+=("$crate")
        print_warn "⚠️  Thiếu crate: $crate"
    else
        print_info "✅ Crate tồn tại: $crate"
    fi
done

if [ ${#MISSING_CRATES[@]} -eq 0 ]; then
    print_info "✅ Tất cả Move crates đã sẵn sàng!"
    exit 0
fi

print_step "Bước 3: Tải các Move crates còn thiếu..."

# If some crates are missing, try to get them
if [ ${#MISSING_CRATES[@]} -gt 0 ]; then
    print_warn "Một số crates còn thiếu. Đang thử tải lại..."
    
    cd "$SUI_DIR"
    
    # Try to checkout the specific directories
    for crate in "${MISSING_CRATES[@]}"; do
        print_info "Đang tải $crate..."
        CRATE_PATH="external-crates/move/crates/$crate"
        
        # Try git sparse checkout
        if [ -d ".git" ]; then
            echo "$CRATE_PATH/*" >> .git/info/sparse-checkout 2>/dev/null || true
            git read-tree -mu HEAD 2>/dev/null || true
        fi
    done
    
    # Final check
    print_step "Bước 4: Kiểm tra lại..."
    ALL_OK=true
    for crate in "${REQUIRED_CRATES[@]}"; do
        CRATE_PATH="$MOVE_CRATES_DIR/$crate"
        if [ ! -d "$CRATE_PATH" ] || [ ! -f "$CRATE_PATH/Cargo.toml" ]; then
            print_error "❌ Vẫn thiếu crate: $crate"
            ALL_OK=false
        else
            print_info "✅ Crate sẵn sàng: $crate"
        fi
    done
    
    if [ "$ALL_OK" = true ]; then
        print_info "✅ Tất cả Move crates đã sẵn sàng!"
    else
        print_error "❌ Một số Move crates vẫn còn thiếu"
        print_info "💡 Có thể cần clone toàn bộ Sui repository:"
        print_info "   cd $MYSTICETI_ROOT"
        print_info "   rm -rf sui"
        print_info "   git clone --depth=1 https://github.com/MystenLabs/sui.git sui"
        exit 1
    fi
fi

print_info "✅ Setup Move dependencies hoàn tất!"

