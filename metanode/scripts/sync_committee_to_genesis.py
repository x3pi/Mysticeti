#!/usr/bin/env python3
"""
Script để sync committee từ Rust committee.json vào Go genesis.json
- Đọc committee từ committee_node_0.json (hoặc committee.json)
- Update genesis.json với validators từ committee
- Tạo delegator_stakes từ stake trong committee
"""

import json
import sys
import os
from typing import Dict, List, Any

def load_committee(committee_path: str) -> Dict[str, Any]:
    """Load committee từ Rust committee file"""
    with open(committee_path, 'r') as f:
        return json.load(f)

def load_genesis(genesis_path: str) -> Dict[str, Any]:
    """Load genesis.json từ Go project"""
    with open(genesis_path, 'r') as f:
        return json.load(f)

def save_genesis(genesis_path: str, genesis: Dict[str, Any]):
    """Save genesis.json"""
    with open(genesis_path, 'w') as f:
        json.dump(genesis, f, indent=2)

def extract_committee_data(committee: Dict[str, Any]) -> List[Dict[str, Any]]:
    """Extract committee data - hỗ trợ cả CommitteeConfig và Committee format"""
    # Check if it's CommitteeConfig format (có committee field)
    if 'committee' in committee:
        authorities = committee['committee'].get('authorities', [])
    else:
        # Plain Committee format
        authorities = committee.get('authorities', [])
    
    return authorities

def sync_committee_to_genesis(committee_path: str, genesis_path: str):
    """Sync committee vào genesis.json"""
    print(f"📝 Loading committee from: {committee_path}")
    committee = load_committee(committee_path)
    
    print(f"📝 Loading genesis.json from: {genesis_path}")
    genesis = load_genesis(genesis_path)
    
    # Extract authorities from committee
    authorities = extract_committee_data(committee)
    print(f"✅ Found {len(authorities)} validators in committee")
    
    if not authorities:
        print("❌ Error: No validators found in committee!")
        sys.exit(1)
    
    # Initialize validators array if not exists
    if 'validators' not in genesis:
        genesis['validators'] = []
    
    # Clear existing validators (hoặc merge nếu cần)
    genesis['validators'] = []
    
    # Convert each authority to validator format
    for idx, authority in enumerate(authorities):
        # Extract keys (có thể là base64 hoặc hex)
        authority_key = authority.get('authority_key', '')
        protocol_key = authority.get('protocol_key', '')
        network_key = authority.get('network_key', '')
        address = authority.get('address', '')
        hostname = authority.get('hostname', f'node-{idx}')
        stake = authority.get('stake', 1)
        
        # Convert stake từ normalized (1) về wei (10^18)
        # stake trong committee là normalized (1 = 1 token)
        # genesis.json cần wei (1 token = 10^18 wei)
        stake_wei = stake * 1_000_000_000_000_000_000  # 10^18
        
        # Generate Ethereum address từ authority_key (hoặc dùng address có sẵn)
        # Nếu address là Multiaddr format, cần convert
        eth_address = None
        if address.startswith('/ip4/'):
            # Multiaddr format - generate deterministic address từ hostname
            import hashlib
            hash_obj = hashlib.sha256(f"{hostname}-{authority_key}".encode())
            eth_address = "0x" + hash_obj.hexdigest()[:40]
        elif address.startswith('0x'):
            eth_address = address
        else:
            # Generate từ hostname
            import hashlib
            hash_obj = hashlib.sha256(f"{hostname}-{authority_key}".encode())
            eth_address = "0x" + hash_obj.hexdigest()[:40]
        
        # Extract port từ Multiaddr address
        port = 9000 + idx
        if address.startswith('/ip4/'):
            try:
                # Parse /ip4/127.0.0.1/tcp/9000
                parts = address.split('/')
                for i, part in enumerate(parts):
                    if part == 'tcp' and i + 1 < len(parts):
                        port = int(parts[i + 1])
                        break
            except:
                port = 9000 + idx
        
        # Create validator entry
        validator = {
            "address": eth_address,
            "primary_address": f"127.0.0.1:{4000 + idx * 100}",
            "worker_address": f"127.0.0.1:{4012 + idx * 100}",
            "p2p_address": address if address.startswith('/ip4/') else f"/ip4/127.0.0.1/tcp/{port}",
            "description": f"Validator {hostname} from committee",
            "website": f"https://validator-{idx}.com",
            "image": f"https://example.com/validator-{idx}.png",
            "commission_rate": 5,
            "min_self_delegation": "1000000000000000000",
            "accumulated_rewards_per_share": "0",
            "delegator_stakes": [
                {
                    "address": eth_address,
                    "amount": str(stake_wei)
                }
            ],
            "total_staked_amount": str(stake_wei),
            "network_key": network_key,
            "hostname": hostname,
            "authority_key": authority_key,
            "protocol_key": protocol_key
        }
        
        genesis['validators'].append(validator)
        print(f"  ✅ Added validator {idx}: {hostname} (address: {eth_address}, stake: {stake} -> {stake_wei} wei)")
    
    # Save updated genesis.json
    print(f"💾 Saving updated genesis.json to: {genesis_path}")
    save_genesis(genesis_path, genesis)
    
    print(f"✅ Successfully synced {len(genesis['validators'])} validators to genesis.json")
    print(f"   💡 Go Master sẽ init genesis với validators này khi khởi động")

if __name__ == "__main__":
    if len(sys.argv) != 3:
        print("Usage: python3 sync_committee_to_genesis.py <committee.json> <genesis.json>")
        sys.exit(1)
    
    committee_path = sys.argv[1]
    genesis_path = sys.argv[2]
    
    if not os.path.exists(committee_path):
        print(f"❌ Error: Committee file not found: {committee_path}")
        sys.exit(1)
    
    if not os.path.exists(genesis_path):
        print(f"❌ Error: Genesis file not found: {genesis_path}")
        sys.exit(1)
    
    try:
        sync_committee_to_genesis(committee_path, genesis_path)
    except Exception as e:
        print(f"❌ Error: {e}")
        import traceback
        traceback.print_exc()
        sys.exit(1)

