#!/bin/bash
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
echo "🚀 Running System Reset (Keeping Keys + Data Cleanup)..."
export KEEP_KEYS=1
"$SCRIPT_DIR/run_mixed_system.sh"
