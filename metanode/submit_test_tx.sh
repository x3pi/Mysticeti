#!/bin/bash

# Submit test transaction using TESTING MODE
# Format: "TEST" + transaction_data

submit_test_transaction() {
    local node_id=$1
    local tx_data=$2
    local port=$((10100 + node_id))
    
    echo "Submitting test transaction to node $node_id (port $port)..."
    
    # Send "TEST" prefix + transaction data
    (echo -n "TEST"; echo -n "$tx_data") | nc -q 1 127.0.0.1 $port
    
    echo "Response from node $node_id:"
}

echo "Submitting test transactions to all nodes..."

# Submit to all nodes
for node_id in 0 1 2 3; do
    submit_test_transaction $node_id "test_tx_from_node_${node_id}_$(date +%s)"
    echo ""
done

echo "Waiting for consensus to process..."
sleep 5

echo "Check logs for consensus activity!"
