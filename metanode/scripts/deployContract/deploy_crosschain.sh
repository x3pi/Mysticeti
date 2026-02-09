#!/bin/bash

# Script to deploy Cross-Chain Gateway contract
# Usage: ./deploy_crosschain.sh [source_nation_id] [dest_nation_id]

set -e

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

print_info() { echo -e "${GREEN}ℹ️  $1${NC}"; }
print_warn() { echo -e "${YELLOW}⚠️  $1${NC}"; }
print_error() { echo -e "${RED}❌ $1${NC}"; }

# Get script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Parse arguments
SOURCE_NATION_ID="${1:-1}"
DEST_NATION_ID="${2:-2}"

# Load environment variables from .env.crosschain
if [ -f ".env.crosschain" ]; then
    print_info "Loading configuration from .env.crosschain"
    export $(cat .env.crosschain | grep -v '^#' | xargs)
else
    print_warn ".env.crosschain not found, using default .env"
    if [ -f ".env" ]; then
        export $(cat .env | grep -v '^#' | xargs)
    fi
fi

# Override with command line arguments
export SOURCE_NATION_ID="$SOURCE_NATION_ID"
export DEST_NATION_ID="$DEST_NATION_ID"

print_info "🚀 Deploying Cross-Chain Gateway Contract..."
print_info "   Source Nation ID: $SOURCE_NATION_ID"
print_info "   Dest Nation ID: $DEST_NATION_ID"
print_info "   RPC URL: ${RPC_URL:-http://192.168.1.234:8545}"
echo ""

# Build and run the deployment
print_info "📦 Building deployment tool..."
go build -o deployCrossChain deployCrossChain.go

print_info "🔨 Deploying contract..."
./deployCrossChain

# Clean up binary
rm -f deployCrossChain

print_info "✅ Deployment completed!"
