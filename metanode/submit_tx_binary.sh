#!/bin/bash

# Submit transaction using binary protocol
# Format: [4-byte big-endian length][transaction data]

submit_binary_transaction() {
    local node_id=$1
    local tx_data=$2
    local port=$((10100 + node_id))
    
    echo "Submitting transaction to node $node_id (port $port)..."
    
    # Convert transaction data to bytes
    local tx_bytes=$(echo -n "$tx_data" | wc -c)
    local tx_hex=$(echo -n "$tx_data" | xxd -p | tr -d '\n')
    
    # Create length prefix (4 bytes, big-endian)
    local len_hex=$(printf '%08x' $tx_bytes | sed 's/\(..\)/\1 /g' | tac | tr -d ' ')
    
    # Combine length + data
    local full_hex="${len_hex}${tx_hex}"
    
    # Convert hex to binary and send via nc
    echo -n "$full_hex" | xxd -r -p | nc -q 1 127.0.0.1 $port
    
    if [ $? -eq 0 ]; then
        echo "✅ Transaction submitted to node $node_id"
    else
        echo "❌ Failed to submit transaction to node $node_id"
    fi
}

# Test transactions
echo "Submitting test transactions..."

# Submit to node 0 (the one with executor)
submit_binary_transaction 0 "test_transaction_0_$(date +%s)"

# Submit to other nodes  
submit_binary_transaction 1 "test_transaction_1_$(date +%s)"
submit_binary_transaction 2 "test_transaction_2_$(date +%s)"
submit_binary_transaction 3 "test_transaction_3_$(date +%s)"

echo "Waiting for consensus to process..."
sleep 5

echo "Check logs for consensus activity!"
