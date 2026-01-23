#!/bin/bash

# Script to generate genesis.json from Rust-generated keys
# Usage: ./generate_genesis_from_rust_keys.sh <rust_config_dir> <output_genesis.json>

set -e

if [ $# -ne 2 ]; then
    echo "Usage: $0 <rust_config_dir> <output_genesis.json>"
    exit 1
fi

RUST_CONFIG_DIR="$1"
OUTPUT_FILE="$2"

echo "Generating genesis.json from Rust keys in: $RUST_CONFIG_DIR"
echo "Output: $OUTPUT_FILE"

# Function to extract key from file
extract_key() {
    local file="$1"
    if [ -f "$file" ]; then
        cat "$file" | tr -d '\n'
    else
        echo "ERROR: Key file not found: $file" >&2
        return 1
    fi
}

# Function to generate validator address from authority key (placeholder - using fixed addresses for now)
generate_address() {
    local node_id="$1"
    case $node_id in
        0) echo "0x0193d8b9f995f13d448da3bc6bee3700056573d4" ;;
        1) echo "0xada389a462e9d4e65c836299c96b85ed0f3f9913" ;;
        2) echo "0x1234567890abcdef1234567890abcdef12345678" ;;
        3) echo "0xabcdef1234567890abcdef1234567890abcdef12" ;;
    esac
}

# Function to generate a random BLS key for authority key


# Get current timestamp in milliseconds for epoch_timestamp_ms
CURRENT_TIMESTAMP_MS=$(python3 -c "import time; print(int(time.time() * 1000))" 2>/dev/null || echo "$(date +%s)000")

# Start genesis.json with current epoch_timestamp_ms
cat > "$OUTPUT_FILE" << EOF
{
  "config": {
    "chainId": 991,
    "epoch": 0,
    "epoch_timestamp_ms": ${CURRENT_TIMESTAMP_MS}
  },
  "validators": [
EOF

# Generate validator entries for nodes 0-3
for i in 0 1 2 3; do
    echo "Processing node $i..."

    # Read keys
    PROTOCOL_KEY_FILE="$RUST_CONFIG_DIR/node_${i}_protocol_key.json"
    NETWORK_KEY_FILE="$RUST_CONFIG_DIR/node_${i}_network_key.json"

    # Read Authority Key from committee.json (using Python for reliability)
    COMMITTEE_FILE="$RUST_CONFIG_DIR/committee.json"
    if [ ! -f "$COMMITTEE_FILE" ]; then
        echo "ERROR: committee.json not found in $RUST_CONFIG_DIR" >&2
        exit 1
    fi

    # Extract authority key for node i
    AUTHORITY_KEY=$(python3 -c "import json, sys; 
try:
    with open('$COMMITTEE_FILE') as f:
        data = json.load(f)
        # Find authority with specific hostname or index? 
        # Usually authorities array index matches node_id, but let's be safe if possible.
        # Assuming index matches because that's how it's generated.
        if $i < len(data['authorities']):
            print(data['authorities'][$i]['authority_key'])
        else:
            sys.exit(1)
except Exception as e:
    sys.exit(1)" 2>/dev/null)

    PROTOCOL_KEY=$(extract_key "$PROTOCOL_KEY_FILE")
    NETWORK_KEY=$(extract_key "$NETWORK_KEY_FILE")

    if [ -z "$AUTHORITY_KEY" ] || [ -z "$PROTOCOL_KEY" ] || [ -z "$NETWORK_KEY" ]; then
        echo "ERROR: Missing keys for node $i" >&2
        exit 1
    fi

    # Generate address
    ADDRESS=$(generate_address $i)

    # Add validator entry
    cat >> "$OUTPUT_FILE" << EOF
    {
      "address": "${ADDRESS}",
      "primary_address": "127.0.0.1:$((4000 + i * 100))",
      "worker_address": "127.0.0.1:$((4012 + i * 100))",
      "p2p_address": "/ip4/127.0.0.1/tcp/$((9000 + i))",
      "description": "Validator node-${i} from committee",
      "website": "https://validator-${i}.com",
      "image": "https://example.com/validator-${i}.png",
      "commission_rate": 5,
      "min_self_delegation": "1000000000000000000",
      "accumulated_rewards_per_share": "0",
      "delegator_stakes": [
        {
          "address": "${ADDRESS}",
          "amount": "1000000000000000000"
        }
      ],
      "total_staked_amount": "1000000000000000000",
      "network_key": "${NETWORK_KEY}",
      "hostname": "node-${i}",
      "authority_key": "${AUTHORITY_KEY}",
      "protocol_key": "${PROTOCOL_KEY}"
    }
EOF

    # Add comma for all but last entry
    if [ $i -lt 3 ]; then
        echo "," >> "$OUTPUT_FILE"
    fi
done

# Close validators array and file
cat >> "$OUTPUT_FILE" << 'EOF'
  ]
}
EOF

echo "✅ Genesis.json generated successfully: $OUTPUT_FILE"
echo "📊 Generated $(grep -c '"address"' "$OUTPUT_FILE") validators"
