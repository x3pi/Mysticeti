#!/bin/bash

# Quick test script to verify Cross-Chain Gateway deployment
# This script only tests the deployment, not the full integration

set -e

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
BLUE='\033[0;34m'
NC='\033[0m'

print_info() { echo -e "${GREEN}ℹ️  $1${NC}"; }
print_warn() { echo -e "${YELLOW}⚠️  $1${NC}"; }
print_error() { echo -e "${RED}❌ $1${NC}"; }
print_step() { echo -e "${BLUE}📋 $1${NC}"; }

# Get script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

print_step "Testing Cross-Chain Gateway Deployment"
echo ""

# Check required files
print_info "Checking required files..."
REQUIRED_FILES=(
    "deployCrossChain.go"
    "deploy_crosschain.sh"
    "crossChainAbi.json"
    "byteCode/byteCode.json"
)

for file in "${REQUIRED_FILES[@]}"; do
    if [ -f "$file" ]; then
        echo "  ✅ $file"
    else
        print_error "$file not found!"
        exit 1
    fi
done

echo ""
print_info "Checking Go dependencies..."
if command -v go &> /dev/null; then
    echo "  ✅ Go is installed: $(go version)"
else
    print_error "Go is not installed!"
    exit 1
fi

echo ""
print_info "Building deployment tool..."
if go build -o deployCrossChain_test deployCrossChain.go; then
    echo "  ✅ Build successful"
    rm -f deployCrossChain_test
else
    print_error "Build failed!"
    exit 1
fi

echo ""
print_info "Checking .env configuration..."
if [ -f ".env.crosschain" ]; then
    echo "  ✅ .env.crosschain found"
    
    # Check for required variables
    source .env.crosschain
    
    if [ -n "$RPC_URL" ]; then
        echo "  ✅ RPC_URL is set: $RPC_URL"
    else
        print_warn "RPC_URL is not set"
    fi
    
    if [ -n "$PRIVATE_KEY" ]; then
        echo "  ✅ PRIVATE_KEY is set (hidden)"
    else
        print_warn "PRIVATE_KEY is not set"
    fi
    
    if [ -n "$SOURCE_NATION_ID" ]; then
        echo "  ✅ SOURCE_NATION_ID is set: $SOURCE_NATION_ID"
    else
        print_warn "SOURCE_NATION_ID is not set (will use default: 1)"
    fi
    
    if [ -n "$DEST_NATION_ID" ]; then
        echo "  ✅ DEST_NATION_ID is set: $DEST_NATION_ID"
    else
        print_warn "DEST_NATION_ID is not set (will use default: 2)"
    fi
else
    print_warn ".env.crosschain not found"
    if [ -f ".env" ]; then
        echo "  ℹ️  Will use .env as fallback"
    else
        print_error "No configuration file found!"
        exit 1
    fi
fi

echo ""
print_info "Checking bytecode format..."
if python3 -c "import json; data=json.load(open('byteCode/byteCode.json')); assert 'cross_chain' in data and len(data['cross_chain']) > 0" 2>/dev/null; then
    BYTECODE_LEN=$(python3 -c "import json; print(len(json.load(open('byteCode/byteCode.json'))['cross_chain']))")
    echo "  ✅ Bytecode valid (length: $BYTECODE_LEN bytes)"
else
    print_error "Invalid bytecode format!"
    exit 1
fi

echo ""
print_info "Checking ABI format..."
if python3 -c "import json; data=json.load(open('crossChainAbi.json')); assert isinstance(data, list) and len(data) > 0" 2>/dev/null; then
    ABI_ITEMS=$(python3 -c "import json; print(len(json.load(open('crossChainAbi.json'))))")
    echo "  ✅ ABI valid ($ABI_ITEMS items)"
else
    print_error "Invalid ABI format!"
    exit 1
fi

echo ""
print_step "✅ All checks passed!"
echo ""
print_info "You can now deploy the contract using:"
print_info "  ./deploy_crosschain.sh [source_nation_id] [dest_nation_id]"
echo ""
print_info "Or run the full system with auto-deployment:"
print_info "  cd ../.. && ./scripts/run_mixed_system.sh"
