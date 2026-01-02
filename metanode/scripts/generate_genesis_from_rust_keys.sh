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
generate_bls_authority_key() {
    local node_id="$1"
    # Generate deterministic BLS keys based on node_id for testing
    case $node_id in
        0) echo "o2/6dk0VXG9p8u1y1wwlFCUrskqxhpRCtRffBN6hPgyGSDYg11cf9k9G28zF8+rhEvyrF1zx1lwbdkCNohtsGSZkmDrfqdpoHiRIJl2C2qOFTHX0FV/UOaB6NweAGdzl" ;;
        1) echo "sC5pYcq+J6QD4Oeb/SgxEWgmmw69VH2t1MjDe0Gwc8cCCzmp7le4GtqD3W9ow+pIDGLcVYqDlo9PeifTMNaHv41TeWLjsO8kEHNg/R3sXkEAVEKbGQJ0dn5r5xXwvftg" ;;
        2) echo "qguXR7WYJyn+pXssbkMVgv24XmDjqrTb/0Mr10WNH0Iot+k0fhK91MY2ddvBA7RgEMNKLy3au7ymM/Ycixl6Nk8waQrGIR8LR3pHeHjB5U0QslzjZD/fImd6ZqBrNTGR" ;;
        3) echo "rrLpSlaNHvS7B4Fka3ym6n8oSR+Qnz4jE7DQjdVlAonIfO+Gu8G6bId8mkCWvfYGA+K2NsSs2wcxG6rdqVYAIQlVN1usAEY12G3VN2SZ9APnVhU3NPqyJG1IyrGbfjRI" ;;
    esac
}

# Start genesis.json
cat > "$OUTPUT_FILE" << 'EOF'
{
  "config": {
    "chainId": 991,
    "epoch": 0
  },
  "validators": [
EOF

# Generate validator entries for nodes 0-3
for i in 0 1 2 3; do
    echo "Processing node $i..."

    # Read keys
    PROTOCOL_KEY_FILE="$RUST_CONFIG_DIR/node_${i}_protocol_key.json"
    NETWORK_KEY_FILE="$RUST_CONFIG_DIR/node_${i}_network_key.json"

    # Generate BLS authority key (deterministic based on node_id)
    AUTHORITY_KEY=$(generate_bls_authority_key $i)

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
