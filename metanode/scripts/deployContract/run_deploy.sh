#!/bin/bash

# Quick deploy and interact with Cross-Chain Gateway
# This script builds and runs the deployment with interactive menu

set -e

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

print_info() { echo -e "${GREEN}ℹ️  $1${NC}"; }
print_step() { echo -e "${BLUE}📋 $1${NC}"; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

print_step "Building deployCrossChain..."
go build -o deployCrossChain deployCrossChain.go

print_step "Running Cross-Chain Gateway Deployment & Menu..."
echo ""
./deployCrossChain

# Cleanup
rm -f deployCrossChain
